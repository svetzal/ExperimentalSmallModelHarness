"""Thinking streams live, then collapses above its tool calls. No model."""
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
SGR = re.compile(rb"\x1b\[[0-9;]*m")
ROW = re.compile(rb"\x1b\[(\d+);(\d+)H([^\x1b]*)")
FIRST_THOUGHT = ["\nThe user wants", " the pacing constant.", "\nLet me search", " for it first.\n"]
# Each turn: thinking chunks, content chunks, tool calls.
TURNS = [
    (FIRST_THOUGHT, ["\n\n"], [("read_file", dict(path="notes.txt", line_start=1, line_end=2)),
                               ("list_tree", dict(path=".", max_depth=2))]),
    (["\nThe constant is likely", " in notes.txt.\n"], ["\n\n"], [("read_file", dict(path="notes.txt"))]),
    (["\nI have what I need.\n"], ["\n\n", "The pace is ", "set by PACE."], []),
]


def screen_rows(frame):
    """Row number to text for one painted frame."""
    return {int(m.group(1)): m.group(3).decode().rstrip() for m in ROW.finditer(SGR.sub(b"", frame))}


def exercise(provider):
    root = base / provider / "project"
    root.mkdir(parents=True)
    (root / "notes.txt").write_text("PACE = 2.0\nsecond line\n")
    state, config = base / provider / "state", base / provider / "config.json"
    config.write_text(json.dumps(dict(schema_version="microgentic_config.v1", provider=provider,
                                     model="test-model", reasoning_effort="low")))
    requests, errors = [], []
    thought_seen = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
                thinking, content, calls = TURNS[len(requests) - 1]
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream" if provider == "omlx" else "application/x-ndjson")
                self.send_header("Connection", "close")
                self.end_headers()

                def frame(content=None, thinking=None, tools=None, done=False):
                    if provider == "omlx":
                        delta = {}
                        if content is not None:
                            delta["content"] = content
                        if thinking is not None:
                            delta["reasoning_content"] = thinking
                        if tools:
                            delta["tool_calls"] = [
                                dict(index=i, id=f"c{len(requests)}-{i}", type="function",
                                     function=dict(name=name, arguments=json.dumps(arguments)))
                                for i, (name, arguments) in enumerate(tools)]
                        reason = ("tool_calls" if calls else "stop") if done else None
                        body = "data: " + json.dumps(dict(choices=[dict(delta=delta, finish_reason=reason)])) + "\n\n"
                        if done:
                            body += "data: [DONE]\n\n"
                    else:
                        message = dict(role="assistant", content=content or "")
                        if thinking is not None:
                            message["thinking"] = thinking
                        if tools:
                            message["tool_calls"] = [dict(function=dict(name=n, arguments=a)) for n, a in tools]
                        body = json.dumps(dict(message=message, done=done, **({"done_reason": "stop"} if done else {}))) + "\n"
                    self.wfile.write(body.encode())
                    self.wfile.flush()

                for chunk in thinking:
                    frame(thinking=chunk)
                if len(requests) == 1:
                    # Hold the stream until the test has seen the live thinking.
                    assert thought_seen.wait(15), "the live thinking never appeared"
                for chunk in content:
                    frame(content=chunk)
                if calls:
                    frame(tools=calls)
                frame(done=True)
            except (BrokenPipeError, ConnectionResetError):
                pass
            except BaseException as error:
                errors.append(error)
            finally:
                self.close_connection = True

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    host = f"http://127.0.0.1:{server.server_port}"
    env = dict(os.environ, TERM="xterm-256color", XDG_STATE_HOME=str(state),
               OMLX_HOST=host, OLLAMA_HOST=host, OMLX_API_KEY="test-key")
    env.pop("NO_COLOR", None)
    env["MICROGENTIC_NO_ANIMATION"] = "1"
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
    process = subprocess.Popen([binary, "--config", str(config)], cwd=root, env=env,
                               stdin=slave, stdout=slave, stderr=slave)
    seen, cursor = b"", 0

    def wait_for(needle):
        nonlocal seen, cursor
        deadline = time.monotonic() + 20
        while needle not in SGR.sub(b"", seen[cursor:]) and time.monotonic() < deadline:
            if select.select([master], [], [], .05)[0]:
                seen += os.read(master, 65536)
        tail = SGR.sub(b"", seen[cursor:])
        assert needle in tail, (needle, tail[-3000:], errors)
        cursor = len(seen)

    try:
        wait_for(b"Enter send")
        os.write(master, b"\x1b[200~Find the pacing constant.\x1b[201~\r")
        # The live thinking shows while the model is still thinking.
        wait_for(b"Let me search for it first.")
        live = screen_rows(seen.split(b"\x1b[2J")[-1])
        assert any(text.strip() == "The user wants the pacing constant." for text in live.values()), live
        thought_seen.set()
        wait_for(b"Ready for your next message")
        time.sleep(.2)
        while select.select([master], [], [], .05)[0]:
            seen += os.read(master, 65536)
        rows = screen_rows(seen.split(b"\x1b[2J")[-1])
        heading = next(row for row, text in rows.items() if text.startswith("Microgentic:"))
        reply = next(row for row, text in rows.items() if text.startswith("The pace is set by PACE."))
        exchange = [rows.get(row, "") for row in range(heading, reply + 1)]
        assert exchange == [
            "Microgentic:",
            "  ∴ Let me search for it first.",
            "  ✓ read_file notes.txt:1-2",
            "  ✓ list_tree . depth 2",
            "  ∴ The constant is likely in notes.txt.",
            "  ✓ read_file notes.txt",
            "The pace is set by PACE.",
        ], exchange
        screen = "\n".join(rows.values())
        assert "The user wants" not in screen, "the full thinking stayed on screen"
        assert "I have what I need." not in screen, "thinking before the reply was kept"
        assert len(requests) == 3
        os.write(master, b"\x1b")
        deadline = time.monotonic() + 10
        while process.poll() is None and time.monotonic() < deadline:
            if select.select([master], [], [], .05)[0]:
                os.read(master, 65536)
        assert process.wait(timeout=1) == 0
        assert termios.tcgetattr(slave) == before
        assert not errors, errors
    finally:
        thought_seen.set()
        if process.poll() is None:
            process.kill()
            process.wait(timeout=10)
        os.close(master)
        os.close(slave)
        server.shutdown()
        server.server_close()


for selected in ("omlx", "ollama"):
    exercise(selected)
