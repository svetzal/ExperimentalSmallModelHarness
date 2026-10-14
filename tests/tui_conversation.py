"""Exercise the real TUI, stream transport and conversation without a model."""
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import sys
import termios
import threading
import time

base_directory = Path(sys.argv[2])


def exercise(provider):
    root = base / provider / "project"
    root.mkdir(parents=True)
    (root / "memo.txt").write_text("Codename copperfern. Unicode 世界.\n")
    state = base / provider / "state"
    config = base / provider / "config.json"
    config.write_text(json.dumps(dict(schema_version="microgentic_config.v1",
                                     provider=provider, model="test-model", reasoning_effort="low")))
    requests = []
    release_first, release_cancel = threading.Event(), threading.Event()
    errors = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(request)
                index = len(requests)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream" if provider == "omlx" else "application/x-ndjson")
                self.send_header("Connection", "close")
                self.end_headers()

                def send(content=None, thinking=None, tool=False, done=False):
                    if provider == "omlx":
                        delta = {}
                        if content is not None:
                            delta["content"] = content
                        if thinking is not None:
                            delta["reasoning_content"] = thinking
                        if tool:
                            delta["tool_calls"] = [dict(index=0, id="memo-read", type="function",
                                                        function=dict(name="read_file", arguments=json.dumps(dict(path="memo.txt"))))]
                        frame = dict(choices=[dict(delta=delta, finish_reason="stop" if done else None)])
                        self.wfile.write(("data: " + json.dumps(frame) + "\n\n").encode())
                        if done:
                            self.wfile.write(b"data: [DONE]\n\n")
                    else:
                        message = dict(role="assistant", content=content or "")
                        if thinking is not None:
                            message["thinking"] = thinking
                        if tool:
                            message["tool_calls"] = [dict(function=dict(name="read_file", arguments=dict(path="memo.txt")))]
                        frame = dict(message=message, done=done)
                        if done:
                            frame["done_reason"] = "stop"
                        self.wfile.write((json.dumps(frame) + "\n").encode())
                    self.wfile.flush()

                if index == 1:
                    send(thinking="Looking at the memo")
                    send(content="FIRST-LIVE-" + "x" * 150 + "-END")
                    assert release_first.wait(10), "TUI never showed the live chunk"
                    send(tool=True)
                    send(done=True)
                elif index == 2:
                    send(content="Copperfern stored.\nDONE")
                    send(done=True)
                elif index == 3:
                    send(content="FOLLOWUP-LIVE copperfern 世界\n")
                    send(content="DONE")
                    send(done=True)
                elif index == 4:
                    send(content="UNFINISHED-REPLY")
                    assert release_cancel.wait(10), "cancellation did not settle"
                    # Deliberately close without completing the native response.
                elif index == 5:
                    send(content="RECOVERED\nDONE")
                    send(done=True)
                elif index in (6, 7):
                    send(content="DONE")
                    send(done=True)
                else:
                    raise AssertionError(f"unexpected provider request {index}")
            except (BrokenPipeError, ConnectionResetError):
                pass
            except BaseException as error:
                errors.append(error)
            finally:
                self.close_connection = True

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    host = f"http://127.0.0.1:{server.server_port}"
    env = dict(os.environ, TERM="xterm-256color", XDG_STATE_HOME=str(state),
               OMLX_HOST=host, OLLAMA_HOST=host, OMLX_API_KEY="test-key")
    pending = {}

    def launch():
        master, slave = pty.openpty()
        before = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 240, 0, 0))
        process = subprocess.Popen([binary, "--config", str(config)], cwd=root,
                                   stdin=slave, stdout=slave, stderr=slave, env=env)
        pending[master] = b""
        return process, master, slave, before

    def wait_for(master, needle):
        data = pending.pop(master, b"")
        deadline = time.monotonic() + 15
        while needle not in data and time.monotonic() < deadline:
            if select.select([master], [], [], .05)[0]:
                data += os.read(master, 65536)
        assert needle in data, (needle, data[-8000:])
        end = data.index(needle) + len(needle)
        pending[master] = data[end:]
        return data[:end]

    def send_prompt(master, text):
        # Bracketed paste handles Unicode and avoids timing-dependent key input.
        os.write(master, b"\x1b[200~" + text.encode() + b"\x1b[201~\r")

    process, master, slave, before = launch()
    try:
        wait_for(master, b"Enter send")
        send_prompt(master, "Remember codename copperfern. Read memo.txt, then finish.")
        live = wait_for(master, b"-END")
        assert len(requests) == 1, "stream only appeared after task completion"
        assert b"FIRST-LIVE-" in live and b"x" * 150 in live, "stream used truncated previews"
        release_first.set()
        completed = wait_for(master, b"Ready for your next message")
        assert b"read_file finished" in completed, "tool activity missing"
        send_prompt(master, "What codename and Unicode did you see? Do not reread the file.")
        wait_for(master, b"FOLLOWUP-LIVE")
        wait_for(master, b"Ready for your next message")
        assert len(requests) == 3
        messages = requests[2]["messages"]
        assert messages[0]["role"] == "system"
        assert sum(m["role"] == "system" for m in messages) == 1
        assert any(m["role"] == "user" and m.get("content") == "Remember codename copperfern. Read memo.txt, then finish." for m in messages)
        assert any(m["role"] == "assistant" and m.get("tool_calls") for m in messages)
        assert any(m["role"] == "tool" and "copperfern" in m.get("content", "") for m in messages)
        assert any(m["role"] == "assistant" and "Copperfern stored." in m.get("content", "") for m in messages)
        assert "What codename" in messages[-1]["content"]

        send_prompt(master, "Start a request I will cancel.")
        wait_for(master, b"UNFINISHED-REPLY")
        os.write(master, b"\x03")
        wait_for(master, b"You can follow up")
        release_cancel.set()
        session_file, = (state / "microgentic/runs").glob("session-*/conversation.json")
        saved = json.loads(session_file.read_text())
        assert saved["last_error"], "cancellation was treated as completed conversation"
        assert "UNFINISHED-REPLY" not in json.dumps(saved["messages"])
        send_prompt(master, "Continue after cancellation; keep our codename.")
        wait_for(master, b"RECOVERED")
        wait_for(master, b"Ready for your next message")
        assert "previous request stopped" in requests[4]["messages"][-1]["content"]
        assert "copperfern" in json.dumps(requests[4]["messages"])
        assert "UNFINISHED-REPLY" not in json.dumps(requests[4]["messages"])
        os.write(master, b"\x1b")
        assert process.wait(timeout=10) == 0
        assert termios.tcgetattr(slave) == before, "terminal did not restore after conversation"

        # Reusing state with another model/project must fail before any provider call.
        other = base / provider / "other-project"
        other.mkdir()
        for directory, model in [(root, "another-model"), (other, "test-model")]:
            output = subprocess.run([binary, "conversation-worker", "--prompt", "wrong session",
                                     "--session-state", str(session_file), "--provider", provider,
                                     "--model", model, "--reasoning-effort", "low"], cwd=directory,
                                    stdin=subprocess.DEVNULL, capture_output=True, env=env, timeout=15)
            assert output.returncode != 0 and b"different project or model" in output.stderr
        assert len(requests) == 5

        # A normal run ignores all conversation state, including in the same project.
        plain = subprocess.run([binary, "--config", str(config), "run", "--prompt", "A fresh isolated request", "--json",
                                "--provider", provider, "--model", "test-model", "--reasoning-effort", "low"],
                               cwd=root, stdin=subprocess.DEVNULL, capture_output=True, env=env, timeout=15)
        assert plain.returncode == 0, plain.stderr
        assert json.loads(plain.stdout)["final_summary"] == "DONE"
        assert len(requests[5]["messages"]) == 2
        assert "copperfern" not in json.dumps(requests[5]["messages"])
        for control in (["temperature", "max_tokens"] if provider == "omlx" else ["options", "think"]):
            assert requests[0].get(control) == requests[5].get(control), control
        assert requests[0]["tools"] == requests[5]["tools"]

        # Opening a second TUI starts a new conversation, never auto-resumes.
        os.close(master)
        os.close(slave)
        process, master, slave, before = launch()
        wait_for(master, b"Enter send")
        send_prompt(master, "Start a separate conversation.")
        wait_for(master, b"Ready for your next message")
        assert len(requests[6]["messages"]) == 2
        os.write(master, b"\x1b")
        assert process.wait(timeout=10) == 0
        assert termios.tcgetattr(slave) == before
        assert not errors, errors
    finally:
        release_first.set()
        release_cancel.set()
        if process.poll() is None:
            process.terminate()
            deadline = time.monotonic() + 10
            while process.poll() is None and time.monotonic() < deadline:
                # Keep draining the PTY during cleanup, as the real user would.
                if select.select([master], [], [], .05)[0]:
                    os.read(master, 65536)
            if process.poll() is None:
                process.kill()
            process.wait(timeout=10)
        os.close(master)
        os.close(slave)
        server.shutdown()
        server.server_close()


for executable in sys.argv[1:2] + sys.argv[3:4]:
    binary = str(Path(executable).resolve())
    base = base_directory / Path(binary).name
    for selected in ("omlx", "ollama"):
        exercise(selected)
