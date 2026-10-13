"""PTY lifecycle check; no prompt is submitted and no model is loaded."""
import fcntl
import os
import pty
import select
import struct
import subprocess
import sys
import termios
import time

master, slave = pty.openpty()
before = termios.tcgetattr(slave)
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 90, 0, 0))
env = dict(os.environ, TERM="xterm-256color", OMLX_HOST="http://127.0.0.1:1")
process = subprocess.Popen([sys.argv[1], "--config", os.path.join(sys.argv[2], "config.json")], stdin=slave, stdout=slave, stderr=slave, env=env)
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
finally:
    if process.poll() is None:
        process.terminate()
        process.wait(timeout=10)
    os.close(master)
    os.close(slave)
