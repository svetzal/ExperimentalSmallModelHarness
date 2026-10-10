"""Admission helper for this experiment's owned pre-forward proxy only.

The harness supplies safe failure metadata plus the captured request digest.
The helper rejects everything without an exact no-dispatch receipt. A real
provider failure, cancellation or residency observation is not such a receipt.
"""

import json
import sys
import uuid
from pathlib import Path
from urllib.parse import urlsplit
from urllib.request import ProxyHandler, Request, build_opener


def decide(context, directory):
    failure = context.get("failure", {})
    identity = failure.get("identity", {})
    request_id = failure.get("provider_request_id")
    if not isinstance(request_id, str):
        return {"decision": "reject"}
    try:
        request_id = str(uuid.UUID(request_id))
        attempt_id = str(uuid.UUID(identity["attempt_id"]))
        logical_id = str(uuid.UUID(identity["logical_request_id"]))
    except (ValueError, TypeError, KeyError, AttributeError):
        return {"decision": "reject"}
    if failure.get("http_status") != 503:
        return {"decision": "reject"}
    progress = failure.get("progress", {})
    for kind in ("observed", "delivered"):
        if any(progress.get(kind, {}).values()):
            return {"decision": "reject"}
    digest = context.get("request_sha256")
    if not isinstance(digest, str) or len(digest) != 64:
        return {"decision": "reject"}
    try:
        if str(directory).startswith("http://"):
            origin = urlsplit(directory)
            if (origin.hostname != "127.0.0.1" or origin.username or origin.password
                    or origin.path or origin.query or origin.fragment):
                return {"decision": "reject"}
            request = Request(directory + "/recovery-receipts/" + request_id)
            with build_opener(ProxyHandler({})).open(request, timeout=5) as response:
                proof = json.load(response)
        else:
            proof = json.loads((Path(directory) / (request_id + ".json")).read_text())
    except (OSError, ValueError):
        return {"decision": "reject"}
    if not (proof.get("receipt_id") == request_id
            and proof.get("request_sha256") == digest
            and proof.get("upstream_dispatched") is False
            and proof.get("evidence_kind") == "not_forwarded"
            and proof.get("status") == 503):
        return {"decision": "reject"}
    return {"decision": "allow", "evidence_kind": "not_forwarded",
            "previous_attempt_id": attempt_id,
            "logical_request_id": logical_id,
            "provider_request_id": request_id, "request_sha256": digest}


if __name__ == "__main__":
    try:
        context = json.load(sys.stdin)
        result = decide(context, sys.argv[1])
    except (ValueError, IndexError, TypeError):
        result = {"decision": "reject"}
    print(json.dumps(result))
