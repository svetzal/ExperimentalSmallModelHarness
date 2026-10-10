# Worker request recovery conformance

## Implementation and evidence identity

The production worker uses reviewed Mojentic
`4ca1ed279c02eab37827a1ed07c30e961155ecf3` through its existing Ollama and oMLX
provider paths. Harness base is
`4854cc8a66ede3dacb0f3d37847ee09b14e16421`. Manifest and lockfile are unchanged.
Foundry owns independent review and Git finalization. The preserved proof
records
source fingerprints and the final release executable hash; it does not claim an
as-yet-unissued reviewed landed Harness revision.

The original prerequisite gap at Mojentic
`bc8150a381a540e6320cd4fd91a8bbecadb9a735` was missing nonsemantic recovery
progress and compatibility metrics. It is resolved by the exact reviewed pin.
`tests/recovery_prerequisites.rs` positively compares legacy, raw recovery and
compatibility progress/metrics, identities and encoded bodies. Those assertions
remain intact. There is no absence diagnostic or substitute provider retry loop.

Evidence lives outside the disposable worktree:

```text
/home/svetzal/.foundry/operations/harness-recovery-production-evidence-20261010/
```

`proof.json` mirrors `.foundry/proof.json`; `logs/` contains full command stdout
and stderr. `release-matrix-final/` records the actual executable identity, fixture
fingerprint and one `assertions.json` per provider/case, with body, trace,
capture
and renderer paths. Earlier failing fixture runs are retained separately.
All HTTP providers are scripted loopback servers. No live model inference or
experiment-directory writes occur in this implementation task.

## Caller settings

Recovery is opt-in and disabled by default. Wire attempts include the initial
request; the default maximum is one even when recovery is enabled. Specify two
or more explicitly to permit resending one immutable encoded completion:

```bash
adaptive-agent-harness run --experiment /work/new-cell --provider ollama \
  --recover-requests --recovery-max-attempts 2 \
  --recovery-base-delay-ms 100 --recovery-delay-ceiling-ms 30000 \
  --recovery-budget-ms 60000 \
  --recovery-admission-command /opt/operator/owned-receipt \
  --recovery-capture-dir /work/private-wire
```

Wire attempts never consume model interactions, reset tool depth, replay a tool
or restart an agent. Messages, native reasoning history, tools and generation
settings remain identical across attempts. Existing reasoning caps, transcript
policy and budgets remain intact. Recovery budgets begin after failure and
never impose total-generation timeouts. Retry-After above the delay ceiling or
remaining budget is refused before dispatch.

Recovery tuning without `--recover-requests`, zero attempts/budget, invalid
delay
ordering and admission/capture paths inside the model tool root are rejected
before HTTP dispatch. `--recovery-retryable-status` optionally selects HTTP
statuses; permanent 400/401/403 remain permanent even when explicitly selected.
One-attempt `--recovery-capture-dir` is permitted without enabling recovery.
Omitting admission rejects ambiguous local resend. SIGINT/SIGTERM cancel
request,
admission or backoff and persist terminal evidence before CLI exit.

## Operator admission schema

The executable is supplied by the trusted operator, canonicalized outside the
model tool root, and invoked directly without a shell. Stdin is JSON:

```json
{
  "failure": {
    "identity": {
      "logical_request_id": "<UUID>",
      "attempt_id": "<UUID>",
      "wire_attempt": 1
    },
    "http_status": 503,
    "provider_request_id": "<validated ID or null>",
    "progress": {
      "observed": {},
      "delivered": {}
    }
  },
  "request_sha256": "<SHA-256 of immutable encoded HTTP body>"
}
```

The full safe Mojentic failure includes classification, phase, Retry-After and
progress counters; private causes, headers and response bodies are excluded.
The coordinator's owned pre-forward receipt helper must verify its receipt
before returning this proof on stdout:

```json
{
  "decision": "allow",
  "evidence_kind": "not_forwarded",
  "previous_attempt_id": "<exact previous UUID>",
  "logical_request_id": "<exact logical UUID>",
  "provider_request_id": "<exact input ID or null>",
  "request_sha256": "<exact input SHA-256>"
}
```

