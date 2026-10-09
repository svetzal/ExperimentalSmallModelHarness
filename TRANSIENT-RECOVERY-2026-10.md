# Transient recovery contract

Status: approved scope from Stacey's October 9, 2026 feature request.
This contract applies to all six maintained Mojentic language ports.
Elixir is the reference implementation. Each port uses idiomatic public APIs.

## Scope and source evidence

Mojentic owns recovery of one provider completion request. The harness owns
experiment policy, resource admission, trace persistence, and session recovery.
Recovery never restarts an agent, replays a broker loop, or executes a tool.

Sandbox2 Generation45 stopped after request 40 returned HTTP 504.
Responses 1 through 39 completed. The cleanup check still found the runner active.
The final response was not captured. The timeout's infrastructure origin is unknown.
The stopped generation remains frozen. A new experiment must test the integration.

## Public error and progress model

Expose these fields as stable types, with optional fields when evidence is absent.
Do not encode HTTP status only inside a message.

| Field | Contract |
| --- | --- |
| Category | `transport`, `http`, `provider_response`, `protocol`, `cancellation`, or `client_timeout` |
| Provider and operation | Provider identity and the completion operation |
| HTTP status | Numeric status, when received |
| Provider code | Validated provider error code, when received |
| Retry-After | Parsed delay or date, with invalid or absent values represented explicitly |
| Provider request ID | Validated provider request ID, when received |
| Phase | `connecting`, `sending`, `awaiting_headers`, `streaming`, or `decoding`; unknown when evidence cannot distinguish phases |
| Acceptance | `yes`, `no`, or `unknown`; an inference request may have been accepted |
| Progress | Headers received, raw bytes observed, delivered reasoning/content, delivered tool fragments, and completed tool calls |
| Classification | Retry eligibility and a stable reason, separate from permission to resend |
| Identity | Logical request ID, attempt ID, and one-based wire attempt number |
| History | Failures for all completed wire attempts and the final attempt details |
| Cause | Original cause retained for inspection without unsafe default formatting |

A keepalive byte increments raw progress but does not count as semantic output.
Track observed semantic output separately from output delivered to the caller.
Keep histories bounded by the configured maximum attempts. An admission wait is
not a wire attempt. A timeout or an empty response does not prove inference ended.

Default error strings, serialization, logs, and lifecycle events exclude
credentials, request contents, response text, tool arguments, and raw cause text.
Provider bodies and transport exception messages can contain secrets.
Retain an original cause privately. Expose it only through an explicit inspection API.
Use validated metadata and stable reason codes in safe summaries.

## Policy semantics

Retries are disabled by default. The default maximum is one wire attempt.
Existing successful results, tool behavior, and legacy entrypoints remain compatible.
Provide an opt-in recovery API where existing error types cannot be extended safely.
Document any legacy error conversion. Recovery-enabled callers receive full metadata.

The shared policy has these configurable values.

- Maximum attempts, including the initial request. It is a positive integer.
- Base delay, delay ceiling, and jitter source.
- Retryable categories and HTTP statuses.
- Optional recovery deadline or duration budget.
- Optional asynchronous admission hook.
- Optional lifecycle observer and explicit wire trace observer.

Default eligibility includes transient transport failures, HTTP 429, and
HTTP 500, 502, 503, and 504. It excludes authentication, invalid requests,
unsupported options, cancellation, and malformed responses.
Caller status/category selection does not authorize unsafe streaming replay.
Protocol errors remain ineligible unless a documented provider policy proves
that a specific error can be retried safely. Never use a catch-all retry loop.

For failed attempt `n`, the exponential ceiling is
`min(delay_ceiling, base_delay * 2 ** (n - 1))`. Avoid arithmetic overflow.
Full jitter chooses a value between zero and this ceiling.
Deterministic tests inject the clock, sleeper, and jitter source.

Retry-After accepts delay seconds and HTTP dates. Calculate dates against the
observed wall clock. A past date has zero remaining delay. Invalid values
do not replace the policy delay. A valid Retry-After is a minimum delay.
Use the greater of Retry-After and the jittered policy delay.
If Retry-After exceeds the configured delay ceiling or remaining recovery
budget, refuse the retry. Do not shorten the provider's minimum delay.

Measure a duration budget from the first failure using a monotonic clock.
An explicit absolute deadline applies to retry admission and delay.
Neither limit introduces a timeout for active local generation.
An admitted attempt may finish after the recovery deadline.
Do not start another attempt at or after the deadline.
Cancellation remains authoritative during active requests, admission, and backoff.
Recheck cancellation and the recovery deadline immediately before sending.

Disable hidden SDK or transport retries on recovery-enabled requests.
Every actual resend counts toward the maximum and has its own attempt identity.
Do not change dependency versions as part of this feature.

## Admission and provider capabilities

Before every retry, invoke the configured asynchronous admission hook.
Give it the failure, next attempt number, logical identity, previous attempt
identity, and observed and delivered progress. Let it allow, reject, or wait.
A wait remains pending until an explicit decision arrives. It is not approval.
Cancel pending hooks promptly without leaving an inference task running locally.
Caller cancellation of HTTP does not itself prove remote inference stopped.

