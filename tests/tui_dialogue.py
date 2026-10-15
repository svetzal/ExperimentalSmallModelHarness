"""No assumed engineering task; requested work and checks remain available. No model."""
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
    (root / "src").mkdir()
    (root / "Cargo.toml").write_text('[package]\nname="dialogue_fixture"\nversion="0.1.0"\nedition="2021"\n')
    (root / ".gitignore").write_text("target/\n")
    state, config = base / provider / "state", base / provider / "config.json"
    config.write_text(json.dumps(dict(schema_version="microgentic_config.v1", provider=provider,
                                     model="test-model", reasoning_effort="low")))
    requests, errors = [], []
    replies = ["Hello! What would you like to talk about?", "", "I am ready to chat.",
               "That is an example we can discuss without running it.", "I will add the function you requested.",
               "I will check the source change.",
               "Added answer() returning 42 and cargo check passed.", "You are welcome!"]

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(request)
                index = len(requests)
                text, tool = replies[index - 1], None
                if index == 5:
                    tool = dict(name="write_file", arguments=dict(path="src/lib.rs", content="pub fn answer() -> u8 { 42 }\n"))
                elif index == 6:
                    tool = dict(name="shell_command", arguments=dict(command="cargo check --offline", timeout_secs=30))
                if provider == "omlx":
                    delta = dict(content=text)
                    if tool:
                        delta["tool_calls"] = [dict(index=0, id=f"tool-{index}", type="function",
                                                    function=dict(name=tool["name"], arguments=json.dumps(tool["arguments"])))]
                    body = "data: " + json.dumps(dict(choices=[dict(delta=delta, finish_reason="tool_calls" if tool else "stop")])) + "\n\ndata: [DONE]\n\n"
                else:
                    message = dict(role="assistant", content=text)
                    if tool:
                        message["tool_calls"] = [dict(function=tool)]
                    body = json.dumps(dict(message=message, done=False)) + "\n" + json.dumps(dict(message=dict(role="assistant", content=""), done=True, done_reason="stop")) + "\n"
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream" if provider == "omlx" else "application/json")
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
    # The generated fixture must build inside its own project, independently
    # of the harness's optional shared Cargo cache directory.
    env.pop("CARGO_TARGET_DIR", None)
    env.pop("NO_COLOR", None)
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 200, 0, 0))
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
        assert needle in data, (needle, data[-7000:], errors)
        end = data.index(needle) + len(needle)
        pending = data[end:]
        return data[:end]

    def exchange(prompt, answer):
        os.write(master, b"\x1b[200~" + prompt.encode() + b"\x1b[201~\r")
        displayed = wait_for(answer.encode())
        displayed += wait_for(b"Ready for your next message")
        session_file, = (state / "microgentic/runs").glob("session-*/conversation.json")
        assert json.loads(session_file.read_text())["last_error"] is None
        return displayed

    try:
        wait_for(b"Enter send")
        assert not requests, "opening a TUI assumed a task"
        greeting = exchange("Hello!", replies[0])
        assert b"Evidence:" not in greeting, "ordinary chat received a work footer"
        assert len(requests) == 1, "a natural reply was forced to continue"
        assert requests[0]["messages"][-1]["content"] == "Hello!"
        assert "No engineering task is assumed" in requests[0]["messages"][0]["content"]
        assert "Complete this benchmark task" not in json.dumps(requests[0]["messages"])
        ready = exchange("Are you ready to chat?", replies[2])
        assert b"Evidence:" not in ready, "empty-reply recovery received a work footer"
        assert "Answer the user's message naturally" in requests[2]["messages"][-1]["content"]
        example = "Let's discuss this example, without executing it:\n```sh\ntouch SHOULD_NOT_EXIST\n```"
        discussion = exchange(example, replies[3])
        assert b"Evidence:" not in discussion, "quoted engineering examples triggered a work footer"
        assert requests[3]["messages"][-1]["content"] == example
        assert not (root / "src/lib.rs").exists() and not (root / "SHOULD_NOT_EXIST").exists()
        assert not (root / "target").exists(), "chat ran a validation command"
        for trace_file in (state / "microgentic/runs").glob("run-*/traces/*.jsonl"):
            events = [json.loads(line) for line in trace_file.read_text().splitlines()]
            resolved = next(e["payload"]["resolved"] for e in events if e["kind"] == "agent.contract.resolved")
            assert resolved["probes"] == [], "a conversation example became a required probe"
        work = exchange("Please add src/lib.rs with an answer function returning 42 and check it.", replies[6])
        assert b"Evidence:" in work, "source edits and checks lost their trace footer"
        assert len(requests) == 7
        observed = requests[6]["messages"][-1]
        assert observed["role"] == "tool"
        result = json.loads(observed["content"])
        assert result["success"] and result["status"] == 0
        assert result["validation_probe"] is False, "chat checks acquired benchmark acceptance authority"
        assert (root / "src/lib.rs").read_text() == "pub fn answer() -> u8 { 42 }\n"
        assert (root / "target").exists(), "post-edit validation did not run"
        thanks = exchange("Thanks!", replies[7])
        # Redraws include earlier history. Only the latest reply must lack a footer.
        assert b"Evidence:" not in thanks.rsplit(b"You: Thanks!", 1)[-1], "work footer leaked into a later chat reply"
        assert len(requests) == 8
        assert requests[7]["messages"][-1]["content"] == "Thanks!"
        assert "previous request stopped" not in json.dumps(requests[7]["messages"])
        traces = list((state / "microgentic/runs").glob("run-*/traces/*.jsonl"))
        assert len(traces) == 5, "hiding chat footers disabled trace retention"
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
