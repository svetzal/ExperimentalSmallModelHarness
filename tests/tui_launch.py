"""The launch animation plays, settles on the resting screen, and never blocks input. No model."""
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
SPARKS = [glyph.encode() for glyph in "✦✧"]
requests = []


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        requests.append(("GET", self.path))
        self.send_response(500)
        self.end_headers()

    def do_POST(self):
        requests.append(("POST", self.path))
        self.send_response(500)
        self.end_headers()


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
host = f"http://127.0.0.1:{server.server_port}"
root = base / "project"
root.mkdir(parents=True)
config = base / "config.json"
config.write_text(json.dumps(dict(schema_version="microgentic_config.v1", provider="omlx",
                                 model="test-model", reasoning_effort="low")))
env = dict(os.environ, TERM="xterm-256color", XDG_STATE_HOME=str(base / "state"),
           OMLX_HOST=host, OLLAMA_HOST=host, OMLX_API_KEY="test-key")
env.pop("NO_COLOR", None)
env.pop("MICROGENTIC_NO_ANIMATION", None)


def frames(data):
    return SGR.sub(b"", data).split(b"\x1b[2J")


def resting(frame):
    return (b"Type /help for commands." in frame and b"Talk to Microgentic." in frame
            and b"Enter send" in frame and not any(spark in frame for spark in SPARKS))


def launch(keys_after_first_frame=None):
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 90, 0, 0))
    started = time.monotonic()
    process = subprocess.Popen([binary, "--config", str(config)], cwd=root, env=env,
                               stdin=slave, stdout=slave, stderr=slave)
    data, settled_at, typed_at, first_frame_at = b"", None, None, None
    try:
        # Time from the first painted frame: a freshly built binary can take a
        # moment to start on macOS, which is not the animation's doing.
        deadline = started + 10
        while time.monotonic() < deadline and settled_at is None:
            if select.select([master], [], [], .01)[0]:
                data += os.read(master, 65536)
            shown = frames(data)
            if first_frame_at is None and len(shown) > 1:
                first_frame_at = time.monotonic()
            if keys_after_first_frame and typed_at is None and len(shown) > 1:
                os.write(master, keys_after_first_frame)
                typed_at = time.monotonic()
            if len(shown) > 1 and resting(shown[-1]) and (typed_at is None or b"> x" in shown[-1]):
                settled_at = time.monotonic()
        assert settled_at is not None, frames(data)[-1][-3000:]
        # Collect anything drawn after settling, then quit.
        time.sleep(.3)
        while select.select([master], [], [], .05)[0]:
            data += os.read(master, 65536)
        os.write(master, b"\x1b")
        deadline = time.monotonic() + 10
        while process.poll() is None and time.monotonic() < deadline:
            if select.select([master], [], [], .05)[0]:
                os.read(master, 65536)
        assert process.wait(timeout=1) == 0
        assert termios.tcgetattr(slave) == before
        return data, settled_at - first_frame_at, typed_at
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=10)
        os.close(master)
        os.close(slave)


try:
    # Left alone, the animation plays and lands on the resting screen.
    data, settled, _ = launch()
    shown = frames(data)
    assert any("✦".encode() in frame for frame in shown), "the seed never appeared"
    assert any(b"microgentic" in frame for frame in shown), "the letters never assembled"
    assert len(shown) >= 6, f"only {len(shown)} frames: the animation did not play"
    assert settled < 2.4, f"the resting screen took {settled:.2f}s"
    assert resting(shown[-1]), shown[-1][-2000:]

    # A key during the animation skips it, and the key lands in the input.
    data, _, typed_at = launch(keys_after_first_frame=b"x")
    assert typed_at is not None
    after = frames(data)
    first_with_key = next(i for i, frame in enumerate(after) if b"> x" in frame)
    assert all(resting(frame) for frame in after[first_with_key:] if b"Enter send" in frame), \
        "the animation kept playing after a key"
    assert not requests, f"launch contacted the provider: {requests}"
finally:
    server.shutdown()
    server.server_close()
