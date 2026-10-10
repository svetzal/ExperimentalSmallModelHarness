"""Single-owner, pre-forward fault injection; this module never retries HTTP.

The coordinator supplies an upstream endpoint and a fresh evidence directory.
Credentials may be supplied in memory; headers are never persisted. A receipt
proves non-dispatch only for this owned proxy's deliberately injected response.
Real upstream failures and incomplete streams establish no termination proof.
"""

import hashlib
import http.client
import json
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit


def save_json(path, value):
    with Path(path).open("x", encoding="utf-8") as output:
        json.dump(value, output, indent=2)
        output.write("\n")
        output.flush()


class CredentialGuard:
    """Detect known credentials across JSON string deltas; never transform output."""

    def __init__(self, secret):
        self.secret = secret.decode() if secret else ""
        self.tails = {}
        self.semantic_tail = ""
        self.pending = bytearray()

    def inspect_value(self, value, path=()):
        if not self.secret:
            return False
        if isinstance(value, str):
            joined = self.tails.get(path, "") + value
            self.tails[path] = joined[-max(len(self.secret) - 1, 1):]
            return self.secret in joined
        if isinstance(value, dict):
            if any(self.secret in key for key in value):
                return True
            # Reasoning precedes visible text at the library boundary. An echo
            # can cross these channels even when JSON paths differ. This is a
            # privacy check only; it does not interpret completion or retry.
            for key in ("thinking", "reasoning_content", "reasoning", "content", "arguments"):
                fragment = value.get(key)
                if isinstance(fragment, str):
                    joined = self.semantic_tail + fragment
                    self.semantic_tail = joined[-max(len(self.secret) - 1, 1):]
                    if self.secret in joined:
                        return True
            return any(self.inspect_value(item, path + (key,)) for key, item in value.items())
        if isinstance(value, list):
            return any(self.inspect_value(item, path + (index,)) for index, item in enumerate(value))
        return False

    def inspect_chunk(self, chunk):
        self.pending.extend(chunk)
        while b"\n" in self.pending:
            line, _, remainder = self.pending.partition(b"\n")
            self.pending = bytearray(remainder)
            line = line.strip()
            if line.startswith(b"data:"):
                line = line[5:].strip()
            try:
                if self.inspect_value(json.loads(line)):
                    return True
            except (ValueError, UnicodeError):
                pass
        return False

    def inspect_final(self):
        try:
            return self.inspect_value(json.loads(self.pending))
        except (ValueError, UnicodeError):
            return False


RECEIPT_COMMAND = "printf '%s\\n' RECOVERY_RECEIPT >> receipt.txt; cat receipt.txt"

