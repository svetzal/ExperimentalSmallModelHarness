#!/usr/bin/env python3
"""Exercise a supplied frozen Linux worker using scripted loopback HTTP only."""
import argparse
import datetime
import email.utils
import hashlib
import http.server
import json
import os
from pathlib import Path
import signal
import re
import subprocess
import threading
import time
import uuid


def encoded(value):
    return json.dumps(value, separators=(",", ":")).encode()


def frame(provider, field=None, text=None, done=False, tools=None):
    if provider == "ollama":
        message = {}
        if field:
            message["thinking" if field == "reasoning" else "content"] = text
        if tools:
            message["tool_calls"] = tools
        value = {"message": message, "done": done}
        if done:
            value.update(done_reason="stop", prompt_eval_count=11, eval_count=7,
                         eval_duration=1000000000, total_duration=2000000000)
        return encoded(value) + b"\n"
    delta = {}
    if field:
        delta["reasoning_content" if field == "reasoning" else "content"] = text
    if tools:
        delta["tool_calls"] = tools
    return b"data: " + encoded({"choices": [{"delta": delta,
        "finish_reason": "stop" if done else None}]}) + b"\n\n" + (b"data: [DONE]\n\n" if done else b"")


def success(provider):
    return (frame(provider) + frame(provider, "reasoning", "Synthetic reasoning.")
            + frame(provider, "content", "DONE") + frame(provider, done=True))


# The synthetic helper is a fixture for binding checks, NOT provider termination support.
HELPER = '''#!/usr/bin/env python3
import json,sys,time,os
from pathlib import Path
x=json.load(sys.stdin)
root=Path(__file__).parent
(root / "admission-stdin.json").write_text(json.dumps(x))
(root / "admission-pid.json").write_text(json.dumps({"pid":os.getpid()}))
mode=MODE
if mode == "pending": time.sleep(30)
i=x["failure"]["identity"]
y={"decision":"allow", "evidence_kind":"not_forwarded",
   "previous_attempt_id":i["attempt_id"], "logical_request_id":i["logical_request_id"],
   "provider_request_id":x["failure"]["provider_request_id"], "request_sha256":x["request_sha256"]}
if mode == "reject": y["decision"]="reject"
if mode == "plain": y={"decision":"allow"}
if mode == "wrong": y["request_sha256"]="0"*64
(root / "admission-proof.json").write_text(json.dumps(y))
print(json.dumps(y))
'''


CREDENTIAL_REGRESSIONS = ["credential_reasoning_interleaved", "credential_content_interleaved",
    "credential_tool_arguments", "credential_tool_name", "credential_tool_id",
    "credential_tool_completed", "credential_tool_escaped", "credential_tool_mixed"]


def credential_chunks(provider, name, secret):
    if name.endswith("interleaved"):
        field = "reasoning" if "reasoning" in name else "content"
        other = "content" if field == "reasoning" else "reasoning"
        return [frame(provider, field, secret[:12]), frame(provider, other, "Harmless unrelated text."),
                frame(provider, field, secret[12:]), frame(provider, done=True)]
    field = "name" if name.endswith("name") else "id" if name.endswith("id") else "arguments"
    chunks = []
    if name.endswith("mixed"):
        chunks.append(frame(provider, "reasoning", secret[:12]))
        pieces = [secret[12:]]
    else:
        pieces = [secret[:12], secret[12:]]
    for piece in pieces:
        call = {"index": 0, "id": "call_partial", "type": "function",
                "function": {"name": "shell_command", "arguments": {"command": piece}}}
        if provider == "omlx":
            call["function"]["arguments"] = encoded({"command": piece}).decode() if len(pieces) == 1 else (
                '{"command":"' + piece if piece == pieces[0] else piece + '"}')
        if field in ("name", "id"):
            call["function"]["arguments"] = {"command": "true"} if provider == "ollama" else "{}"
            if field == "name": call["function"]["name"] = piece
            if field == "id": call["id"] = piece
        escaped = "".join("\\u%04x" % ord(c) for c in piece)
        if name.endswith("escaped") and provider == "omlx":
            call["function"]["arguments"] = call["function"]["arguments"].replace(piece, escaped)
        chunk = frame(provider, tools=[call])
        if name.endswith("escaped") and provider == "ollama":
            chunk = chunk.replace(piece.encode(), escaped.encode())
        chunks.append(chunk)
        # A different semantic channel must not release a held tool-field prefix.
        if piece == pieces[0]: chunks.append(frame(provider, "content", "Harmless unrelated text."))
    if name.endswith("completed"): chunks.append(frame(provider, done=True))
    return chunks


