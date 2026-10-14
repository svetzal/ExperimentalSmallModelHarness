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

baseline, current = [Path(path).resolve() for path in sys.argv[1:3]]
results = []
with tempfile.TemporaryDirectory(prefix="microgentic-continuity-") as temporary:
    base = Path(temporary)
    root = base / "project"
    root.mkdir()
    contract = base / "contract.json"
    prompt = "Inspect the project with list_tree, then finish. Unicode 世界."
    contract.write_text(json.dumps(dict(profile=dict(id="terminal_work", version="terminal_work_profile.v1"),
                                        guidance=prompt, read_scope=["./**"], write_scope=["./**"], probes=[])))
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
                    first = len(requests) == 1
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
            trace_dir = base / f"{provider}-{name}"
            env = dict(os.environ, OMLX_HOST=host, OLLAMA_HOST=host, OMLX_API_KEY="test-key")
            try:
                output = subprocess.run([str(binary), "run", "--experiment", str(base), "--contract", str(contract),
                                         "--prompt", prompt, "--json", "--trace-dir", str(trace_dir),
                                         "--provider", provider, "--reasoning-effort", "low", "--max-tokens", "2048",
                                         "--max-model-interactions", "4", "--max-iterations", "3"],
                                        cwd=root, stdin=subprocess.DEVNULL, capture_output=True, env=env, timeout=30)
                assert output.returncode == 0, output.stderr
                summary = json.loads(output.stdout)
                records = [json.loads(line) for line in Path(summary["trace_file"]).read_text().splitlines()]
                packets = [r["payload"] for r in records if r["kind"] == "llm.provider_request.assembled"]
                ledgers = [r["payload"] for r in records if r["kind"] == "llm.context_assembly.ledger"]
                assert not any(r["kind"].startswith("session.") for r in records)
                assert len(requests) == len(packets) == 2
                captured.append((requests, packets, ledgers, summary))
            finally:
                server.shutdown()
                server.server_close()
        assert captured[0][:3] == captured[1][:3], f"{provider}: provider packets or ledgers changed"
        assert captured[0][3].keys() == captured[1][3].keys(), "summary schema changed"
        assert captured[0][3]["final_summary"] == captured[1][3]["final_summary"]
        results.append(dict(provider=provider, requests_compared=2, provider_json_equal=True,
                            exact_snapshots_equal=True, context_ledgers_equal=True, summary_schema_equal=True))

print(json.dumps(dict(schema_version="microgentic_conversation_isolation_check.v1",
                      baseline_sha256=hashlib.sha256(baseline.read_bytes()).hexdigest(),
                      current_sha256=hashlib.sha256(current.read_bytes()).hexdigest(), results=results), indent=2))
