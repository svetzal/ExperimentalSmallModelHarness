"""The calculate tool answers batches, reports errors, and stays out of benchmarks. No model."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import threading

binary, base = str(Path(sys.argv[1]).resolve()), Path(sys.argv[2])


def serve(provider, calls):
    """A mock gateway: each request answers with the next scripted turn."""
    requests = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
            text, call = calls[min(len(requests), len(calls)) - 1]
            if provider == "omlx":
                delta = dict(content=text)
                if call:
                    delta["tool_calls"] = [dict(index=0, id=f"calc-{len(requests)}", type="function",
                                                function=dict(name=call[0], arguments=json.dumps(call[1])))]
                body = ("data: " + json.dumps(dict(choices=[dict(delta=delta, finish_reason="tool_calls" if call else "stop")]))
                        + "\n\ndata: [DONE]\n\n")
                kind = "text/event-stream"
            else:
                message = dict(role="assistant", content=text)
                if call:
                    message["tool_calls"] = [dict(function=dict(name=call[0], arguments=call[1]))]
                body = (json.dumps(dict(message=message, done=False)) + "\n"
                        + json.dumps(dict(message=dict(role="assistant", content=""), done=True, done_reason="stop")) + "\n")
                kind = "application/json"
            self.send_response(200)
            self.send_header("Content-Type", kind)
            self.send_header("Content-Length", str(len(body.encode())))
            self.end_headers()
            self.wfile.write(body.encode())

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, requests


def tool_names(request):
    return [tool["function"]["name"] for tool in request.get("tools", [])]


def tool_message(request):
    return next(m for m in reversed(request["messages"]) if m["role"] == "tool")["content"]


for provider in ("omlx", "ollama"):
    root = base / provider / "project"
    root.mkdir(parents=True)
    env_base = dict(os.environ, XDG_STATE_HOME=str(base / provider / "state"),
                    XDG_CONFIG_HOME=str(base / provider / "config"), OMLX_API_KEY="test-key")

    # An everyday run offers calculate; a batch comes back one line each.
    server, requests = serve(provider, [
        ("", ("calculate", {"expression": "140+90+70+120+350+60+60+154+60+280+90\n1474/1.1"})),
        ("", ("calculate", {"expression": "1354 / (1.1"})),
        ("The total is 1474 ms and the new length is 1340 ms.", None),
    ])
    host = f"http://127.0.0.1:{server.server_port}"
    try:
        output = subprocess.run(
            [binary, "run", "--prompt", "Add up the stage holds, then divide by 1.1.", "--json",
             "--provider", provider, "--model", "test-model"],
            cwd=root, stdin=subprocess.DEVNULL, capture_output=True, timeout=60,
            env=dict(env_base, OMLX_HOST=host, OLLAMA_HOST=host))
        assert output.returncode == 0, output.stderr
        assert "calculate" in tool_names(requests[0]), tool_names(requests[0])
        batch = json.loads(tool_message(requests[1]))
        assert batch == {"results": [
            "140 + 90 + 70 + 120 + 350 + 60 + 60 + 154 + 60 + 280 + 90 = 1474",
            "1474 / 1.1 = 1340",
        ]}, batch
        error = json.loads(tool_message(requests[2]))
        assert "error at column" in error["error"] and "never closed" in error["error"], error
        trace = json.loads(output.stdout)["trace_file"]
        results = [json.loads(line)["payload"] for line in Path(trace).read_text().splitlines()
                   if json.loads(line)["kind"] == "llm.context_assembly.appended"
                   and json.loads(line)["payload"].get("component") == "tool_result"]
        assert [r["ok"] for r in results] == [True, False], results
    finally:
        server.shutdown()
        server.server_close()

    # Benchmark runs keep their exact tool set.
    contract = base / provider / "contract.json"
    contract.write_text(json.dumps(dict(profile=dict(id="terminal_work", version="terminal_work_profile.v1"),
                                        guidance="Finish.", read_scope=["./**"], write_scope=["./**"], probes=[])))
    server, requests = serve(provider, [("DONE", None)])
    host = f"http://127.0.0.1:{server.server_port}"
    try:
        output = subprocess.run(
            [binary, "benchmark", "run", "--experiment", str(base / provider), "--contract", str(contract),
             "--trace-dir", str(base / provider / "bench-traces"), "--provider", provider, "--dir", str(root),
             "--max-model-interactions", "2", "--max-iterations", "1"],
            cwd=root, stdin=subprocess.DEVNULL, capture_output=True, timeout=60,
            env=dict(env_base, OMLX_HOST=host, OLLAMA_HOST=host))
        assert output.returncode == 0, output.stderr
        assert requests and "calculate" not in tool_names(requests[0]), tool_names(requests[0])
    finally:
        server.shutdown()
        server.server_close()
