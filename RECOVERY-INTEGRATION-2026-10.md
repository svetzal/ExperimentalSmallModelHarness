# Request recovery integration

Owner: Stacey Vetzal. Scope approved October 9, 2026.

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

The fresh experiment is
`~/Work/Sandbox2/Experiments/TransientRecovery-2026-10-09/`.
The coordinator owns its plan, frozen registration, and result artifacts.
Foundry changes this Harness repository only. It cannot write the experiment.

Provide a deterministic integration test or runnable fixture that the coordinator
can invoke after freezing the harness revision and worker binary hash.

The Mac coordinates. The Linux worker build and validation run on `mojility-ops-01`.
Do not start live inference. Do not restart or edit Generation45 or another
stopped benchmark. Do not infer model efficacy or timeout origin from fake providers.

## Repository rules

Fetch and pull with rebase before edits. Investigate dirty state first.
Stop on a rebase conflict. Do not revert other sessions' work.
Land focused changes on `main`, without pull requests or public feature branches.

Changes to dependency versions need separate commits. The Mojentic pin is part
of integration. Unrelated dependency maintenance is outside this feature's scope.