class ControlledProxy:
    def __init__(self, upstream, evidence, inject="none", receipt_path=None,
                 authorization=None, max_requests=18):
        self.upstream = urlsplit(upstream)
        if self.upstream.scheme not in ("http", "https"):
            raise ValueError("HTTP upstream required")
        if self.upstream.username or self.upstream.password or self.upstream.query:
            raise ValueError("Credentials and queries forbidden in upstream URL")
        self.evidence = Path(evidence)
        self.evidence.mkdir(parents=True, exist_ok=False)
        self.inject = inject
        self.receipt_path = Path(receipt_path) if receipt_path else None
        self.authorization = authorization
        self.max_requests = max_requests
        self.injected = False
        self.unresolved = False
        self.count = 0
        self.lock = threading.Lock()
        self.server = None
        self.thread = None

    def should_inject(self, payload):
        if self.injected or self.inject == "none":
            return False
        if self.inject == "first":
            return True
        if self.inject != "after-tool":
            raise ValueError("Unknown injection mode")
        completed_target = False
        calls = {}
        for message in payload.get("messages", []):
            if message.get("role") == "assistant":
                for call in message.get("tool_calls", []):
                    calls[call.get("id")] = call.get("function", {})
            if message.get("role") != "tool":
                continue
            attached = [call.get("function", {}) for call in message.get("tool_calls", [])]
            tool_id = message.get("tool_call_id")
            if tool_id is not None and tool_id in calls:
                attached.append(calls[tool_id])
            try:
                result = json.loads(message.get("content", ""))
            except (ValueError, TypeError):
                continue
            if not isinstance(result, dict):
                continue
            for function in attached:
                arguments = function.get("arguments", {})
                if isinstance(arguments, str):
                    try:
                        arguments = json.loads(arguments)
                    except ValueError:
                        continue
                if (function.get("name") == "shell_command"
                        and isinstance(arguments, dict)
                        and arguments.get("command") == RECEIPT_COMMAND
                        and result.get("command") == RECEIPT_COMMAND
                        and result.get("success") is True
                        and result.get("status") == 0
                        and result.get("stdout", "").splitlines() == ["RECOVERY_RECEIPT"]):
                    completed_target = True
        if not completed_target:
            return False
        if not self.receipt_path or not self.receipt_path.exists():
            raise RuntimeError("Completed target tool without independent receipt")
        if self.receipt_path.read_bytes() != b"RECOVERY_RECEIPT\n":
            raise RuntimeError("Expected exactly one completed receipt write")
        return True

    def start(self):
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args):
                pass

            def reply(self, code, body, receipt=None):
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
                if receipt:
                    self.send_header("X-Request-ID", receipt)
                self.end_headers()
                self.wfile.write(body)
                self.wfile.flush()
                self.close_connection = True

            def do_GET(self):
                # Safe proofs only, for a helper reached through an owned SSH
                # loopback tunnel. This endpoint never dispatches inference.
                prefix = "/recovery-receipts/"
                if not self.path.startswith(prefix):
                    self.reply(404, b'{}')
                    return
                try:
                    receipt = str(uuid.UUID(self.path[len(prefix):]))
                    body = (owner.evidence / (receipt + ".json")).read_bytes()
                except (ValueError, OSError):
                    self.reply(404, b'{}')
                    return
                self.reply(200, body)

            def do_POST(self):
                if not owner.lock.acquire(blocking=False):
                    owner.unresolved = True
                    self.reply(409, b'{"error":"overlapping dispatch rejected"}')
                    return
                try:
                    self.handle_owned_request()
                except Exception:
                    # Exception strings can contain URLs or credentials. The
                    # caller gets only a fixed safe error and must stop dispatch.
                    owner.unresolved = True
                    self.close_connection = True
                finally:
                    owner.lock.release()

            def handle_owned_request(self):
                if owner.unresolved or owner.count >= owner.max_requests:
                    self.reply(409, b'{"error":"dispatch admission closed"}')
                    return
                if self.path not in ("/api/chat", "/v1/chat/completions"):
                    self.reply(404, b'{"error":"completion route required"}')
                    return
                length = int(self.headers.get("Content-Length", "0"))
                if length <= 0 or length > 4_000_000:
                    self.reply(400, b'{"error":"bounded JSON payload required"}')
                    return
                body = self.rfile.read(length)
                if len(body) != length:
                    owner.unresolved = True
                    return
                payload = json.loads(body)
                owner.count += 1
                cell = owner.evidence / ("wire-%03d" % owner.count)
                cell.mkdir()
                secret = (owner.authorization.split(" ", 1)[-1].encode()
                          if owner.authorization else b"")
                if secret and (secret in body or CredentialGuard(secret).inspect_value(payload)):
                    owner.unresolved = True
                    (cell / "request.redacted.bin").write_bytes(
                        b"[credential-contaminated request withheld]")
                    save_json(cell / "outcome.json", {
                        "status": 400, "upstream_dispatched": False,
                        "credentials_redacted": True, "protocol_complete": False})
                    self.reply(400, b'{"error":"credential-bearing request rejected"}')
                    return
                (cell / "request.json").write_bytes(body)
                digest = hashlib.sha256(body).hexdigest()
                started = time.time()
                try:
                    inject_now = owner.should_inject(payload)
                except RuntimeError:
                    owner.unresolved = True
                    response = b'{"error":"target receipt validation failed"}'
                    receipt = str(uuid.uuid4())
                    (cell / "response.bin").write_bytes(response)
                    save_json(cell / "outcome.json", {
                        "receipt_id": receipt, "request_sha256": digest,
                        "status": 400, "upstream_dispatched": False,
                        "evidence_kind": "not_forwarded", "wire_index": owner.count,
                        "reason": "target_receipt_validation_failed",
                        "started_at": started, "finished_at": time.time()})
                    self.reply(400, response, receipt)
                    return
                if inject_now:
                    owner.injected = True
                    receipt = str(uuid.uuid4())
                    response = b'{"error":"controlled pre-forward transient"}'
                    (cell / "response.bin").write_bytes(response)
                    proof = {"receipt_id": receipt, "request_sha256": digest,
                             "status": 503, "upstream_dispatched": False,
                             "evidence_kind": "not_forwarded",
                             "wire_index": owner.count, "started_at": started,
                             "finished_at": time.time(), "route": self.path}
                    save_json(cell / "outcome.json", proof)
                    save_json(owner.evidence / (receipt + ".json"), proof)
                    self.reply(503, response, receipt)
                    return
                connection_class = (http.client.HTTPSConnection
                                    if owner.upstream.scheme == "https"
                                    else http.client.HTTPConnection)
                # No healthy-generation wall-clock cutoff. The inference owner
                # and registered execution budget are authoritative.
                connection = connection_class(owner.upstream.hostname,
                                              owner.upstream.port, timeout=None)
                headers = {"Content-Type": "application/json"}
                if owner.authorization:
                    headers["Authorization"] = owner.authorization
                complete = False
                status = None
                captured = bytearray()
                redacted = False
                pending_output = bytearray()
                client_closed = False
                credential_guard = CredentialGuard(secret)
                try:
                    connection.request("POST", self.path, body=body, headers=headers)
                    response = connection.getresponse()
                    status = response.status
                    self.send_response(status)
                    self.send_header("Content-Type", response.getheader(
                        "Content-Type", "application/json"))
                    self.send_header("Connection", "close")
                    self.end_headers()
                    terminator_seen = False
                    tail = b""
                    while True:
                        chunk = response.read1(8192)
                        if not chunk:
                            break
                        if len(captured) + len(chunk) > 4_000_000:
                            raise RuntimeError("Registered response capture bound exceeded")
                        captured.extend(chunk)
                        pending_output.extend(chunk)
                        if secret and (secret in pending_output or credential_guard.inspect_chunk(chunk)):
                            redacted = True
                            raise RuntimeError("Credential echoed by upstream")
                        tail = (tail + chunk)[-16384:]
                        if self.path == "/api/chat":
                            for line in tail.splitlines():
                                try:
                                    terminator_seen |= json.loads(line).get("done") is True
                                except (ValueError, AttributeError):
                                    pass
                        else:
                            terminator_seen |= any(line.strip() == b"data: [DONE]"
                                                   for line in tail.splitlines())
                        # Provider parsers also consume complete lines. Validate
                        # strings before forwarding a line, including credentials
                        # split into separate JSON deltas or escaped on wire.
                        ready = (pending_output.rfind(b"\n") + 1 if secret
                                 else len(pending_output))
                        if ready > 0:
                            if not client_closed:
                                try:
                                    self.wfile.write(pending_output[:ready])
                                    self.wfile.flush()
                                except (BrokenPipeError, ConnectionResetError):
                                    # Continue draining only this already-owned
                                    # response. Disconnect is not termination.
                                    client_closed = True
                            del pending_output[:ready]
                    if secret and (secret in pending_output or credential_guard.inspect_final()):
                        redacted = True
                        raise RuntimeError("Credential echoed by upstream")
                    if not client_closed:
                        try:
                            self.wfile.write(pending_output)
                            self.wfile.flush()
                        except (BrokenPipeError, ConnectionResetError):
                            client_closed = True
                    complete = status == 200 and terminator_seen
                finally:
                    connection.close()
                    self.close_connection = True
                    # Buffer before persistence so a credential echoed across
                    # transport chunks cannot leak through an exact capture.
                    if owner.authorization:
                        if secret and secret in captured:
                            redacted = True
                        if redacted:
                            captured = bytearray(b"[credential-contaminated capture withheld]")
                            complete = False
                    (cell / "response.bin").write_bytes(captured)
                    if not complete:
                        owner.unresolved = True
                    save_json(cell / "outcome.json", {
                        "request_sha256": digest, "status": status,
                        "upstream_dispatched": True, "protocol_complete": complete,
                        "termination_proof": False, "credentials_redacted": redacted,
                        "client_disconnected": client_closed,
                        "started_at": started,
                        "finished_at": time.time(), "wire_index": owner.count})

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return "http://127.0.0.1:%d" % self.server.server_address[1]

    def close(self):
        if self.lock.locked():
            raise RuntimeError("Cannot close proxy with unresolved active request")
        if self.server:
            self.server.shutdown()
            self.server.server_close()
            self.thread.join()