If no hook exists, a local provider retry needs safe termination evidence when
the prior request may have executed. Otherwise return an admission-required outcome.
Explicit caller admission can authorize the retry after the caller's checks.
An acceptance value of `unknown` is ambiguous, including proxy HTTP 504.
A rejection prevents any resend and retains the original failure and decision.
Eligibility alone never authorizes an ambiguous local-provider resend.

Expose capabilities for request cancellation, request status, and idempotency.
Distinguish supported, unsupported, and unknown. Report termination as confirmed
only with evidence for this exact attempt. Document the evidence source.
Inspect current provider documentation before claiming support.

Ollama model unload and loaded-model lists are model-level operations.
They are not proof that this request ended. oMLX model activity can also be
aggregate evidence. Never kill an unrelated model or process.
Unsupported request status/cancellation must remain unsupported.

Keep a logical request ID across all attempts. Give each wire request a new
attempt ID. Local IDs provide correlation, not inference idempotency.
Use a provider idempotency facility only when it is documented for the actual
endpoint. Describe its limitations. Unsupported providers receive no invented
idempotency header. Attempt metadata may vary only where documented.

## Streaming and immutable payloads

Recovery applies to ordinary, structured, and streaming completion requests.
Preserve one immutable semantic payload across eligible wire attempts.
Keep messages, native reasoning history, tools, schemas, sampling controls,
model choice, and generation limits identical.
Only documented attempt and idempotency metadata may change.

Before semantic output reaches the caller, transparent recovery is allowed
subject to eligibility, admission, the attempt limit, and recovery limits.
After reasoning, content, or any tool fragment reaches the caller, return an
explicit interrupted-stream outcome. Include progress, failure, and history.
Do not append a new attempt to a partial response or report successful completion.
Keepalive-only progress does not block admission of an otherwise safe retry.

Completed tool calls and their delivery count as semantic progress.
Do not execute tools again, restart the broker loop, or restart the agent.
Test the public broker/session boundary, not only a private retry helper.
Exhaustion is always a failure. Legacy APIs may raise or return their existing
error form, but cannot turn interruption into a successful complete response.

## Lifecycle and exact traces

Emit typed lifecycle events for each of these transitions.

- Attempt started and attempt succeeded.
- Attempt failed with safe metadata and progress.
- Admission pending, allowed, rejected, or required.
- Delay scheduled and retry started.
- Exhausted, interrupted, or cancelled.

Include logical and attempt IDs, actual wire attempt counts, phase, and progress.
Keep wire attempts distinct from model interactions and broker tool depth.
Admission and backoff do not consume model-interaction budget.

Allow an explicit observer to capture each actual request and response separately,
including HTTP failures and partial responses. Hooks run at the wire boundary.
Default lifecycle events contain no payloads. Raw capture is an explicit caller
choice with its own storage policy. Capture failures must not silently mark a
request successful or create another inference request.

## Deterministic conformance evidence

Use scripted HTTP transports or fake provider boundaries. No live model is needed.
Run the cases through public entrypoints and each applicable adapter.

| Case | Required proof |
| --- | --- |
| 503 then success | Exact wire attempts, unchanged payload, distinct IDs, complete lifecycle |
| 429 | Seconds and date Retry-After, invalid values, delay ceiling and budget refusal |
| Persistent 504 | Bounded exhaustion, final structured error, complete attempt history |
| Ambiguous local timeout | No resend while pending; allow permits it; reject prevents it |
| Cancellation | Prompt exit in request, admission, and backoff; no later wire attempt |
| Partial stream | Reasoning, content, and tool fragments each interrupt without replay |
| Permanent errors | 400, 401, unsupported options, and malformed protocol are not retried blindly |
| Keepalive only | Raw progress is present; delivered semantic progress is absent |
| Compatibility | Default one attempt, unchanged successful behavior, legacy API checks |
| Privacy | Sentinel credentials and payloads absent from safe errors and events |
| Broker tools | No duplicated tool execution or reset of tool depth during request recovery |
| Capabilities | Unsupported and unknown facilities are explicit for Ollama and oMLX |

Every maintained port must provide an end-user migration example and provider
capability table. Include all completion adapters the port maintains, not only
Ollama and oMLX. Separate realtime voice and embeddings from completion recovery.
Run all required project quality gates and applicable security checks.
Do not lower quality thresholds or audit scope to make a check pass.

## Implementation and harness integration

Implement Elixir first. Port its contract, then compare deterministic evidence
from Python, TypeScript, Rust, Swift, and Kotlin. Make per-port commits.
Update the root parity matrix only with verified evidence.
No coordinated release is needed to validate pinned commits.

Pin the exact Rust implementation revision in the harness and its lockfile.
Register a fresh recovery experiment before running its scripted provider cell.
Preserve conversation history and tool counts across eligible request recovery.
Record request/response traces for every attempt, including final HTTP failures.
Test ambiguous admission rejection, interruption, and cancellation in that cell.
Do not rerun Generation45 or any other stopped benchmark.
Do not infer model efficacy or the original 504's origin from fake-provider results.
