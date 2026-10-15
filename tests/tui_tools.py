"""Tool calls show as one live line each, resolving in place. No model."""
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
SPINNER = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
# An alternation, not a byte class: each glyph is several UTF-8 bytes.
RUNNING_MARK = ("(?:" + "|".join(SPINNER) + ") ").encode()
RUNNING = re.compile(RUNNING_MARK)


def exercise(provider):
    root = base / provider / "project"
    root.mkdir(parents=True)
    (root / "notes.txt").write_text("first line\nsecond line\nthird line\n")
    state, config = base / provider / "state", base / provider / "config.json"
    config.write_text(json.dumps(dict(schema_version="microgentic_config.v1", provider=provider,
                                     model="test-model", reasoning_effort="low")))
    requests, errors = [], []
    turns = [
        ("Checking two things.", [
            ("shell_command", dict(command="sleep 1", timeout_secs=30)),
            ("read_file", dict(path="notes.txt", line_start=1, line_end=2)),
        ]),
        ("", [("read_file", dict(path="missing.txt"))]),
        ("All done.", []),
    ]

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
                text, calls = turns[len(requests) - 1]
                if provider == "omlx":
                    delta = dict(content=text)
                    if calls:
                        delta["tool_calls"] = [
                            dict(index=i, id=f"call-{len(requests)}-{i}", type="function",
                                 function=dict(name=name, arguments=json.dumps(arguments)))
                            for i, (name, arguments) in enumerate(calls)]
                    frame = dict(choices=[dict(delta=delta, finish_reason="tool_calls" if calls else "stop")])
                    body, kind = "data: " + json.dumps(frame) + "\n\ndata: [DONE]\n\n", "text/event-stream"
                else:
                    message = dict(role="assistant", content=text)
                    if calls:
                        message["tool_calls"] = [dict(function=dict(name=name, arguments=arguments))
                                                 for name, arguments in calls]
                    body = (json.dumps(dict(message=message, done=False)) + "\n"
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
    # The launch animation has its own test; the rest stay deterministic.
    env["MICROGENTIC_NO_ANIMATION"] = "1"
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 160, 0, 0))
    process = subprocess.Popen([binary, "--config", str(config)], cwd=root, env=env,
                               stdin=slave, stdout=slave, stderr=slave)
    seen, cursor = b"", 0

    def wait_for(needle):
        nonlocal seen, cursor
        deadline = time.monotonic() + 20
        start = cursor
        while needle not in seen[start:] and time.monotonic() < deadline:
            if select.select([master], [], [], .05)[0]:
                seen = SGR.sub(b"", seen + os.read(master, 65536))
        assert needle in seen[start:], (needle, seen[-6000:], errors)
        cursor = seen.index(needle, start) + len(needle)

    def frames():
        return seen.split(b"\x1b[2J")

    try:
        wait_for(b"Enter send")
        os.write(master, b"\x1b[200~Check the notes.\x1b[201~\r")
        wait_for(b"All done.")
        wait_for(b"Ready for your next message")

        shell, read = "shell_command sleep 1".encode(), "read_file notes.txt:1-2".encode()
        running_together = [frame for frame in frames()
                            if re.search(RUNNING_MARK + re.escape(shell), frame)
                            and re.search(RUNNING_MARK + re.escape(read), frame)]
        assert running_together, "the batch never showed both calls running at once"
        spinner_glyphs = {glyph for glyph in SPINNER if (glyph + " ").encode() + shell in seen}
        assert len(spinner_glyphs) >= 2, f"the spinner did not animate: {spinner_glyphs}"

        final = frames()[-1]
        for line in ("✓ shell_command sleep 1", "✓ read_file notes.txt:1-2", "✗ read_file missing.txt"):
            assert line.encode() in final, (line, final)
        region = final.split(b"Ready for your next message")[0].split(b"Microgentic:")[-1]
        assert not RUNNING.search(region), ("a call still spins after the run finished", region)
        assert b"finished" not in seen, "a separate finished line appeared"
        assert not re.search(rb"H {2}Using ", seen), "a separate Using line appeared in the history"
        assert b"Evidence:" in final
        assert len(requests) == 3
        os.write(master, b"\x1b")
        assert process.wait(timeout=10) == 0
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


for selected in ("omlx", "ollama"):
    exercise(selected)
