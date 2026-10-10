#!/usr/bin/env python3
"""Record worker progress prerequisites using only scripted loopback providers.

This is a baseline diagnostic, not the recovery conformance matrix.
"""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import subprocess
import threading
import uuid


def encoded(value):
    return json.dumps(value, separators=(",", ":")).encode()


def run_case(worker, artifacts, provider):
    root = artifacts / provider
    root.mkdir(parents=True, exist_ok=False)
    workspace = root / "workspace"
    workspace.mkdir()
    (root / "task.md").write_text("Finish with DONE.\n")
    secret = str(uuid.uuid4())  # Never persist headers, environment, or this value.
    requests = []
    failures = []
    if provider == "ollama":
        frames = [
            {"message": {}, "done": False},
            {"message": {"thinking": "Synthetic reasoning."}, "done": False},
            {"message": {"content": "DONE"}, "done": False},
            {"message": {}, "done": True, "done_reason": "stop",
             "prompt_eval_count": 11, "eval_count": 7,
             "total_duration": 2000000000, "load_duration": 100000000,
             "prompt_eval_duration": 400000000, "eval_duration": 1000000000},
        ]
        body = b"\n".join(map(encoded, frames)) + b"\n"
        route = "/api/chat"
        content_type = "application/x-ndjson"
    else:
        frames = [
            {"model": "keepalive"},
            {"choices": [{"delta": {"reasoning_content": "Synthetic reasoning."},
                          "finish_reason": None}]},
            {"choices": [{"delta": {"content": "DONE"}, "finish_reason": "stop"}]},
        ]
        body = b"".join(b"data: " + encoded(frame) + b"\n\n" for frame in frames)
        body += b"data: [DONE]\n\n"
        route = "/v1/chat/completions"
        content_type = "text/event-stream"
    (root / "response.body").write_bytes(body)

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                assert self.path == route, "unexpected provider route"
                if provider == "omlx":
                    assert self.headers.get("Authorization") == "Bearer " + secret
                request = self.rfile.read(int(self.headers["Content-Length"]))
                requests.append(request)
                (root / f"request-{len(requests)}.body").write_bytes(request)
                self.send_response(200)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            except Exception as error:
                failures.append(type(error).__name__)  # Never serialize unsafe causes.
                self.close_connection = True

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    # A clean environment also removes provider keys, proxies and logging overrides.
    environment = {key: os.environ[key] for key in ("PATH", "HOME", "TMPDIR")
                   if key in os.environ}
    environment[provider.upper() + "_HOST"] = f"http://127.0.0.1:{server.server_port}"
    if provider == "omlx":
        environment["OMLX_API_KEY"] = secret
    try:
        completed = subprocess.run(
            [str(worker), "run", "--experiment", str(root), "--provider", provider,
             "--model", "synthetic-model", "--max-iterations", "1",
             "--max-model-interactions", "1", "--reasoning-effort", "enabled",
             "--preserve-reasoning", "--max-tokens", "8192"],
            cwd=workspace, env=environment, capture_output=True, timeout=30,
        )
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
    assert not failures, f"scripted server failed: {failures}"
    assert completed.returncode == 0, "worker did not complete the synthetic request"
    assert len(requests) == 1, "baseline must make exactly one wire request"
    summary = json.loads(completed.stdout)
    assert summary["final_summary"].strip() == "DONE"
    traces = list((root / "traces").glob("run-*.jsonl"))
    assert len(traces) == 1
    trace = traces[0].read_bytes()
    for payload in [completed.stdout, completed.stderr, trace, body, *requests]:
        assert secret.encode() not in payload, "credential found in an artifact"
    (root / "stdout.json").write_bytes(completed.stdout)
    (root / "stderr.log").write_bytes(completed.stderr)
    events = [json.loads(line) for line in trace.splitlines()]
    kinds = [event["kind"] for event in events]
    assert "llm.stream.thinking" in kinds, "native reasoning was lost"
    request = json.loads(requests[0])
    assert request["stream"] is True
    assert request["tools"], "worker tools must reach the provider"
    assert summary["max_model_interactions"] == 1
    if provider == "ollama":
        assert request["think"] is True
        assert request["options"]["num_predict"] == 8192
        progress = [event["payload"] for event in events
                    if event["kind"] == "llm.stream.progress"]
        assert len(progress) == 4, "worker must observe every provider frame"
        assert progress[0]["content_chars"] == 0
        assert progress[0]["thinking_chars"] == 0
        assert progress[0]["tool_call_count"] == 0
        metrics = [event["payload"] for event in events
                   if event["kind"] == "llm.stream.metrics"]
        assert len(metrics) == 1
        assert metrics[0]["eval_count"] == 7
        assert metrics[0]["prompt_eval_count"] == 11
        assert metrics[0]["tokens_per_second"] == 7.0
    else:
        assert request["max_tokens"] == 8192
        assert request["enable_thinking"] is True
        assert request["chat_template_kwargs"]["preserve_thinking"] is True
    assertions = {
        "provider": provider, "wire_requests": len(requests),
        "native_reasoning": True, "worker_tools": True,
        "ollama_frame_progress_and_metrics": provider == "ollama",
        "credential_absent_from_artifacts": True,
        "recovery_exercised": False,
        "request_bodies": [str(root / "request-1.body")],
        "response_body": str(root / "response.body"), "trace": str(traces[0]),
        "stdout": str(root / "stdout.json"), "stderr": str(root / "stderr.log"),
    }
    (root / "assertions.json").write_text(json.dumps(assertions, indent=2) + "\n")
    print(json.dumps(assertions))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker", required=True, type=Path,
                        help="Exact frozen Linux worker executable")
    parser.add_argument("--artifacts", required=True, type=Path,
                        help="New fixture output directory, outside historical experiments")
    args = parser.parse_args()
    worker = args.worker.resolve(strict=True)
    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=False)
    identity = {"worker": str(worker),
                "sha256": hashlib.sha256(worker.read_bytes()).hexdigest()}
    (artifacts / "worker.json").write_text(json.dumps(identity, indent=2) + "\n")
    for provider in ("ollama", "omlx"):
        run_case(worker, artifacts, provider)


if __name__ == "__main__":
    main()
