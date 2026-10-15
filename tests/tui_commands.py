"""Slash commands stay local and change only later provider requests. No model."""
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


def exercise(provider):
    root = base / provider / "project"
    root.mkdir(parents=True)
    state, config = base / provider / "state", base / provider / "config.json"
    saved_config = json.dumps(dict(schema_version="microgentic_config.v1", provider=provider,
                                   model="test-model", reasoning_effort="low"))
    config.write_text(saved_config)
    requests, listings, errors = [], [], []
    release_slow = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def reply(self, body, content_type):
            self.send_response(200)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            listings.append(self.path)
            if provider == "omlx":
                body = dict(data=[dict(id="test-model"), dict(id="other-model")])
            else:
                body = dict(models=[dict(name="test-model"), dict(name="other-model")])
            self.reply(json.dumps(body).encode(), "application/json")

        def do_POST(self):
            try:
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(request)
                text = f"REPLY-{len(requests)}"
                if request["messages"][-1]["content"] == "Take your time.":
                    # Hold the reply open so commands arrive while a run is active.
                    assert release_slow.wait(15), "the busy-command check never released the reply"
                if provider == "omlx":
                    frame = dict(choices=[dict(delta=dict(content=text), finish_reason="stop")])
                    body = "data: " + json.dumps(frame) + "\n\ndata: [DONE]\n\n"
                    self.reply(body.encode(), "text/event-stream")
                else:
                    body = (json.dumps(dict(message=dict(role="assistant", content=text), done=False)) + "\n"
                            + json.dumps(dict(message=dict(role="assistant", content=""), done=True,
                                              done_reason="stop")) + "\n")
                    self.reply(body.encode(), "application/json")
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
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 160, 0, 0))
    process = subprocess.Popen([binary, "--config", str(config)], cwd=root, env=env,
                               stdin=slave, stdout=slave, stderr=slave)
    pending = b""

    def wait_for(needle):
        nonlocal pending
        data, pending = pending, b""
        deadline = time.monotonic() + 20
        while needle not in data and time.monotonic() < deadline:
            if select.select([master], [], [], .05)[0]:
                data = SGR.sub(b"", data + os.read(master, 65536))
        assert needle in data, (needle, data[-6000:], errors)
        end = data.index(needle) + len(needle)
        pending = data[end:]
        return data[:end]

    def type_line(text):
        os.write(master, b"\x1b[200~" + text.encode() + b"\x1b[201~\r")

    def exchange(prompt):
        count = len(requests)
        type_line(prompt)
        wait_for(f"REPLY-{count + 1}".encode())
        wait_for(b"Ready for your next message")
        assert len(requests) == count + 1
        return requests[-1]

    def press(keys):
        os.write(master, keys)

    def effort_control(request):
        return request.get("reasoning_effort" if provider == "omlx" else "think")

    def history_text(request):
        return json.dumps(request["messages"])

    try:
        wait_for(b"Enter send")
        type_line("/help")
        wait_for(b"Start a new conversation")
        type_line("/nope")
        wait_for(b"Unknown command /nope. Type /help to see commands.")
        type_line("/effort extreme")
        wait_for(b"Unknown effort \"extreme\"")
        type_line("/model")
        wait_for(b"Model: test-model on " + provider.encode())
        wait_for(b"Choose a model from " + provider.encode())
        picker = wait_for(b"Enter choose | Esc cancel")  # the hint ends the picker's frame
        assert "› test-model (current)".encode() in picker and b"  other-model" in picker, picker
        press(b"\x1b")  # Esc closes the picker without a change.
        wait_for(b"Enter send")
        type_line("/effort")
        wait_for("› low (current)".encode())
        press(b"\x1b[B\x1b")  # Down, then Esc: still no change.
        wait_for(b"Enter send")
        assert not requests, "a command reached the provider"
        assert len(listings) == 1, listings

        first = exchange("First message.")
        assert first["model"] == "test-model" and effort_control(first) == "low"
        follow = exchange("Second message.")
        assert "First message." in history_text(follow), "ordinary follow-ups lost their history"

        type_line("/model")
        wait_for("› test-model (current)".encode())
        press(b"\x1b[B\r")  # Down to other-model, then Enter.
        wait_for(b"Model set to other-model for this session.")
        # The status bar is drawn below the history in the same frame.
        wait_for("🤖 other-model  🧠 low".encode())
        switched = exchange("After the model switch.")
        assert switched["model"] == "other-model"
        assert effort_control(switched) == "low"
        assert "First message." not in history_text(switched), "history crossed a model change"

        type_line("/effort")
        wait_for("› low (current)".encode())
        press(b"\x1b[B\x1b[B\r")  # low -> medium -> high, then Enter.
        wait_for(b"Effort set to high for this session.")
        wait_for("🤖 other-model  🧠 high".encode())
        raised = exchange("After the effort change.")
        assert raised["model"] == "other-model" and effort_control(raised) == "high"
        kept = exchange("Still the same settings.")
        assert "After the effort change." in history_text(kept)

        type_line("/clear")
        wait_for(b"Started a new conversation.")
        cleared = exchange("After clearing.")
        assert len(cleared["messages"]) == 2, cleared["messages"]
        assert "Still the same settings." not in history_text(cleared)
        sessions = list((state / "microgentic/runs").glob("session-*/conversation.json"))
        assert len(sessions) == 4, "clearing or switching deleted retained session state"

        type_line("/effort default")
        wait_for(b"Effort set to model default for this session.")
        unset = exchange("With the model default effort.")
        assert effort_control(unset) is None, unset

        escaped = exchange("//etc/hosts is a path, not a command.")
        assert escaped["messages"][-1]["content"] == "/etc/hosts is a path, not a command."

        type_line("Take your time.")
        wait_for(b"Ctrl-C to cancel")
        type_line("/model test-model")
        wait_for(b"/model is unavailable while Microgentic is replying.")
        type_line("/help")
        wait_for(b"Start a new conversation")
        release_slow.set()
        wait_for(b"Ready for your next message")
        busy = requests[-1]
        assert busy["model"] == "other-model", "a rejected command changed the model"

        assert config.read_text() == saved_config, "session commands wrote the configuration"
        os.write(master, b"\x1b")
        assert process.wait(timeout=10) == 0
        assert termios.tcgetattr(slave) == before
        assert not errors, errors
    finally:
        release_slow.set()
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
