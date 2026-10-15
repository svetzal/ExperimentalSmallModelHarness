"""The status bar shows the model, effort, folder, git state and context fill. No model."""
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import sys
import termios
import threading
import time

binary, base = str(Path(sys.argv[1]).resolve()), Path(sys.argv[2])
# Colour stays on; matching ignores SGR styling so text within a row stays contiguous.
SGR = re.compile(rb"\x1b\[[0-9;]*m")


def git(root, *args):
    subprocess.run(["git", *args], cwd=root, check=True, capture_output=True,
                   env=dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.com",
                            GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.com"))


def compact(tokens):
    if tokens < 1000:
        return str(tokens)
    return f"{tokens / 1000:.1f}k" if tokens < 1_000_000 else f"{tokens / 1_000_000:.1f}M"


def exercise(provider, model, in_git):
    root = base / provider / "project"
    root.mkdir(parents=True)
    (root / "tracked.txt").write_text("one\n")
    if in_git:
        git(root, "init", "-q", "-b", "main")
        git(root, "add", "tracked.txt")
        git(root, "commit", "-q", "-m", "start")
        (root / "tracked.txt").write_text("two\n")
        (root / "new.txt").write_text("untracked\n")
    state, config = base / provider / "state", base / provider / "config.json"
    config.write_text(json.dumps(dict(schema_version="microgentic_config.v1", provider=provider,
                                     model=model, reasoning_effort="low")))
    errors, replies = [], []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                replies.append(f"REPLY-{len(replies) + 1}")
                text = replies[-1]
                if provider == "omlx":
                    frame = dict(choices=[dict(delta=dict(content=text), finish_reason="stop")])
                    body, kind = "data: " + json.dumps(frame) + "\n\ndata: [DONE]\n\n", "text/event-stream"
                else:
                    body = (json.dumps(dict(message=dict(role="assistant", content=text), done=False)) + "\n"
                            + json.dumps(dict(message=dict(role="assistant", content=""), done=True,
                                              done_reason="stop")) + "\n")
                    kind = "application/json"
                self.send_response(200)
                self.send_header("Content-Type", kind)
                self.send_header("Content-Length", str(len(body.encode())))
                self.end_headers()
                self.wfile.write(body.encode())
            except BaseException as error:
                errors.append(error)

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    host = f"http://127.0.0.1:{server.server_port}"
    env = dict(os.environ, TERM="xterm-256color", XDG_STATE_HOME=str(state),
               OMLX_HOST=host, OLLAMA_HOST=host, OMLX_API_KEY="test-key")
    env.pop("NO_COLOR", None)
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 160, 0, 0))
    process = subprocess.Popen([binary, "--config", str(config)], cwd=root, env=env,
                               stdin=slave, stdout=slave, stderr=slave)
    seen, cursor = b"", 0

    def wait_for(needle):
        nonlocal seen, cursor
        start = cursor
        deadline = time.monotonic() + 20
        while needle not in seen[start:] and time.monotonic() < deadline:
            if select.select([master], [], [], .05)[0]:
                seen = SGR.sub(b"", seen + os.read(master, 65536))
        assert needle in seen[start:], (needle, seen.split(b"\x1b[2J")[-1].replace("─".encode(), b""), errors)
        cursor = seen.index(needle, start) + len(needle)

    def exchange(prompt):
        # The status line already reads "Ready" before the run starts, so wait
        # for this exchange's own reply first.
        expected = f"REPLY-{len(replies) + 1}".encode()
        os.write(master, b"\x1b[200~" + prompt.encode() + b"\x1b[201~\r")
        wait_for(expected)
        wait_for(b"Ready for your next message")

    def last_ledger():
        traces = sorted((state / "microgentic/runs").glob("run-*/traces/*.jsonl"), key=os.path.getmtime)
        events = [json.loads(line) for line in traces[-1].read_text().splitlines()]
        return [e["payload"] for e in events if e["kind"] == "llm.context_assembly.ledger"][-1]

    try:
        identity = f"🤖 {model}  🧠 low  📁 project"
        if model == "qwen3.8:27b-mxfp8":
            # A known window shows an empty gauge before the first request.
            wait_for((identity + "  🌿 main ●1 …1  ctx:[░░░░░░░░░░]0%").encode())
        else:
            wait_for(b"Enter send")
            assert identity.encode() + b"\x1b" in seen, "the bar showed more than it knows"
        exchange("Hello.")
        ledger = last_ledger()
        used, window = ledger["estimated_tokens"], ledger["context_window_tokens"]
        if window:
            assert window == 262_144
            gauge = f"]{round(used * 100 / window)}%"
        else:
            gauge = f"ctx: {compact(used)}"
        wait_for(gauge.encode())
        if in_git:
            # The bar refreshes git after each exchange, never on a timer.
            (root / "another.txt").write_text("x\n")
            exchange("Again.")
            wait_for("🌿 main ●1 …2".encode())
        else:
            assert "🌿".encode() not in seen, "a project outside git showed a branch"
        os.write(master, b"\x1b")
        deadline = time.monotonic() + 10
        while process.poll() is None and time.monotonic() < deadline:
            if select.select([master], [], [], .05)[0]:
                os.read(master, 65536)
        assert process.wait(timeout=1) == 0
        assert termios.tcgetattr(slave) == before
        assert not errors, errors
    finally:
        if process.poll() is None:
            process.terminate()
            deadline = time.monotonic() + 10
            while process.poll() is None and time.monotonic() < deadline:
                if select.select([master], [], [], .05)[0]:
                    os.read(master, 65536)
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)
        os.close(master)
        os.close(slave)
        server.shutdown()
        server.server_close()


exercise("ollama", "qwen3.8:27b-mxfp8", in_git=True)
exercise("omlx", "test-model", in_git=False)
