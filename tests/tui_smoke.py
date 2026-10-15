"""PTY lifecycle check; no prompt is submitted and no model is loaded."""
import fcntl
import os
import pty
import re
import select
import struct
import subprocess
import sys
import termios
import time

# Foreground colour as crossterm writes it (CSI 38;5;n m); attributes alone are not colour.
COLOUR = re.compile(rb"\x1b\[38;5;\d+m")
SGR = re.compile(rb"\x1b\[[0-9;]*m")


def open_view(no_color):
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 90, 0, 0))
    env = dict(os.environ, TERM="xterm-256color", OMLX_HOST="http://127.0.0.1:1")
    env.pop("NO_COLOR", None)
    if no_color is not None:
        env["NO_COLOR"] = no_color
    process = subprocess.Popen([sys.argv[1], "--config", os.path.join(sys.argv[2], "config.json")],
                               stdin=slave, stdout=slave, stderr=slave, env=env)
    try:
        captured = b""
        deadline = time.monotonic() + 10
        while b"Enter send" not in captured and time.monotonic() < deadline:
            if select.select([master], [], [], .1)[0]:
                captured += os.read(master, 65536)
        assert b"Microgentic" in captured and b"Enter send" in captured, captured
        # Small resize and Unicode input exercise layout without submitting work.
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 5, 20, 0, 0))
        os.write(master, "世界".encode())
        time.sleep(.1)
        os.write(master, b"\x1b")
        assert process.wait(timeout=10) == 0
        assert termios.tcgetattr(slave) == before, "raw terminal mode was not restored"
        return captured
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
        os.close(master)
        os.close(slave)


assert COLOUR.search(open_view(None)), "the default view has no colour"
# An empty NO_COLOR does not disable colour (https://no-color.org).
assert COLOUR.search(open_view("")), "an empty NO_COLOR disabled colour"
plain = open_view("1")
assert not COLOUR.search(plain), "NO_COLOR still produced colour"
assert "● Ready".encode() in SGR.sub(b"", plain), "the monochrome status lost its label"
