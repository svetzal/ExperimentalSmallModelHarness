"""At a narrow width, replies wrap at words and keep their indentation. No model."""
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
# Longer than two rows at this width, so it must be shortened rather than split.
URL = ("https://github.com/svetzal/microgentic/releases/download/v0.2.0/"
       "microgentic-0.2.0-aarch64-apple-darwin-with-a-deliberately-long-asset-name.tar.gz")
REPLY = (
    "Found it. The launch animation's pace is set by a single constant, and every stage of the "
    "timeline is divided by it, so the proportions never change when you adjust the speed.\n\n"
    f"The release notes are at {URL} if you want the details.\n\n"
    "```rust\n"
    "    const PACE: f64 = 1.1;\n"
    "    let hold = prototype_ms / PACE; // milliseconds for each stage of the burst\n"
    "```\n\n"
    "- Raise PACE to make the whole animation play faster than the prototype timeline\n"
    "- Lower it to slow everything down evenly"
)


def exercise(provider):
    root = base / provider / "project"
    root.mkdir(parents=True)
    (root / "notes.txt").write_text("PACE = 1.1\n")
    state, config = base / provider / "state", base / provider / "config.json"
    config.write_text(json.dumps(dict(schema_version="microgentic_config.v1", provider=provider,
                                     model="test-model", reasoning_effort="low")))
    requests, errors = [], []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
                first = len(requests) == 1
                calls = [("read_file", dict(path="notes.txt"))] if first else []
                chunks = ["\n\n"] if first else [REPLY[i:i + 17] for i in range(0, len(REPLY), 17)]
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream" if provider == "omlx" else "application/x-ndjson")
                self.send_header("Connection", "close")
                self.end_headers()
                for chunk in chunks:
                    if provider == "omlx":
                        body = "data: " + json.dumps(dict(choices=[dict(delta=dict(content=chunk), finish_reason=None)])) + "\n\n"
                    else:
                        body = json.dumps(dict(message=dict(role="assistant", content=chunk), done=False)) + "\n"
                    self.wfile.write(body.encode())
                if provider == "omlx":
                    delta = {}
                    if calls:
                        delta["tool_calls"] = [dict(index=0, id="c1", type="function",
                                                    function=dict(name=n, arguments=json.dumps(a))) for n, a in calls]
                    body = ("data: " + json.dumps(dict(choices=[dict(delta=delta, finish_reason="tool_calls" if calls else "stop")]))
                            + "\n\ndata: [DONE]\n\n")
                else:
                    message = dict(role="assistant", content="")
                    if calls:
                        message["tool_calls"] = [dict(function=dict(name=n, arguments=a)) for n, a in calls]
                    body = json.dumps(dict(message=message, done=True, done_reason="stop")) + "\n"
                self.wfile.write(body.encode())
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
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 48, 60, 0, 0))
    process = subprocess.Popen([binary, "--config", str(config)], cwd=root, env=env,
                               stdin=slave, stdout=slave, stderr=slave)
    seen = b""

    def wait_for(needle):
        nonlocal seen
        deadline = time.monotonic() + 20
        while needle not in SGR.sub(b"", seen) and time.monotonic() < deadline:
            if select.select([master], [], [], .05)[0]:
                seen += os.read(master, 65536)
        assert needle in SGR.sub(b"", seen), (needle, SGR.sub(b"", seen)[-3000:], errors)

    try:
        wait_for(b"Enter send")
        os.write(master, b"\x1b[200~Where is the pace set?\x1b[201~\r")
        wait_for(b"slow everything down evenly")
        wait_for(b"Ready for your next message")
        time.sleep(.3)
        while select.select([master], [], [], .05)[0]:
            seen += os.read(master, 65536)
        frame = SGR.sub(b"", seen.split(b"\x1b[2J")[-1])
        rows = {}
        for m in ROW.finditer(frame):
            # The canvas paints from the first written cell; restore the margin column.
            rows[int(m.group(1))] = " " * (int(m.group(2)) - 2) + m.group(3).decode().rstrip()
        tool = next(row for row, text in rows.items() if "✓ read_file notes.txt" in text)
        first = next(row for row, text in rows.items() if text.startswith("Found it."))
        assert first == tool + 2 and rows.get(tool + 1, "") == "", "no blank row above the reply"
        last = next(row for row, text in rows.items() if "slow everything down evenly" in text)
        reply = [rows.get(row, "") for row in range(first, last + 1)]
        for text in reply:
            assert len(text) <= 58, text
            leading = len(text) - len(text.lstrip(" "))
            assert leading in (0, 2, 4), f"stray leading space: {text!r}"
        assert "    const PACE: f64 = 1.1;" in reply, reply
        original = set(REPLY.split())
        for token in " ".join(reply).split():
            assert token in original or "…" in token, f"split or altered word: {token!r}\n" + "\n".join(reply)
        shortened = [token for token in " ".join(reply).split() if token.startswith("https://")]
        assert shortened and "…" in shortened[0] and shortened[0].endswith("asset-name.tar.gz"), shortened
        assert any(text.startswith("- Raise PACE") for text in reply)
        assert any(text.startswith("  ") and not text.startswith("    ") for text in reply), "no list hanging indent"
        os.write(master, b"\x1b")
        deadline = time.monotonic() + 10
        while process.poll() is None and time.monotonic() < deadline:
            if select.select([master], [], [], .05)[0]:
                os.read(master, 65536)
        assert process.wait(timeout=1) == 0
        assert not errors, errors
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=10)
        os.close(master)
        os.close(slave)
        server.shutdown()
        server.server_close()


for selected in ("omlx", "ollama"):
    exercise(selected)