def reconstructed_channels(body):
    channels = {}
    chronological = []
    def visit(value, path=""):
        if isinstance(value, list):
            for index, item in enumerate(value): visit(item, path + "/" + str(index))
        elif isinstance(value, dict):
            for key, item in value.items():
                visit(item, path + "/" + key)
        elif isinstance(value, str):
            channels[path] = channels.get(path, "") + value
            chronological.append(value)
            try: nested = json.loads(value)
            except (ValueError, TypeError): return
            if isinstance(nested, (dict, list)): visit(nested, path + "/decoded")
    for line in body.splitlines():
        try: visit(json.loads(line.removeprefix(b"data:").strip()))
        except (ValueError, TypeError): pass
    reconstructed = list(channels.values())
    semantic_fields = [text for path, text in channels.items() if
        path.endswith(("/content", "/thinking", "/reasoning_content", "/reasoning"))
        or "/tool_calls/" in path and (path.endswith(("/id", "/name")) or "/function/arguments/" in path)]
    for path, text in list(channels.items()):
        try: value = json.loads(text)
        except (ValueError, TypeError): continue
        if isinstance(value, (dict, list)):
            def strings(value):
                if isinstance(value, str): return [value]
                if isinstance(value, dict): return [text for item in value.values() for text in strings(item)]
                if isinstance(value, list): return [text for item in value for text in strings(item)]
                return []
            values = strings(value)
            reconstructed.extend(values)
            if path.endswith("/arguments"):
                semantic_fields.extend(values)
    reconstructed.extend(left + right for index, left in enumerate(semantic_fields)
                         for other, right in enumerate(semantic_fields) if index != other)
    return reconstructed + ["".join(chronological)]


