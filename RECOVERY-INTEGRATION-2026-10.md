# Request recovery integration

Owner: Stacey Vetzal. Integration scope approved October 9, 2026.
Scoped live authorization updated October 10, 2026.

## Required behavior

Integrate the shared Mojentic transient recovery contract through the existing
worker provider paths. Use the exact reviewed Rust implementation commit.
Keep the pinned native-reasoning prototype behavior and generation controls.

Retries remain disabled by default. Add an explicit caller setting for request
recovery. Keep its wire-attempt limit separate from model interactions and tool
depth.

Use the library's request-level recovery API. Do not wrap or replay the agent loop.

Use cancellable admission to check provider ownership and previous inference state.
Reject an ambiguous request when the harness cannot establish safe admission.
Model residency and aggregate activity alone do not prove a request stopped.
Do not unload or kill unrelated models. Do not add a wall-clock generation cutoff.

Persist the library's safe lifecycle events with logical IDs, attempt IDs, actual
wire counts, progress, admission decisions, delays, interruption, and final failure.

Allow explicit capture of each request and response, including failed HTTP responses
and partial streams. Keep exact payload capture separate from safe lifecycle events.
Do not log credentials. Do not write credentials to any file.

An interrupted semantic stream is an explicit terminal outcome for this request.
Preserve the conversation and prior tool results. Do not restart the agent, execute
tools again, append a retry to partial text, or report exhaustion as success.

## Deterministic evidence

Exercise the real public worker path against scripted Ollama and oMLX HTTP servers.
Use explicit loopback endpoints and synthetic credentials only.
Clear inherited provider credentials and proxies before each worker starts.
Do not read the secret store or send a request to a real model endpoint.
Test 503 then success, Retry-After, persistent 504, ambiguous pending and rejected
admission, cancellation, partial reasoning/content/tools, keepalive, and permanent
errors.
Include a tool that already ran before the next completion request failed.
Prove that its side effect occurs once and its result survives the recovered request.

Test with recovery disabled and enabled. Assert semantic payload identity, native
reasoning history, exact attempt counts, distinct attempt IDs, model interaction
accounting, tool depth, and complete request/response traces.
Run the full suite, type/compile checks, lint, and the dependency audit.
Do not suppress findings or reduce gate scope.

Add `RECOVERY-CONFORMANCE.md` with assertion-bearing test names, results, caller
examples, trace fields, capability limits, and exact implementation revisions.
Follow the CLI UX skill when adding flags or output. Keep names consistent with
existing run settings. Help must explain attempts versus semantic interactions.

## Experiment boundary

The October 9 proposal named
`~/Work/Sandbox2/Experiments/TransientRecovery-2026-10-09/`. That historical
path does not authorize restarting a cell. The October 10 scope covers only a
NEW registered bounded experiment chosen and owned by the Mac coordinator,
including its plan, frozen registration and result artifacts. Foundry changes
this Harness repository only and cannot write any experiment directory.

Provide a deterministic integration test or runnable fixture that the coordinator
can invoke after freezing the harness revision and worker binary hash.

The Mac coordinates. The Linux worker build and validation run on `mojility-ops-01`.
Foundry must not start live inference or write experiment directories. After
validation, the Mac coordinator may run only the NEW registered bounded live
experiment under the October 10 authorization. The previous no-live rule remains
part of the historical work record. Generation45 and every stopped benchmark
remain frozen. Do not infer model efficacy or timeout origin from fake providers.

## Repository rules

Investigate dirty state and compare upstream main before edits. Ordinary work
fetches and pulls before editing. Inside a Foundry task worktree, Foundry owns
Git finalization: do not change refs, commit, push, merge or rebase. Leave focused
changes for independent review; preserve other sessions' edits. No pull request.

Changes to dependency versions need separate commits. The Mojentic pin is part
of integration. Unrelated dependency maintenance is outside this feature's scope.

## Resolved API prerequisite and production integration

The original reviewed API at `bc8150a381a540e6320cd4fd91a8bbecadb9a735`
lacked per-frame recovery progress and convenience-wrapper metrics. This blocked
runtime integration without changing existing generation controls.

Reviewed Mojentic `4ca1ed279c02eab37827a1ed07c30e961155ecf3` resolves the gap
with typed `Progress { identity, progress }` and `Metrics { identity, metrics }`.
Harness base `4854cc8a66ede3dacb0f3d37847ee09b14e16421` contains positive
legacy/raw-recovery/compatibility equivalence assertions. Keep those assertions
and the exact manifest and lockfile pin.

The production worker uses the reviewed request recovery engine with one wire
attempt by default. Explicit recovery, cancellable proof-bound admission,
credential-safe capture and native lifecycle rendering are implemented in
`src/recovery.rs`, `src/agent.rs` and `src/transcript.rs`. See
`RECOVERY-CONFORMANCE.md` for assertion-bearing actual-worker evidence, helper
schema, frozen Linux binary repetitions and explicit limits.

The credential correction retains possible prefixes per semantic channel and tool
field. Harmless content interleaved between reasoning fragments cannot release
credential pieces. Both-provider public regressions cover partial and completed
tools, argument/name/ID fragments, JSON escaping and mixed semantic fields.
Captures with detected contamination are withheld and marked nonexact and
credential-redacted; the worker records terminal progress without executing a
tool or retrying. See the credential correction evidence in
`RECOVERY-CONFORMANCE.md`; the coordinator's earlier partial-tool case was already
redacted by the recovered production source.