`exact_request_terminated` is the alternative evidence kind for an operator with
proof of exact request termination. Every binding must match. Missing/ambiguous
proof, plain allow, digest mismatch, nonzero exit, malformed or oversized
output,
timeout and credential contamination reject admission. Identifying metadata
removed by credential filtering also rejects admission. The child has a five
second bound, a 16 KiB output limit, discarded stderr and kill-on-drop. Pending
admission never dispatches; cancellation drops and kills only the owned child.
No unrelated model process is killed or unloaded. The fixture's synthetic helper
checks bindings only and is not general provider termination support.

## Assertion-bearing worker matrix

`actual_worker_recovery_matrix_both_adapters` invokes
`fixtures/recovery/worker_conformance.py` with the actual Cargo-built CLI. Each
case runs independently for Ollama and oMLX:

- `503_success`, `persistent504`: Byte-identical attempts, bounded
  success/exhaustion, safe terminal history.

- `retry_seconds_inside`, `retry_date_inside`: Retry-After respected within
  explicit limits.

- `retry_seconds_outside_ceiling`, `retry_date_outside_ceiling`: No resend above
  delay ceiling.

- `retry_seconds_outside_budget`, `retry_date_outside_budget`: No resend beyond
  recovery budget.

- `reject`, `plain_allow`, `wrong_digest`, `no_admission`: Ambiguous or unbound
  proof cannot dispatch.

- `pending`: Finite pending admission, no resend, owned child killed.

- `cancel_request`, `cancel_admission`, `cancel_backoff`: One ordered
  AttemptFailed then Cancelled, no success/resend.

- `cancel_terminal`, `cancel_terminal_body`: Cancellation wins over
  terminal-only body arrival; terminal bytes counted.

- `partial_reasoning`, `partial_content`, `partial_tool`: Explicit interruption,
  accurate observed progress, no completed tool effects.

- `stream_drop_cap`: Existing explicit reasoning cap drops the stream; ordered
  cancellation and exact partial capture survive worker shutdown.

- `keepalive`: Nonsemantic-only truncation can recover without semantic replay.

- `protocol`: Malformed provider protocol remains permanent.

- `permanent_400`, `permanent_401`, `permanent_403`: Permanent with allowing
  admission and explicitly selected status.

- `permanent_400_truncated`, `permanent_401_truncated`,
  `permanent_403_truncated`: Truncated error bodies retain permanent HTTP
  classification.

- `disabled`, `one_attempt_capture`: Default one attempt; capture independently
  permitted without recovery.

- `tool_once`: Tool side effect once, tool result/native reasoning retained,
  depth and interactions preserved.

- `credential_header`: UUID-shaped credential in X-Request-ID removed; admission
  rejected.

- `credential_raw`: Credential split over raw transport chunks withheld;
  nonexact capture.

- `credential_semantic`: Credential split across reasoning/content deltas cannot
  reconstruct in trace or renderer.

- `credential_error`: Arbitrary JSON-escaped credential in a split HTTP error
  body is withheld and never admitted for resend.

- `capture_failure`: Body-capture refusal after semantic bytes retains observed
  progress and never retries.

Common assertions cover production tools and generation settings, exact wire
counts, stable logical IDs, distinct attempt IDs, wire counts per logical
request,
provider metrics, capture coverage and native renderer lifecycle cards. Safe
request/response bodies remain available for repeated coordinator cells;
credential-contaminated synthetic bodies are intentionally withheld. Every case
records emitted artifact paths in its assertion file and full fixture stdout.
`cancel_terminal_body` uses an owned FIFO to pause the HTTP capture hook after
terminal bytes arrive; SIGTERM is observed before that hook is released. This
exercises terminal-only cancellation through the public worker surface.

Functional privacy tests additionally cover cross-kind credential prefixes,
normal UTF-8 payload preservation and metadata key/value redaction. Public API
progress/metrics equivalence remains covered by the prerequisite tests.

## Capture and trace contract

