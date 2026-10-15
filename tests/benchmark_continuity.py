"""Compare benchmark provider packets against a preserved pre-change binary."""
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time

baseline, current = [Path(path).resolve() for path in sys.argv[1:3]]
benchmark_mode = len(sys.argv) > 3 and sys.argv[3] == "benchmark"
results = []
with tempfile.TemporaryDirectory(prefix="microgentic-continuity-") as temporary:
    base = Path(temporary)
    root = base / "project"
    root.mkdir()
    # Project guidance must never reach benchmark or contract runs.
    (root / "AGENTS.md").write_text("PROJECT-GUIDANCE-SENTINEL: benchmark runs must never see this.\n")
    contract = base / "contract.json"
    prompt = "Inspect the project with list_tree, then finish. Unicode 世界."
    contract.write_text(json.dumps(dict(profile=dict(id="terminal_work", version="terminal_work_profile.v1"),
                                        guidance=prompt, read_scope=["./**"], write_scope=["./**"], probes=[])))
    config = base / "personal-config.json"
    config.write_text("invalid personal config: must never be read by benchmarks")
    (base / "task.md").write_text(prompt)
    (base / "expected.txt").write_text("expected artifact\n")
    cases = ("contract", "goal", "sequential") if benchmark_mode else ("contract",)
    for case in cases:
        for provider in ("omlx", "ollama"):
            captured = []
            for name, binary in (("baseline", baseline), ("current", current)):
                requests = []

                class Handler(http.server.BaseHTTPRequestHandler):
                    def log_message(self, *_args):
                        pass

                    def do_POST(self):
                        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                        requests.append(request)
                        first = len(requests) % 2 == 1
                        if case == "sequential" and first:
                            # Historical trace filenames have one-second precision.
                            # Pace mock attempts so their trace names stay distinct.
                            time.sleep(1.05)
                        if provider == "omlx":
                            delta = dict(content="Inspecting." if first else "DONE")
                            if first:
                                delta["tool_calls"] = [dict(index=0, id="inspection", type="function",
                                                            function=dict(name="list_tree", arguments="{}"))]
                            body = "data: " + json.dumps(dict(choices=[dict(delta=delta, finish_reason="tool_calls" if first else "stop")])) + "\n\ndata: [DONE]\n\n"
                        else:
                            message = dict(role="assistant", content="Inspecting." if first else "DONE")
                            if first:
                                message["tool_calls"] = [dict(function=dict(name="list_tree", arguments={}))]
                            body = json.dumps(dict(message=message, done=False)) + "\n" + json.dumps(dict(message=dict(role="assistant", content=""), done=True, done_reason="stop")) + "\n"
                        self.send_response(200)
                        self.send_header("Content-Type", "text/event-stream" if provider == "omlx" else "application/json")
                        self.send_header("Content-Length", str(len(body.encode())))
                        self.end_headers()
                        self.wfile.write(body.encode())

                server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                thread = threading.Thread(target=server.serve_forever, daemon=True)
                thread.start()
                host = f"http://127.0.0.1:{server.server_port}"
                trace_dir = base / f"{case}-{provider}-{name}"
                env = dict(os.environ, OMLX_HOST=host, OLLAMA_HOST=host, OMLX_API_KEY="test-key")
                try:
                    controls = ["--experiment", str(base),
                                "--trace-dir", str(trace_dir), "--provider", provider,
                                "--reasoning-effort", "low", "--max-tokens", "2048",
                                "--max-model-interactions", "4", "--max-iterations", "3"]
                    if case != "goal":
                        controls += ["--contract", str(contract)]
                    verb = "run-sequential" if case == "sequential" else "run"
                    if case == "sequential":
                        controls += ["--artifact", "answer.txt", "--expected-artifact", "expected.txt",
                                     "--max-attempts", "2", "--max-consecutive-unchanged-attempts", "2"]
                    if benchmark_mode:
                        # Both historical file inputs may be supplied; the contract wins.
                        if name == "current" and case != "goal":
                            controls += ["--goal", "missing-unused-goal.md"]
                        command = [str(binary), verb, *controls, *(["--json"] if baseline == current else [])] if name == "baseline" else [
                            str(binary), "--config", str(config), "benchmark", verb, *controls,
                            "--dir", str(root)]
                        env.update(XDG_CONFIG_HOME=str(base / "config"), XDG_STATE_HOME=str(base / "state"))
                    else:
                        command = [str(binary), "run", *controls, "--prompt", prompt, "--json"]
                    output = subprocess.run(command, cwd=root, input=b"ignore this ambient stdin" if benchmark_mode and name == "current" else b"",
                                            capture_output=True, env=env, timeout=30)
                    assert output.returncode == 0, output.stderr
                    summary = json.loads(output.stdout)
                    if benchmark_mode:
                        assert not (base / "state").exists(), "benchmark created personal run state"
                    trace_files = [a["trace_file"] for a in summary["attempts"]] if case == "sequential" else [summary["trace_file"]]
                    records = [json.loads(line) for file in trace_files for line in Path(file).read_text().splitlines()]
                    packets = [r["payload"] for r in records if r["kind"] == "llm.provider_request.assembled"]
                    ledgers = [r["payload"] for r in records if r["kind"] == "llm.context_assembly.ledger"]
                    assert not any(r["kind"].startswith("session.") for r in records)
                    expected_requests = 4 if case == "sequential" else 2
                    assert len(requests) == len(packets) == expected_requests
                    if case == "sequential":
                        assert requests[:2] == requests[2:], "context leaked between task attempts"
                        assert summary["stop_reason"] == "consecutive_unchanged_attempts"
                        assert summary["confirmed_exact_success"] is False
                    assert "PROJECT-GUIDANCE-SENTINEL" not in json.dumps(requests), "project guidance reached a benchmark request"
                    captured.append((requests, packets, ledgers, summary))
                finally:
                    server.shutdown()
                    server.server_close()
            assert captured[0][:3] == captured[1][:3], f"{provider}: provider packets or ledgers changed"
            assert captured[0][3].keys() == captured[1][3].keys(), "summary schema changed"
            if case == "sequential":
                for left, right in zip(captured[0][3]["attempts"], captured[1][3]["attempts"]):
                    for key in ("final_summary", "accepted_done", "independent_exact", "terminal_error",
                                "pre_artifact", "post_artifact", "artifact_unchanged"):
                        assert left[key] == right[key], f"sequential attempt {key} changed"
            else:
                assert captured[0][3]["final_summary"] == captured[1][3]["final_summary"]
            results.append(dict(case=case, provider=provider, requests_compared=expected_requests, provider_json_equal=True,
                                exact_snapshots_equal=True, context_ledgers_equal=True, summary_schema_equal=True))
print(json.dumps(dict(schema_version="microgentic_benchmark_mode_check.v1" if benchmark_mode else "microgentic_conversation_isolation_check.v1",
                      baseline_sha256=hashlib.sha256(baseline.read_bytes()).hexdigest(),
                      current_sha256=hashlib.sha256(current.read_bytes()).hexdigest(), results=results), indent=2))