def run_case(worker, artifacts, provider, name):
    root = artifacts / provider / name
    root.mkdir(parents=True, exist_ok=False)
    workspace = root / "workspace"
    workspace.mkdir()
    (root / "task.md").write_text("Finish with DONE.\n")
    capture = root / "capture"
    helper = root / "admission"
    mode = "pending" if name in ("pending", "cancel_admission") else (
        "reject" if name == "reject" else "plain" if name == "plain_allow" else
        "wrong" if name == "wrong_digest" else "allow")
    helper.write_text(HELPER.replace("MODE", repr(mode)))
    helper.chmod(0o700)
    secret = str(uuid.uuid4())
    arbitrary_secret = "synthetic-" + str(uuid.uuid4()) + '"/unsafe'
    requests, responses, failures = [], [], []
    terminal_sent = threading.Event()
    received = threading.Event()
    stop = threading.Event()
    status = 503
    expected_count = 2
    expected_success = True
    expected_outcome = None
    retry_after = None
    partial = None
    enabled = name != "disabled"
    if name == "tool_once":
        expected_count = 3
    if name == "stream_drop_cap":
        status, expected_count = 200, 1
    if name in ("disabled", "one_attempt_capture"):
        expected_count, expected_success, expected_outcome = 1, False, "exhausted"
    if name == "persistent504":
        status, expected_count, expected_success, expected_outcome = 504, 3, False, "exhausted"
    if name in ("reject", "plain_allow", "wrong_digest", "pending", "no_admission"):
        expected_count, expected_success = 1, False
        expected_outcome = "admission_required" if name == "no_admission" else "admission_rejected"
    if name.startswith("permanent"):
        status = int(name.split("_")[1])
        expected_count, expected_success, expected_outcome = 1, False, "failed"
    if name == "protocol":
        status, expected_count, expected_success, expected_outcome = 200, 1, False, "failed"
    if name.startswith("partial_"):
        partial = name.split("_")[1]
        status, expected_count, expected_success, expected_outcome = 200, 1, False, "interrupted"
    if name.startswith("cancel_"):
        expected_count, expected_success, expected_outcome = 1, False, "cancelled"
        if name in ("cancel_request", "cancel_terminal", "cancel_terminal_body"):
            status = 200
    if name.startswith("retry_"):
        status = 429
        if "date" in name:
            retry_after = email.utils.format_datetime(datetime.datetime.now(datetime.timezone.utc)
                + datetime.timedelta(seconds=3 if "inside" in name else 10), usegmt=True)
        else:
            retry_after = "1" if "inside" in name else "10"
        if "outside" in name:
            expected_count, expected_success = 1, False
            expected_outcome = "budget_expired" if "budget" in name else "delay_ceiling"
    if name == "keepalive":
        status = 200  # An empty/nonsemantic body truncated before a terminal frame.
    if name in CREDENTIAL_REGRESSIONS or name in ("credential_header", "credential_raw", "credential_semantic", "credential_error", "capture_failure"):
        expected_count, expected_success = 1, False
        expected_outcome = "admission_rejected" if name == "credential_header" else "observer_failed"
        if name not in ("credential_header", "credential_error"):
            status = 200

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def do_POST(self):
            try:
                assert self.path == ("/api/chat" if provider == "ollama" else "/v1/chat/completions")
                if provider == "omlx":
                    assert self.headers.get("Authorization") == "Bearer " + secret
                request = self.rfile.read(int(self.headers["Content-Length"]))
                requests.append(request)
                index = len(requests)
                (root / f"request-{index}.body").write_bytes(request)
                first = (index == 2 if name == "tool_once" else index == 1) or name == "persistent504"
                response_status = status if first else 200
                body = b'{"error":"synthetic failure"}' if response_status != 200 else success(provider)
                if name == "tool_once" and index == 1:
                    args = {"command": "printf 'once\\n' >> effects.txt; printf synthetic-result"}
                    tool = {"function": {"name": "shell_command", "arguments": args}}
                    if provider == "omlx":
                        tool = {"index": 0, "id": "call_once", "type": "function", "function": {"name": "shell_command", "arguments": json.dumps(args)}}
                        final = b"data: " + encoded({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}) + b"\n\ndata: [DONE]\n\n"
                    else:
                        final = frame(provider, done=True)
                    body = frame(provider, "reasoning", "Native reasoning before tool.") + frame(provider, tools=[tool]) + final
                chunks = [body]
                truncate = name.startswith("permanent") and "truncated" in name and first
                if first and name == "protocol":
                    body = b"malformed-json\n" if provider == "ollama" else b"data: malformed-json\n\n"
                    chunks = [body]
                if first and (partial or name in ("keepalive", "capture_failure")):
                    body = frame(provider, partial, "partial") if partial in ("reasoning", "content") else frame(provider)
                    if partial == "tool":
                        tool = {"index": 0, "id": "call_partial", "type": "function",
                                "function": {"name": "shell_command", "arguments": '{"command":'}}
                        if provider == "ollama":
                            tool = {"function": {"name": "shell_command", "arguments": {"command": "touch forbidden"}}}
                        body = frame(provider, tools=[tool])
                    if name == "capture_failure":
                        body = frame(provider, "content", "observed")
                        chunks = [body, frame(provider, "content", "next")]
                    else:
                        chunks = [body]
                    truncate = True
                if first and name == "credential_raw":
                    body = frame(provider, "content", secret) + frame(provider, done=True)
                    offset = body.index(secret.encode()) + 13
                    chunks = [body[:offset], body[offset:]]
                if first and name == "credential_error":
                    body = encoded({"error": "arbitrary unsafe body: " + arbitrary_secret})
                    chunks = [body[:len(body)//2], body[len(body)//2:]]
                if first and name == "credential_semantic":
                    chunks = [frame(provider, "reasoning", secret[:12]),
                              frame(provider, "content", secret[12:24]),
                              frame(provider, "reasoning", secret[24:]), frame(provider, done=True)]
                    body = b"".join(chunks)
                if first and name in CREDENTIAL_REGRESSIONS:
                    chunks = credential_chunks(provider, name, secret)
                    body = b"".join(chunks)
                    assert any(secret in text for text in reconstructed_channels(body)), "credential fixture cannot reconstruct its echo"
                    truncate = "tool" in name and not name.endswith("completed")
                if first and name in ("cancel_terminal", "cancel_terminal_body"):
                    chunks = [frame(provider, "content", "DONE"), frame(provider, done=True)]
                    body = b"".join(chunks)
                if first and name == "stream_drop_cap":
                    chunks = [frame(provider, "reasoning", "Synthetic reasoning beyond explicit cap."), frame(provider, done=True)]
                    body = b"".join(chunks)
                # Persist safe scripted bytes. Contaminated responses are intentionally withheld.
                recorded = b"[credential-contaminated synthetic body withheld]" if name in CREDENTIAL_REGRESSIONS or name in ("credential_raw", "credential_semantic", "credential_error") else body
                (root / f"response-{index}.body").write_bytes(recorded)
                responses.append(recorded)
                self.send_response(response_status)
                self.send_header("Content-Type", "application/x-ndjson" if provider == "ollama" else "text/event-stream")
                if retry_after:
                    self.send_header("Retry-After", retry_after)
                # Arbitrary unsafe header plus a UUID-shaped credential echo.
                self.send_header("X-Unsafe", secret)
                if name == "credential_header":
                    self.send_header("X-Request-ID", secret)
                else:
                    self.send_header("X-Request-ID", "11111111-1111-4111-8111-111111111111")
                self.send_header("Content-Length", str(sum(map(len, chunks)) + (100 if truncate else 0)))
                self.end_headers()
                received.set()
                if first and name == "cancel_request":
                    stop.wait(10)
                    return
                for n, chunk in enumerate(chunks):
                    if n and name in ("cancel_terminal", "stream_drop_cap"):
                        stop.wait(10)
                    if n and name == "cancel_terminal_body":
                        deadline = time.monotonic() + 5
                        while time.monotonic() < deadline:
                            traces = list((root / "traces").glob("*.jsonl"))
                            if traces and "agent.stream.chunk" in traces[0].read_text(): break
                            time.sleep(.01)
                        else: raise AssertionError("first semantic delta not consumed")
                        pending = next(capture.glob("*.pending.json"))
                        pending.unlink()
                        os.mkfifo(pending, 0o600)
                    if n and name == "capture_failure":
                        time.sleep(.15)
                        for child in capture.glob("*"):
                            child.unlink()
                        if capture.exists(): capture.rmdir()
                        capture.write_text("synthetic capture failure")
                    self.wfile.write(chunk)
                    self.wfile.flush()
                    if n and name == "cancel_terminal_body": terminal_sent.set()
                    if name in CREDENTIAL_REGRESSIONS or name in ("credential_raw", "credential_semantic", "credential_error", "capture_failure", "cancel_terminal"):
                        time.sleep(.1)
                self.close_connection = True
            except (BrokenPipeError, ConnectionResetError):
                pass  # Expected cancellation or fail-closed observer.
            except Exception as error:
                failures.append(type(error).__name__)
                self.close_connection = True

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    environment = {key: os.environ[key] for key in ("PATH", "HOME", "TMPDIR") if key in os.environ}
    environment[provider.upper() + "_HOST"] = f"http://127.0.0.1:{server.server_port}"
    # Also active for Ollama, to test safe persistence consistently without real credentials.
    environment["OMLX_API_KEY"] = secret
    environment["OPENAI_API_KEY"] = arbitrary_secret
    command = [str(worker), "run", "--experiment", str(root), "--provider", provider,
               "--model", "synthetic-model", "--max-iterations", "1",
               "--max-model-interactions", "2" if name == "tool_once" else "1", "--reasoning-effort", "enabled",
               "--preserve-reasoning", "--max-tokens", "8192"]
    if enabled and name != "one_attempt_capture":
        command += ["--recover-requests", "--recovery-max-attempts", "3" if name == "persistent504" else "2",
                    "--recovery-base-delay-ms", "0", "--recovery-delay-ceiling-ms", "5000",
                    "--recovery-budget-ms", "8000"]
        if name != "no_admission":
            command += ["--recovery-admission-command", str(helper)]
    if name not in ("disabled", "capture_failure"):
        command += ["--recovery-capture-dir", str(capture)]
    if name == "capture_failure":
        command += ["--recovery-capture-dir", str(capture)]
    if name == "stream_drop_cap":
        command += ["--max-thinking-only-tokens", "1"]
    if name.startswith("permanent"):
        command += ["--recovery-retryable-status", str(status)]
    if name.startswith("retry_") and "outside" in name:
        option = "--recovery-budget-ms" if "budget" in name else "--recovery-delay-ceiling-ms"
        command[command.index(option) + 1] = "100"
        if "budget" in name:
            command[command.index("--recovery-delay-ceiling-ms") + 1] = "20000"
    if name == "cancel_backoff":
        # Retry-After gives a deterministic cancellation window.
        retry_after = "3"
    proc = subprocess.Popen(command, cwd=workspace, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        if name.startswith("cancel_"):
            assert received.wait(5), "worker never dispatched"
            if name in ("cancel_admission", "cancel_backoff"):
                wanted = "admission_pending" if name == "cancel_admission" else "delay_scheduled"
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    traces = list((root / "traces").glob("*.jsonl"))
                    if traces and wanted in traces[0].read_text() and (name != "cancel_admission" or (root / "admission-pid.json").exists()): break
                    time.sleep(.01)
                else: raise AssertionError("cancellation phase not reached")
            elif name == "cancel_terminal":
                time.sleep(.2)
            elif name == "cancel_terminal_body":
                assert terminal_sent.wait(5), "terminal-only chunk not sent"
                time.sleep(.1)  # HTTP observer is blocked opening the owned FIFO.
            proc.send_signal(signal.SIGTERM)
            if name == "cancel_terminal": stop.set()
            if name == "cancel_terminal_body":
                time.sleep(.1)  # Let the runtime observe SIGTERM before releasing the wire hook.
                pending = next(capture.glob("*.pending.json"))
                descriptor = os.open(pending, os.O_RDONLY | os.O_NONBLOCK)
                try:
                    deadline = time.monotonic() + 2
                    while time.monotonic() < deadline:
                        try:
                            if os.read(descriptor, 4096): break
                        except BlockingIOError: pass
                        time.sleep(.01)
                    else: raise AssertionError("terminal body capture hook was not reached")
                finally:
                    os.close(descriptor)
        stdout, stderr = proc.communicate(timeout=12)
    finally:
        stop.set()
        if proc.poll() is None:
            proc.kill()
            proc.wait()
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
    (root / "stdout.json").write_bytes(stdout)
    (root / "stderr.log").write_bytes(stderr)
    assert not failures, (provider, name, failures)
    assert len(requests) == expected_count, (provider, name, len(requests), expected_count, stderr.decode())
    assert (proc.returncode == 0) == expected_success, (provider, name, proc.returncode, stderr.decode())
    retried = requests[1:] if name == "tool_once" else requests
    assert all(request == retried[0] for request in retried), "retry bytes changed"
    request = json.loads(requests[0])
    assert request["tools"], "production worker tools missing"
    if provider == "ollama":
        assert request["think"] is True
        assert request["options"]["num_predict"] == 8192
    else:
        assert request["max_tokens"] == 8192
        assert request["enable_thinking"] is True
        assert request["chat_template_kwargs"]["preserve_thinking"] is True
    traces = list((root / "traces").glob("*.jsonl"))
    assert len(traces) == 1
    events = [json.loads(line) for line in traces[0].read_text().splitlines()]
    started_run = next(e["payload"] for e in events if e["kind"] == "run.started")
    assert started_run["recovery"]["recover_requests"] == (enabled and name != "one_attempt_capture")
    assert started_run["recovery"]["recovery_max_attempts"] == (3 if name == "persistent504" else 2 if enabled and name != "one_attempt_capture" else 1)
    accounting = [e["payload"] for e in events if e["kind"] == "llm.recovery.accounting"]
    assert accounting == [{"wire_attempts": expected_count}], (name, accounting)
    lifecycle = [e["payload"] for e in events if e["kind"] == "llm.recovery.lifecycle"]
    starts = [e for e in lifecycle if e["transition"] == "attempt_started"]
    assert len(starts) == expected_count
    ids = [e["identity"] for e in starts]
    assert len({i["attempt_id"] for i in ids}) == expected_count
    assert len({i["logical_request_id"] for i in ids}) == (2 if name == "tool_once" else 1)
    assert [i["wire_attempt"] for i in ids] == ([1, 1, 2] if name == "tool_once" else list(range(1, expected_count + 1)))
    if name == "tool_once":
        assert ids[1]["logical_request_id"] == ids[2]["logical_request_id"]
        assert (workspace / "effects.txt").read_text() == "once\n"
        messages = json.loads(requests[1])["messages"]
        assert any(m.get("role") == "tool" and "synthetic-result" in m.get("content", "") for m in messages)
        assert any(m.get("role") == "assistant" and "Native reasoning before tool." in (m.get("thinking", "") + m.get("reasoning_content", "")) for m in messages), messages
    admission_input = root / "admission-stdin.json"
    if admission_input.exists():
        admission = json.loads(admission_input.read_text())
        previous = admission["failure"]["identity"]
        previous_index = next(n for n, identity in enumerate(ids) if identity["attempt_id"] == previous["attempt_id"])
        assert admission["request_sha256"] == hashlib.sha256(requests[previous_index]).hexdigest()
        assert set(admission) == {"failure", "request_sha256"}
        assert admission["failure"]["progress"]["observed"] == {"reasoning_bytes":0, "content_bytes":0, "tool_fragments":0, "completed_tool_calls":0}
    if name == "credential_header":
        assert not admission_input.exists(), "redacted identifying metadata reached admission"
        assert any(e.get("identifying_metadata_redacted") for e in lifecycle), "missing identifying redaction evidence"
    if name in ("pending", "cancel_admission"):
        child_pid = json.loads((root / "admission-pid.json").read_text())["pid"]
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            try: os.kill(child_pid, 0)
            except ProcessLookupError: break
            time.sleep(.01)
        else: raise AssertionError("owned admission child survived cancellation/timeout")
    if not expected_success:
        final = [e["payload"] for e in events if e["kind"] == "llm.recovery.final_failure"]
        assert len(final) == 1, (provider, name, final)
        actual = final[0].get("error", {}).get("outcome", final[0].get("outcome"))
        if name in CREDENTIAL_REGRESSIONS or name == "credential_semantic":
            assert actual in ("cancelled", "interrupted", "observer_failed"), (name, actual)
        else:
            assert actual == expected_outcome, (provider, name, actual, expected_outcome)
        if partial or name == "capture_failure":
            progress = final[0]["error"]["failure"]["progress"]
            assert any(progress["observed"].values()), (name, progress)
        if name.startswith("permanent"):
            assert final[0]["error"]["failure"]["http_status"] == status
            assert final[0]["error"]["failure"]["classification"] == {"eligible": False, "reason": "permanent"}
        if name.startswith("cancel_"):
            transitions = [e["transition"] for e in lifecycle]
            assert transitions.count("cancelled") == 1
            assert transitions.count("attempt_failed") == 1
            assert transitions.index("attempt_failed") < transitions.index("cancelled")
            assert "attempt_succeeded" not in transitions
            if name == "cancel_terminal_body":
                assert final[0]["error"]["failure"]["progress"]["raw_bytes"] == len(responses[0]), "terminal chunk was not received before cancellation"
        if partial == "tool":
            assert not any(e["kind"].startswith("tool.") for e in events)
    elif name == "stream_drop_cap":
        transitions = [e["transition"] for e in lifecycle]
        assert transitions.count("cancelled") == 1, ("stream-drop terminal", transitions)
        assert transitions.count("attempt_failed") == 1
        assert "attempt_succeeded" not in transitions
        assert any(e["kind"] == "llm.thinking_only_stream.action_transitioned" or "thinking_only_stream" in e["kind"] for e in events)
    else:
        assert json.loads(stdout)["final_summary"].strip() == "DONE"
        assert sum(e["transition"] == "attempt_succeeded" for e in lifecycle) == (2 if name == "tool_once" else 1)
        if provider == "ollama":
            metrics = [e["payload"] for e in events if e["kind"] == "llm.stream.metrics"]
            assert len(metrics) == (2 if name == "tool_once" else 1) and metrics[0]["eval_count"] == 7
            assert metrics[0]["prompt_eval_count"] == 11 and metrics[0]["tokens_per_second"] == 7.0
        depths = [e["payload"]["llm_call_depth"] for e in events if e["kind"] == "llm.provider_request.assembled"]
        assert depths == ([0, 1] if name == "tool_once" else [0]), depths
    rendered = root / "transcript.html"
    render = subprocess.run([str(worker), "render-transcript", str(traces[0]), "--output", str(rendered)],
                            env=environment, capture_output=True, timeout=5)
    assert render.returncode == 0
    assert b"Request recovery" in rendered.read_bytes()
    match = re.search(r'<script id="transcriptData" type="application/json">(.*?)</script>', rendered.read_text(), re.S)
    rendered_data = json.loads(match.group(1))
    for session in rendered_data["sessions"]:
        rendered_semantic = "".join(call["reasoning"] + call["assistant"] for call in session["calls"])
        assert secret not in rendered_semantic
        for field in ("reasoning", "assistant"):
            assert secret not in "".join(call[field] for call in session["calls"])
    if partial in ("content", "reasoning"):
        assert "partial" in rendered_semantic
    # Scan all final artifacts, plus text concatenation that removes delta boundaries.
    for path in root.rglob("*"):
        if path.is_file():
            assert secret.encode() not in path.read_bytes(), (name, "credential leaked", path)
            assert arbitrary_secret.encode() not in path.read_bytes(), (name, "escaped credential leaked", path)
            for reconstructed in reconstructed_channels(path.read_bytes()):
                assert secret not in reconstructed, (name, "reconstructed channel credential leaked", path)
    for kind in ("agent.stream.chunk", "llm.stream.thinking"):
        text = "".join(e["payload"].get("text", e["payload"].get("preview", "")) for e in events if e["kind"] == kind)
        assert secret not in text
    semantic = "".join(e["payload"].get("text", e["payload"].get("preview", "")) for e in events if e["kind"] in ("agent.stream.chunk", "llm.stream.thinking"))
    assert secret not in semantic
    if name in CREDENTIAL_REGRESSIONS:
        assert not any(e["kind"].startswith("tool.") for e in events), "contaminated tool executed"
        assert not admission_input.exists(), "credential contamination retried"
        observed = [e.get("progress", {}).get("observed", {}) for e in lifecycle]
        assert any(any(progress.values()) for progress in observed), "lost observed progress"
        if "tool" in name and not name.endswith("mixed"):
            assert any(progress.get("tool_fragments", 0) > 0 for progress in observed), "lost observed tool fragment"
        elif name.endswith("interleaved"):
            field = "reasoning_bytes" if "reasoning" in name else "content_bytes"
            assert any(progress.get(field, 0) >= 12 for progress in observed), "lost credential-prefix observation"
    captures = list(capture.glob("*.json")) if capture.is_dir() else []
    completed_captures = [json.loads(p.read_text()) for p in captures if not p.name.endswith(".pending.json")]
    if name not in ("disabled", "capture_failure"):
        assert len(completed_captures) == expected_count
        if name in CREDENTIAL_REGRESSIONS or name in ("credential_raw", "credential_semantic", "credential_error"):
            assert all(c["credential_redacted"] and not c["exact"] for c in completed_captures)
        else:
            assert all(c["exact"] for c in completed_captures)
            for identity in ids:
                key = identity["attempt_id"]
                assert (capture / (key + ".request.body")).read_bytes() == requests[ids.index(identity)]
                captured = (capture / (key + ".response.body")).read_bytes()
                if name.startswith("cancel_") or name == "stream_drop_cap":
                    assert responses[ids.index(identity)].startswith(captured)
                    attempt_events = [e for e in lifecycle if e.get("identity", {}).get("attempt_id") == key]
                    assert len(captured) == attempt_events[-1]["progress"]["raw_bytes"]
                else:
                    assert captured == responses[ids.index(identity)]
    assertions = {"test": name, "provider": provider, "passed": True, "wire_attempts": expected_count,
                  "identical_retry_bytes": True, "stable_logical_id": True, "distinct_attempt_ids": True,
                  "native_reasoning_and_settings": True, "credential_safe_trace_capture_renderer": True,
                  "outcome": expected_outcome if not expected_success else "success",
                  "trace": str(traces[0]), "renderer": str(rendered), "capture": str(capture),
                  "requests": [str(root / f"request-{i+1}.body") for i in range(len(requests))],
                  "responses": [str(root / f"response-{i+1}.body") for i in range(len(responses))]}
    (root / "assertions.json").write_text(json.dumps(assertions, indent=2) + "\n")
    print(json.dumps(assertions), flush=True)


CASES = ["503_success", "disabled", "one_attempt_capture", "persistent504", "reject", "plain_allow",
         "wrong_digest", "pending", "no_admission", "retry_seconds_inside", "retry_date_inside",
         "retry_seconds_outside_ceiling", "retry_date_outside_ceiling", "retry_seconds_outside_budget",
         "retry_date_outside_budget", "cancel_request", "cancel_admission", "cancel_backoff", "cancel_terminal", "cancel_terminal_body",
         "partial_reasoning", "partial_content", "partial_tool", "keepalive", "protocol",
         "permanent_400", "permanent_401", "permanent_403", "permanent_400_truncated",
         "permanent_401_truncated", "permanent_403_truncated", "credential_header", "credential_raw",
         "credential_semantic", "credential_error", "capture_failure", "tool_once", "stream_drop_cap"] + CREDENTIAL_REGRESSIONS


def validate_cli(worker, artifacts):
    root = artifacts / "cli-validation"
    root.mkdir()
    workspace = root / "workspace"
    workspace.mkdir()
    (root / "task.md").write_text("Finish with DONE.\n")
    owned = workspace / "untrusted-helper"
    owned.write_text("#!/bin/sh\nexit 0\n")
    owned.chmod(0o700)
    reached = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_args): pass
        def do_POST(self):
            reached.set()
            self.send_error(400)

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    environment = {key: os.environ[key] for key in ("PATH", "HOME", "TMPDIR") if key in os.environ}
    environment["OLLAMA_HOST"] = f"http://127.0.0.1:{server.server_port}"
    scenarios = {
        "disabled_tuning": ["--recovery-max-attempts", "2"],
        "zero_attempts": ["--recover-requests", "--recovery-max-attempts", "0"],
        "zero_budget": ["--recover-requests", "--recovery-budget-ms", "0"],
        "delay_order": ["--recover-requests", "--recovery-base-delay-ms", "2", "--recovery-delay-ceiling-ms", "1"],
        "tool_admission": ["--recover-requests", "--recovery-admission-command", str(owned)],
        "tool_capture": ["--recovery-capture-dir", str(workspace / "capture")],
        "parent_capture": ["--recovery-capture-dir", str(root / "new" / ".." / "workspace" / "capture")],
    }
    try:
        for name, flags in scenarios.items():
            result = subprocess.run([str(worker), "run", "--experiment", str(root), "--provider", "ollama"] + flags,
                                    cwd=workspace, env=environment, capture_output=True, timeout=5)
            assert result.returncode != 0, name
            assert not reached.is_set(), "invalid settings dispatched"
            (root / (name + ".stderr.log")).write_bytes(result.stderr)
        assert not (root / "traces").exists(), "invalid configuration created dispatch state"
        help_result = subprocess.run([str(worker), "run", "--help"], env=environment, capture_output=True, timeout=5)
        assert help_result.returncode == 0
        assert b"Wire attempts" in help_result.stdout and b"Examples:" in help_result.stdout
        (root / "help.txt").write_bytes(help_result.stdout)
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
    assertions = {"test":"cli_recovery_validation_before_dispatch", "passed":True,
                  "cases":list(scenarios), "wire_requests":0, "artifacts":str(root)}
    (root / "assertions.json").write_text(json.dumps(assertions, indent=2) + "\n")
    print(json.dumps(assertions), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker", type=Path, required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--case", choices=CASES, action="append")
    parser.add_argument("--provider", choices=("ollama", "omlx"), action="append")
    args = parser.parse_args()
    worker = args.worker.resolve(strict=True)
    artifacts = args.artifacts.resolve()
    artifacts.mkdir(parents=True, exist_ok=False)
    identity = {"worker": str(worker), "worker_sha256": hashlib.sha256(worker.read_bytes()).hexdigest(),
                "fixture_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
    (artifacts / "worker.json").write_text(json.dumps(identity, indent=2) + "\n")
    validate_cli(worker, artifacts)
    for provider in args.provider or ("ollama", "omlx"):
        for name in args.case or CASES:
            run_case(worker, artifacts, provider, name)


if __name__ == "__main__":
    main()
