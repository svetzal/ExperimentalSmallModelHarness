"""Project guidance reaches everyday runs and conversations, framed and traced. No model."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import threading

binary, base = str(Path(sys.argv[1]).resolve()), Path(sys.argv[2])


def serve(provider, turns):
    requests = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
            text, call = turns[min(len(requests), len(turns)) - 1]
            if provider == "omlx":
                delta = dict(content=text)
                if call:
                    delta["tool_calls"] = [dict(index=0, id=f"c{len(requests)}", type="function",
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


def system(request):
    return request["messages"][0]["content"]


def users(request):
    return [m["content"] for m in request["messages"] if m["role"] == "user"]


for provider in ("omlx", "ollama"):
    work = base / provider
    root = work / "repo"
    (root / ".git").mkdir(parents=True)
    (root / "AGENTS.md").write_text("ROOT-RULE: run tests with make check.\n")
    (root / "sub").mkdir()
    (root / "sub" / "AGENTS.md").write_text("SUB-RULE: in sub, answer in French.\n")
    (root / "sub" / "notes.txt").write_text("notes\n")
    config_home = work / "config"
    (config_home / "microgentic").mkdir(parents=True)
    (config_home / "microgentic" / "AGENTS.md").write_text("USER-RULE: be brief.\n")
    env = dict(os.environ, XDG_STATE_HOME=str(work / "state"), XDG_CONFIG_HOME=str(config_home),
               OMLX_API_KEY="test-key")

    def run(*extra, turns):
        server, requests = serve(provider, turns)
        host = f"http://127.0.0.1:{server.server_port}"
        try:
            output = subprocess.run([binary, "run", "--prompt", "Check the notes.", "--json", "--provider", provider,
                                     "--model", "test-model", *extra],
                                    cwd=root, stdin=subprocess.DEVNULL, capture_output=True, timeout=60,
                                    env=dict(env, OMLX_HOST=host, OLLAMA_HOST=host))
            assert output.returncode == 0, output.stderr
            return requests, json.loads(output.stdout)
        finally:
            server.shutdown()
            server.server_close()

    turns = [("", ("read_file", {"path": "sub/notes.txt"})), ("", ("read_file", {"path": "sub/notes.txt"})), ("Done.", None)]
    requests, summary = run(turns=turns)
    prompt = system(requests[0])
    assert "# Project guidance" in prompt, prompt
    user_header = f"## {config_home / 'microgentic' / 'AGENTS.md'} (user guidance)"
    assert user_header in prompt, prompt
    assert "## AGENTS.md (project directory and repository root)" in prompt, prompt
    assert prompt.index("USER-RULE") < prompt.index("ROOT-RULE"), "user guidance must come first"
    assert "- sub/ (sub/AGENTS.md)" in prompt and "SUB-RULE" not in prompt
    # The nested file arrives after the first read under sub/, once.
    assert not any("SUB-RULE" in text for text in users(requests[0]))
    injected = [text for text in users(requests[1]) if text.startswith("[Project guidance for sub/ from sub/AGENTS.md.")]
    assert len(injected) == 1 and "SUB-RULE" in injected[0], users(requests[1])
    assert sum("SUB-RULE" in text for text in users(requests[2])) == 1, "nested guidance was injected twice"
    records = [json.loads(line) for line in Path(summary["trace_file"]).read_text().splitlines()]
    discovered = next(r["payload"] for r in records if r["kind"] == "context.guidance.discovered")
    statuses = {c["shown"]: c["status"]["status"] for c in discovered["candidates"]}
    assert statuses == {str(config_home / "microgentic" / "AGENTS.md"): "included", "AGENTS.md": "included",
                        "sub/AGENTS.md": "pending"}, statuses
    assert all(len(c["sha256"]) == 64 for c in discovered["candidates"])
    assert any(r["kind"] == "context.guidance.nested_injected" for r in records)

    # Guidance never changes the tools; the flag removes it entirely.
    plain, summary = run("--no-project-guidance", turns=[("Done.", None)])
    assert "Project guidance" not in json.dumps(plain[0])
    assert [t["function"]["name"] for t in plain[0]["tools"]] == [t["function"]["name"] for t in requests[0]["tools"]]
    records = [json.loads(line) for line in Path(summary["trace_file"]).read_text().splitlines()]
    assert not any(r["kind"].startswith("context.guidance") for r in records)

    # The conversation worker reads guidance too, and notices changes between exchanges.
    session = work / "session" / "conversation.json"
    session.parent.mkdir()
    server, requests = serve(provider, [("Hello.", None)])
    host = f"http://127.0.0.1:{server.server_port}"
    try:
        for prompt_text in ("Hi.", "Again."):
            output = subprocess.run([binary, "conversation-worker", "--prompt", prompt_text, "--session-state", str(session),
                                     "--provider", provider, "--model", "test-model"],
                                    cwd=root, stdin=subprocess.DEVNULL, capture_output=True, timeout=60,
                                    env=dict(env, OMLX_HOST=host, OLLAMA_HOST=host))
            assert output.returncode == 0, output.stderr
            (root / "AGENTS.md").write_text("ROOT-RULE: run tests with make verify.\n")
    finally:
        server.shutdown()
        server.server_close()
    assert "ROOT-RULE: run tests with make check." in system(requests[0])
    assert "ROOT-RULE: run tests with make verify." in system(requests[1])
    assert "project guidance was updated" not in users(requests[0])[-1]
    assert "project guidance was updated since the last message (AGENTS.md changed)" in users(requests[1])[-1], users(requests[1])
