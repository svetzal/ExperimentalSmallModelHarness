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
/home/svetzal/.foundry/operations/harness-key-boundary-evidence-20261010/
```

`proof.json` mirrors `.foundry/proof.json`; `logs/` contains full command stdout
and stderr. `release-matrix-final/` records the actual executable identity, fixture
fingerprint and one `assertions.json` per provider/case, with body, trace,
capture
and renderer paths. Earlier failing fixture runs are retained separately.
Contaminated synthetic
capture bodies are replaced with a withholding marker and their original SHA-256.
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

## Credential correction and reproduction

The recovered production source from `55a76a1` already withholds the coordinator's
partial oMLX tool-argument echo: its capture is nonexact and credential-redacted.
The original coordinator fixture stops at an outdated terminal assertion because
Mojentic reports `observer_failed` at the contaminated HTTP boundary. Repeating
with that outcome accepted passes the original capture-redaction assertion.
The provisional counterexample and both repeat logs remain in the evidence directory.

A separate public reproduction against that exact recovered release is RED:
reasoning prefix, harmless content, reasoning suffix succeeds and reconstructs a
UUID-shaped active credential in reasoning and an exact capture. The correction
tracks each semantic channel and each tool index/field independently, plus
cross-field echoes. Unrelated content cannot release a held reasoning prefix.
Pending text stays ordered until the originating channel disproves its prefix
or safe completion releases it; interruption discards the held suffix.
Mojentic's typed `ToolFragment` events are checked without waiting for completed
calls. JSON string decoding serves credential verification only.

Eight new public cases run through both providers:
`credential_reasoning_interleaved`, `credential_content_interleaved`,
`credential_tool_arguments`, `credential_tool_name`, `credential_tool_id`,
`credential_tool_completed`, `credential_tool_escaped`, and
`credential_tool_mixed`. Ollama uses object arguments; oMLX uses argument-string
deltas, including unfinished JSON and escaped string values. Tests first prove
the scripted body reconstructs the synthetic echo, then reconstruct individual
semantic channels, tool fields, decoded argument values and raw captures. They
require nonexact credential-redacted captures, one explicit failure, accurate
observed prefix/tool progress, no tool execution and no admission or retry.
HTTP-boundary contamination retains Mojentic's `observer_failed` terminal;
semantic cancellation can report `cancelled` or `interrupted`. Library observations
and failure identities are retained without manufacturing unseen fragment counts.

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

## Decoded argument key/colon regression

Privacy checks scan full assembled decoded strings, including unfinished JSON
and incomplete escape suffixes. Opening quote offsets identify strings across
snapshots, so a key awaiting its colon cannot supply a value's prior length.
Only new decoded characters enter the existing cross-field prefix guards;
repeated full snapshots are never concatenated as deltas. This guard does not
interpret completion or retry semantics.

Five additional cases run for each provider: escaped credential contamination
at the key/colon boundary in truncated, completed, and escape-tail streams, plus
ordinary argument controls in truncated and completed streams. Ollama uses its
native argument objects with an HTTP key/colon split; oMLX uses argument-string
deltas, including an unfinished Unicode escape. Contamination cannot retry or
execute tools, captures are withheld and nonexact, and progress, terminal state
and wire accounting remain visible. Controls retain exact captures and completed
controls retain native tool results and history.

## Validation and truthful limits

Final validation on October 10, 2026:

- All four required gates passed unchanged.
- Tests: 357 passed, two existing tests ignored; standalone doctests passed.
- Documentation generation and standalone doctests passed.
- Unfiltered security audit passed: 289 dependencies scanned.
- Release fixture: 102 provider cases and seven configuration refusal checks passed.
- `cargo deny check`: license policy failed; advisories, bans and sources passed.

Final release worker SHA-256:

```text
c851e5c0b49910a7078f1e548d2fae68eeba328295960e1d1ac4e71c2a25898c
```

The exact executable is preserved at `worker/adaptive-agent-harness` inside the
owned evidence directory. `release-assertions.json` indexes every final scripted
assertion. Gate results, full logs and source fingerprints are in `proof.json`.

 Required unchanged gates are formatting, strict locked all-target and
all-feature Clippy/tests, and locked release bins build. Documentation, standalone
doctests and unfiltered security audit are recorded alongside those gates.

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

## Bounded live inference evidence, October 10, 2026

The independently reviewed integration landed as
`8739f6dadc24df9264d8a909aac27df72d23f2be`. The live worker source revision was
`2c0e9e973d3038cf15a053d1859ebf2e7197271e`; following commits changed only the
cancellation fixture synchronization. Mojentic is pinned to reviewed Rust
`4ca1ed279c02eab37827a1ed07c30e961155ecf3`, without requiring publication.
The canonical ops-01 release SHA-256 was
`973c5a55f57b388bea04523c0948e8cdac978673fe6badb56c001bbef031209c`.

All unchanged required gates and the unfiltered security audit passed. Three
release-worker repetitions passed 306 provider cases and 21 configuration
refusals across both adapters. Six actual launch combinations passed scripted
endpoints, and 16 experiment-adapter safety tests passed. The existing optional
license-policy gap remains unsuppressed.

The fresh immutable experiment directories are
`Experiments/TransientRecovery-2026-10-10/` and
`Experiments/TransientRecovery-2026-10-10-Live02/` in the Sandbox2 workspace.
The first preserves a zero-dispatch contract-mount failure. The replacement
registration preserved the worker, workload and bounds, correcting the mount
layout after actual-launch validation. Full notes, registrations, lifecycle
traces, safe exact captures, Git workspaces, binary patches and native combined
transcripts remain in those directories.

Real Ollama 0.40.2 inference passed the disabled baseline (one wire attempt,
one logical interaction, 27.550 seconds) and controlled pre-forward HTTP503
recovery (two attempts, one interaction, 19.858 seconds). Retry payloads were
identical, the controlled failure did not dispatch upstream generation, and
both cells passed cleanup and independent evidence audits. Runtime includes
cleanup and artifact generation and is not pure inference latency.

The next cell stopped: the model first completed `list_tree` in response to
actual harness guidance. The fault adapter treated any tool result as its
trigger and required a shell receipt, closing before forwarding when the
receipt did not exist. The worker rejected recovery without request-bound
admission proof. This is an adapter failure, not a natural provider failure.
Its exact second packet retained the completed inspection result and reasoning.
No shell executed, no oMLX live cells ran, and no further inference or model
unload followed the unresolved receipt boundary. The stopped registration is
not resumed. Real shell-history recovery and oMLX live recovery remain
unvalidated; exactly-once tools on both paths have repeated scripted evidence.

Recovery remains opt-in, disabled by default. A future fresh live cell must
trigger only after the intended shell receipt and account for harmless
inspection under its predeclared interaction bounds. MXFP8 versus MLX affine
8-bit and retained caches preclude comparative efficacy claims. These results
do not identify Generation45's original 504 cause or establish conformance
across all six Mojentic libraries. All historical stopped benchmarks remain
unchanged.

## Completed receipt-bound live evidence

The fresh `Experiments/TransientRecovery-2026-10-10-Live03/` registration completed
six of six real-worker cells on October 10, 2026. The exact registered Harness
revision was `aecaded0dc6f7dca8b2c5387b704bda09938be45`, Mojentic Rust remained
`4ca1ed279c02eab37827a1ed07c30e961155ecf3`, and the canonical ops-01 worker hash
remained `973c5a55f57b388bea04523c0948e8cdac978673fe6badb56c001bbef031209c`.
Adapter corrections are preserved in `fixtures/recovery/adapters/`; compiled
worker behavior and opt-in defaults are unchanged.

The adapter binds its trigger to the intended successful completed shell call,
its exact command/result and independently observed receipt bytes. Inspection
results do not trigger injection. It supports Ollama's native tool binding
without IDs and oMLX's explicit call IDs. Invalid intended receipts produce an
owned permanent refusal before forwarding. Nineteen adapter safety tests and
three repetitions of all six actual-worker scripted launch cells passed before
registration. All unchanged required repository gates and unfiltered security
audit passed; the optional existing license-policy gap remains unsuppressed.

Both providers passed disabled baseline, controlled pre-forward HTTP503 recovery,
and tool-history recovery. Each history task completed one inspection, one shell
write and final `DONE`: three logical requests, four wire attempts, one shell
execution. Its failed and retried completion bodies were identical, retaining
the completed tool result and native reasoning. Across all six cells, ten
logical requests used fourteen attempts; four controlled failures were never
forwarded. All six terminal and unload checks passed. Every exact request was
correlated with snapshots/ledgers and safe wire captures; Git patches matched
and reconstructed full workspaces. The combined artifact uses native
`render-transcript`.

Measured end-to-end seconds for baseline/pre-forward/history were
24.505/16.754/22.112 on Ollama and 31.517/17.211/101.997 on oMLX. These include
launch, cleanup and artifact generation. They are single observations, with
retained caches and MXFP8 versus MLX affine 8-bit, not comparative speed or
coding-efficacy estimates. Both used Qwen3.8 27B, low effort, retained native
reasoning, four logical interactions and at most three attempts per request.
No healthy-generation wall-clock cutoff was introduced.

This supersedes only the previous live-validation gap. The prior stopped
registrations remain unchanged; a read-only source/request audit established
that their adapter exception preceded upstream connection construction.
Controlled recovery does not explain Generation45's original 504 or establish
six-library parity. Real mid-generation interruption and general provider
termination proof remain limits: uncertain local ownership still refuses
further dispatch. Recovery remains disabled by default.

## Empty semantic fragment correction (October 11, 2026)

Generation50's interrupted second trial exposed a detector false positive. An
empty content delta copied a reasoning credential prefix into the content
channel. That stale prefix survived a reasoning mismatch and later combined
with an unrelated reasoning fragment. Offline replay with the dummy Ollama key
matched the worker's interruption at 90583 consumed reasoning bytes. Historical
experiment inputs and results remain frozen.

`CredentialFilter::push` now ignores empty fragments before inspecting or
changing channel state. Nonempty fragments still pass through the existing
per-channel, chronological, cross-field and decoded-tool credential checks.
No credential exemption, retry change or capture-policy relaxation was added.

`empty_content_does_not_retain_a_disproved_reasoning_prefix` reproduces the
failure using both provider frame formats; it failed before the correction.
`empty_fragments_do_not_hide_real_split_credentials` proves that real split
credentials still fail across reasoning, content, tool arguments and channels.
The public-worker `empty_content_prefix_control` case checks both Ollama and
oMLX: one request, successful DONE, preserved reasoning, exact captures and no
retry. These tests use scripted loopback providers, not model inference.

Validation: 369 tests passed with two existing ignored tests, including the
104-case public-worker matrix and seven CLI refusals. Formatting, strict
all-target/all-feature Clippy, locked release bins build and the unfiltered
289-dependency security audit passed. The release worker also passed the new
control on both providers. Doctests passed (zero examples); documentation built
with four existing private-link warnings. Markdown lint is clean. No live
inference or historical experiment changes were made.
