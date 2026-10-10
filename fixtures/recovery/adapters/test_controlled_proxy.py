import http.client
import hashlib
import json
import socket
import struct
import tempfile
import threading
import unittest
import uuid
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from urllib.parse import urlsplit

from controlled_proxy import ControlledProxy, CredentialGuard, RECEIPT_COMMAND
from admission_receipt import decide


class ProxyTests(unittest.TestCase):
    def test_credential_split_across_reasoning_and_content_is_detected(self):
        credential = str(uuid.uuid4())
        guard = CredentialGuard(credential.encode())
        first = json.dumps({"message": {"thinking": credential[:18]}}).encode() + b"\n"
        last = json.dumps({"message": {"content": credential[18:]}}).encode() + b"\n"
        self.assertFalse(guard.inspect_chunk(first))
        self.assertTrue(guard.inspect_chunk(last))

    def test_interleaved_other_channel_does_not_hide_reasoning_credential(self):
        credential = str(uuid.uuid4())
        guard = CredentialGuard(credential.encode())
        for fragment in ({"message": {"thinking": credential[:18]}},
                         {"message": {"content": "harmless interleaving"}}):
            self.assertFalse(guard.inspect_chunk(json.dumps(fragment).encode() + b"\n"))
        self.assertTrue(guard.inspect_chunk(json.dumps(
            {"message": {"thinking": credential[18:]}}).encode() + b"\n"))

    def test_credential_split_across_tool_argument_deltas_is_detected(self):
        credential = str(uuid.uuid4())
        guard = CredentialGuard(credential.encode())
        for index, part in enumerate((credential[:18], credential[18:])):
            value = {"choices": [{"delta": {"tool_calls": [{"index": 0,
                     "function": {"arguments": part}}]}}]}
            line = b"data: " + json.dumps(value).encode() + b"\n\n"
            self.assertEqual(guard.inspect_chunk(line), index == 1)

    def close_fixture(self, proxy):
        self.assertTrue(proxy.lock.acquire(timeout=5), "fixture request did not finish")
        proxy.lock.release()
        proxy.close()

    def request(self, endpoint, body):
        url = urlsplit(endpoint)
        connection = http.client.HTTPConnection(url.hostname, url.port, timeout=5)
        connection.request("POST", "/api/chat", body,
                           {"Content-Type": "application/json"})
        response = connection.getresponse()
        result = response.status, response.getheader("X-Request-ID"), response.read()
        connection.close()
        return result

    def upstream(self, response_body):
        seen = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                seen.append(self.rfile.read(int(self.headers["Content-Length"])))
                self.send_response(200)
                self.send_header("Content-Length", str(len(response_body)))
                self.end_headers()
                self.wfile.write(response_body)

        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(thread.join)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return "http://127.0.0.1:%d" % server.server_port, seen

    def test_controlled_failure_does_not_dispatch_and_retry_payload_is_exact(self):
        upstream, seen = self.upstream(b'{"message":{"content":"DONE"},"done":true}\n')
        with tempfile.TemporaryDirectory() as temp:
            proxy = ControlledProxy(upstream, Path(temp) / "capture", inject="first")
            endpoint = proxy.start()
            try:
                payload = b'{"messages":[{"role":"user","content":"Finish"}],"stream":true}'
                status, receipt, body = self.request(endpoint, payload)
                self.assertEqual(status, 503)
                self.assertEqual(seen, [])
                proof = json.loads((proxy.evidence / (receipt + ".json")).read_text())
                self.assertFalse(proof["upstream_dispatched"])
                self.assertEqual(proof["evidence_kind"], "not_forwarded")
                self.assertIn(b"controlled", body)
                context = {"request_sha256": hashlib.sha256(payload).hexdigest(),
                           "failure": {"http_status": 503,
                                       "provider_request_id": receipt,
                                       "identity": {"attempt_id": str(uuid.uuid4()),
                                                    "logical_request_id": str(uuid.uuid4())},
                                       "progress": {"observed": {}, "delivered": {}}}}
                self.assertEqual(decide(context, endpoint)["decision"], "allow")
                self.assertEqual(self.request(endpoint, payload)[0], 200)
                self.assertEqual(seen, [payload])
                self.assertFalse(proxy.unresolved)
                self.assertEqual((proxy.evidence / "wire-001/request.json").read_bytes(),
                                 (proxy.evidence / "wire-002/request.json").read_bytes())
            finally:
                self.close_fixture(proxy)

    def test_incomplete_upstream_closes_further_dispatch(self):
        upstream, seen = self.upstream(b'{"message":{"thinking":"Partial"},"done":false}\n')
        with tempfile.TemporaryDirectory() as temp:
            proxy = ControlledProxy(upstream, Path(temp) / "capture")
            endpoint = proxy.start()
            try:
                payload = b'{"messages":[],"stream":true}'
                self.assertEqual(self.request(endpoint, payload)[0], 200)
                self.assertTrue(proxy.unresolved)
                self.assertEqual(self.request(endpoint, payload)[0], 409)
                self.assertEqual(len(seen), 1)
            finally:
                self.close_fixture(proxy)

    def target_payload(self, provider="ollama"):
        function = {"name": "shell_command", "arguments": {"command": RECEIPT_COMMAND}}
        result = json.dumps({"command": RECEIPT_COMMAND, "success": True,
                             "status": 0, "stdout": "RECOVERY_RECEIPT\n"})
        if provider == "ollama":
            return {"messages": [{"role": "tool", "content": result,
                    "tool_calls": [{"function": function}]}]}
        return {"messages": [{"role": "assistant", "tool_calls": [
                    {"id": "receipt", "function": function}]},
                    {"role": "tool", "tool_call_id": "receipt", "content": result}]}

    def test_inspection_result_passes_without_receipt_and_target_injects_once(self):
        upstream, seen = self.upstream(b'{"message":{"content":"DONE"},"done":true}\n')
        with tempfile.TemporaryDirectory() as temp:
            receipt = Path(temp) / "receipt.txt"
            proxy = ControlledProxy(upstream, Path(temp) / "capture",
                                    inject="after-tool", receipt_path=receipt)
            endpoint = proxy.start()
            try:
                inspection = json.dumps({"messages": [{"role": "tool",
                    "content": '{"entries":[]}', "tool_calls": [{"function":
                    {"name": "list_tree", "arguments": {}}}]}]}).encode()
                self.assertEqual(self.request(endpoint, inspection)[0], 200)
                self.assertEqual(len(seen), 1)
                self.assertFalse(proxy.injected)
                receipt.write_bytes(b"RECOVERY_RECEIPT\n")
                target = json.dumps(self.target_payload()).encode()
                self.assertEqual(self.request(endpoint, target)[0], 503)
                self.assertEqual(len(seen), 1)
                self.assertEqual(self.request(endpoint, target)[0], 200)
                self.assertEqual(len(seen), 2)
                self.assertFalse(proxy.unresolved)
            finally:
                self.close_fixture(proxy)

    def test_target_validation_requires_exact_independent_receipt(self):
        with tempfile.TemporaryDirectory() as temp:
            receipt = Path(temp) / "receipt.txt"
            proxy = ControlledProxy("http://127.0.0.1:1", Path(temp) / "capture",
                                    inject="after-tool", receipt_path=receipt)
            payload = self.target_payload()
            with self.assertRaises(RuntimeError): proxy.should_inject(payload)
            receipt.write_bytes(b"RECOVERY_RECEIPT\nRECOVERY_RECEIPT\n")
            with self.assertRaises(RuntimeError): proxy.should_inject(payload)
            receipt.write_bytes(b"RECOVERY_RECEIPT\n")
            self.assertTrue(proxy.should_inject(payload))
            self.assertTrue(proxy.should_inject(self.target_payload("omlx")))

    def test_omlx_unmatched_tool_id_cannot_trigger(self):
        with tempfile.TemporaryDirectory() as temp:
            receipt = Path(temp) / "receipt.txt"; receipt.write_bytes(b"RECOVERY_RECEIPT\n")
            proxy = ControlledProxy("http://127.0.0.1:1", Path(temp) / "capture",
                                    inject="after-tool", receipt_path=receipt)
            payload = self.target_payload("omlx")
            payload["messages"][1]["tool_call_id"] = "unrelated"
            self.assertFalse(proxy.should_inject(payload))

    def test_missing_target_receipt_persists_non_dispatch_failure(self):
        upstream, seen = self.upstream(b'{"done":true}\n')
        with tempfile.TemporaryDirectory() as temp:
            proxy = ControlledProxy(upstream, Path(temp) / "capture", inject="after-tool",
                                    receipt_path=Path(temp) / "missing")
            try:
                status, _, _ = self.request(proxy.start(), json.dumps(self.target_payload()).encode())
                self.assertEqual(status, 400); self.assertEqual(seen, [])
                proof = json.loads((proxy.evidence / "wire-001/outcome.json").read_text())
                self.assertFalse(proof["upstream_dispatched"])
                self.assertEqual(proof["evidence_kind"], "not_forwarded")
                self.assertTrue(proxy.unresolved)
            finally: self.close_fixture(proxy)

    def test_echoed_credentials_are_never_persisted_and_close_admission(self):
        credential = str(uuid.uuid4()).encode()
        body = b'{"message":{"content":"' + credential + b'"},"done":true}\n'
        upstream, _seen = self.upstream(body)
        with tempfile.TemporaryDirectory() as temp:
            proxy = ControlledProxy(upstream, Path(temp) / "capture",
                                    authorization="Bearer " + credential.decode())
            endpoint = proxy.start()
            try:
                status, _receipt, observed = self.request(endpoint, b'{"messages":[]}')
                self.assertEqual(status, 200)
                self.assertNotIn(credential, observed)
                self.assertTrue(proxy.lock.acquire(timeout=5))
                proxy.lock.release()
                self.assertTrue(proxy.unresolved)
                for artifact in proxy.evidence.rglob("*"):
                    if artifact.is_file():
                        self.assertNotIn(credential, artifact.read_bytes())
                outcome = json.loads((proxy.evidence / "wire-001/outcome.json").read_text())
                self.assertTrue(outcome["credentials_redacted"])
                self.assertFalse(outcome["protocol_complete"])
            finally:
                self.close_fixture(proxy)

    def test_semantic_delta_credential_echo_is_not_reconstructible_from_captures(self):
        credential = str(uuid.uuid4())
        frames = [json.dumps({"message": {"content": part}, "done": False}).encode() + b"\n"
                  for part in (credential[:18], credential[18:])]
        upstream, _seen = self.upstream(b"".join(frames) + b'{"done":true}\n')
        with tempfile.TemporaryDirectory() as temp:
            proxy = ControlledProxy(upstream, Path(temp) / "capture",
                                    authorization="Bearer " + credential)
            try:
                status, _receipt, observed = self.request(proxy.start(), b'{"messages":[]}')
                self.assertEqual(status, 200)
                self.assertTrue(proxy.lock.acquire(timeout=5))
                proxy.lock.release()
                self.assertTrue(proxy.unresolved)
                reconstructed = "".join(json.loads(line).get("message", {}).get("content", "")
                                        for line in observed.splitlines())
                self.assertNotIn(credential, reconstructed)
                self.assertEqual((proxy.evidence / "wire-001/response.bin").read_bytes(),
                                 b"[credential-contaminated capture withheld]")
                outcome = json.loads((proxy.evidence / "wire-001/outcome.json").read_text())
                self.assertTrue(outcome["credentials_redacted"])
                self.assertFalse(outcome["protocol_complete"])
            finally:
                self.close_fixture(proxy)

    def test_overlapping_requests_are_rejected_before_upstream_dispatch(self):
        entered = threading.Event()
        release = threading.Event()
        seen = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                seen.append(self.rfile.read(int(self.headers["Content-Length"])))
                entered.set()
                if not release.wait(5):
                    return
                body = b'{"message":{"content":"DONE"},"done":true}\n'
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        server = HTTPServer(("127.0.0.1", 0), Handler)
        serving = threading.Thread(target=server.serve_forever, daemon=True)
        serving.start()
        with tempfile.TemporaryDirectory() as temp:
            proxy = ControlledProxy("http://127.0.0.1:%d" % server.server_port,
                                    Path(temp) / "capture")
            endpoint = proxy.start()
            completed = []
            request = threading.Thread(target=lambda: completed.append(
                self.request(endpoint, b'{"messages":[],"stream":true}')))
            request.start()
            try:
                self.assertTrue(entered.wait(3))
                self.assertEqual(self.request(endpoint, b'{"messages":[]}')[0], 409)
                self.assertTrue(proxy.unresolved)
                release.set()
                request.join(5)
                self.assertFalse(request.is_alive())
                self.assertEqual(len(seen), 1)
                self.assertEqual(completed[0][0], 200)
            finally:
                release.set()
                request.join(5)
                self.close_fixture(proxy)
                server.shutdown()
                server.server_close()
                serving.join()

    def test_credential_bearing_request_is_rejected_without_persistence_or_dispatch(self):
        credential = str(uuid.uuid4()).encode()
        upstream, seen = self.upstream(b'{"done":true}\n')
        with tempfile.TemporaryDirectory() as temp:
            proxy = ControlledProxy(upstream, Path(temp) / "capture",
                                    authorization="Bearer " + credential.decode())
            endpoint = proxy.start()
            try:
                payload = b'{"messages":[{"content":"' + credential + b'"}]}'
                self.assertEqual(self.request(endpoint, payload)[0], 400)
                self.assertEqual(seen, [])
                self.assertTrue(proxy.unresolved)
                for artifact in proxy.evidence.rglob("*"):
                    if artifact.is_file():
                        self.assertNotIn(credential, artifact.read_bytes())
            finally:
                self.close_fixture(proxy)

    def test_disconnected_client_does_not_substitute_for_owned_protocol_completion(self):
        release = threading.Event()
        final_body = b'{"done":false}\n' * 1024 + b'{"done":true}\n'
        seen = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                seen.append(self.rfile.read(int(self.headers["Content-Length"])))
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b'{"done":false}\n')
                self.wfile.flush()
                if release.wait(5):
                    self.wfile.write(final_body)
                    self.wfile.flush()

        server = HTTPServer(("127.0.0.1", 0), Handler)
        serving = threading.Thread(target=server.serve_forever, daemon=True)
        serving.start()
        with tempfile.TemporaryDirectory() as temp:
            proxy = ControlledProxy("http://127.0.0.1:%d" % server.server_port,
                                    Path(temp) / "capture")
            endpoint = urlsplit(proxy.start())
            client = socket.create_connection((endpoint.hostname, endpoint.port), timeout=5)
            try:
                body = b'{"messages":[]}'
                client.sendall(b'POST /api/chat HTTP/1.1\r\nHost: localhost\r\nContent-Length: '
                               + str(len(body)).encode() + b'\r\n\r\n' + body)
                received = b""
                while b'{"done":false}\n' not in received:
                    received += client.recv(4096)
                client.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
                client.close()
                self.assertTrue(proxy.lock.locked())
                self.assertFalse(proxy.unresolved)
                release.set()
                self.assertTrue(proxy.lock.acquire(timeout=5))
                proxy.lock.release()
                outcome = json.loads((proxy.evidence / "wire-001/outcome.json").read_text())
                self.assertTrue(outcome["protocol_complete"])
                self.assertTrue(outcome["client_disconnected"])
                self.assertFalse(proxy.unresolved)
                self.assertEqual(len(seen), 1)
                self.assertEqual((proxy.evidence / "wire-001/response.bin").read_bytes(),
                                 b'{"done":false}\n' + final_body)
            finally:
                client.close()
                release.set()
                self.close_fixture(proxy)
                server.shutdown()
                server.server_close()
                serving.join()


if __name__ == "__main__":
    unittest.main()