See `TRACE_SCHEMA.md` for the exact main trace and capture fields. Redacted
event envelopes explicitly record redaction and nonexactness.
Lifecycle
contains identities, phase, observed/delivered semantic counters, safe failures,
admission, delay and terminal transitions. Final errors retain structured
history
before the legacy worker returns an error. The renderer shows lifecycle/failure
cards and interrupted content/reasoning previews. `llm.recovery.accounting`
records actual sends separately from interactions and depth; `run.started`
records explicit recovery settings.

Response buffers stay in memory until terminal privacy checks. JSON/SSE framing
is inspected only for credential verification, including unconsumed deltas and
JSON-escaped error strings; Mojentic alone decodes completion semantics and
controls retries. Known active
credentials are filtered from metadata, errors and semantic deltas across chunk
boundaries. Contamination fails closed and withholds the response body, marking
`credential_redacted: true` and `exact: false`. Normal nonsecret payloads and
provider metrics/progress are preserved. Headers and authorization configuration
are never written. If the capture destination fails, the main trace retains
failure/progress and no complete capture is claimed. Library-delivered counters
refer to the library API boundary; privacy filtering may withhold those bytes
from the worker. Pending metadata never claims exact capture.

## Frozen Linux release repetition

Build the ordinary Linux release worker with the pinned dependencies, then let
the coordinator repeat the complete external fixture against that exact binary:

```bash
python3 fixtures/recovery/worker_conformance.py \
  --worker /absolute/path/adaptive-agent-harness \
  --artifacts /absolute/path/new-scripted-cell
```

The output directory must be new and outside all historical experiments. The
fixture clears ambient credentials/proxies, uses synthetic credentials created
only in memory, fingerprints the worker/fixture and records assertions. Optional
`--provider ollama` or `--provider omlx` and repeatable `--case NAME` select
cells.
No extra toolchain target, static build or live dispatch is required. The Mac
coordinator uses the exact normal release worker in a compatible Ubuntu 24.04
Linux container before its separately registered bounded live experiment.

## Validation and truthful limits

Final validation on October 10, 2026:

- All four required gates passed unchanged.
- Tests: 351 passed, two existing tests ignored; standalone doctests passed.
- Documentation and modified Markdown lint passed.
- Unfiltered security audit passed: 289 dependencies scanned.
- Release fixture: 76 provider cases and seven configuration refusal checks passed.
- `cargo deny check`: license policy failed; advisories, bans and sources passed.

Final release worker SHA-256:

```text
5756a423a8042455a82f00c71dcbca2ed679f18a1d401687d9e6f5a9ea49fef7
```

The exact executable is preserved at `worker/adaptive-agent-harness` inside the
owned evidence directory. `release-assertions.json` indexes every final scripted
assertion. Gate results, full logs and source fingerprints are in `proof.json`.

 Required unchanged gates are formatting, strict locked all-target and
all-feature Clippy/tests, and locked release bins build. Documentation, modified
Markdown lint and unfiltered security audit are recorded alongside those gates.

The previous license-policy limitation is retained: `cargo deny check` has no
approved repository license policy and reports license requirements. No license
policy or advisory suppression is added. Audit scope remains unfiltered.

The CLI exposes deliberate stream drop through its existing explicit reasoning
cap; `stream_drop_cap` exercises that path with preserved caps and capture.
Arbitrary paused-consumer control is not exposed and is not claimed as
worker-tested. The owned FIFO tests a paused wire capture hook, not a paused
stream consumer. Shutdown has a five second terminal-evidence drain bound after
the worker returns and drops its streams; this is not a generation timeout.
Structured/advisory completion, embeddings, remote termination efficacy and live
model behavior are not established by this worker streaming matrix. Ollama and
oMLX do not provide proven general request cancellation/status/idempotency;
local
HTTP cancellation alone does not prove remote termination. A trusted helper must
verify owned evidence. Credentials transformed beyond exact known values are
outside this filter's guarantee. Abrupt SIGKILL cannot persist terminal
evidence.

Rust edition 2024, Tokio, dependency/features and formatting style are
preserved.
No MSRV, explicit workspace resolver, toolchain pin or CI workflows are
declared;
an explicit MSRV would help future maintenance. No inference ran and every
historical benchmark remains frozen.
