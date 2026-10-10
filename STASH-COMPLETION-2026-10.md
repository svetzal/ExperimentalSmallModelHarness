# Stash inventory and completion checks

## Observation and hypothesis

Generation46 r02 used `git stash && python -m pytest ... && git stash pop`.
The test returned nonzero and the final pop did not execute. The saved-stash
feedback was present in provider snapshot depth 46 and its adjacent ledger.
Integration edits remained in the preserved stash when the interaction 50 limit
ended the run; its final workspace scored 0/24 features and 2/2 preserved tests.

Stacey authorized the stash-entry/completion check on October 10, accepting the
risk of concurrent changes in a shared repository. This is a preservation check,
not a reasoning, context-budget or retry adaptation. HYP-STASH-01: comparing
entry and completion inventories detects unresolved additions without treating
an unchanged pre-existing stash as work created by the agent. The nearest
alternative is a naive nonempty-stash check that blocks legitimate existing
work or relies on shifting reflog indices. Deterministic real-Git and public
worker fixtures distinguish these cases; no inference is needed for this gate.

## Behavior

Before any agent tool runs, capture the Git directory and every stash object ID.
The entry inventory is traced and delivered to the model for Git repositories.
Use object IDs and occurrence counts, not `stash@{0}` positions: a new stash
renumbers existing entries, and duplicate IDs can have multiple entries.

Before accepting DONE, capture another inventory. An unchanged baseline is
allowed, including nonempty pre-existing stashes. New entries, removed baseline
entries, a changed repository identity or an unavailable inventory withhold
DONE and deliver explicit feedback. A non-repository is supported; creating a
repository during the run establishes a current inventory against the empty
entry state. A missing Git executable or unreadable inventory is unknown, not
an empty stash list. Each Git subprocess has a finite 10-second timeout and kill
on cancellation; this is not a timeout on model generation.

The agent explicitly decides how to restore or resolve its new stash and then
validates the actual work tree. The harness never automatically applies, pops,
drops or deletes a stash. If the last outer turn rejects DONE, the final summary
is FAIL, not a stale DONE. No interaction/outer-turn budget is widened. FAIL and
natural limit outcomes remain possible, with the exit inventory recorded;
stream errors also record the exit inventory without another model dispatch.

Trace events are `agent.git_stash.entry`, `agent.git_stash.completion_check`,
`agent.git_stash.done_rejected` and `agent.git_stash.exit`. The native transcript
includes these checks; a rejection is displayed as a warning. This is an
inventory difference, not proof of exclusive process ownership. Shared Git
worktree stashes or other concurrent writers can trigger it. That risk was
accepted; attribution is explicitly labeled uncertain rather than invented.

## Deterministic validation

Real-Git module tests cover unchanged/reordered entry stashes, new and removed
entries, duplicate object IDs, explicit restoration, unknown state and repository
replacement. Agent-loop fixtures verify entry guidance, DONE rejection and
retained feedback, preservation through FAIL, removal of a baseline entry and
last-turn rejection. The built public worker additionally tests an existing
stash and an agent-created stash through both scripted provider adapters.

The exact assertion-bearing tests are in `src/stash.rs`, the stash-named agent
tests in `src/agent.rs`, and
`built_worker_preserves_entry_stash_and_rejects_new_stash_both_providers` in
`tests/omlx_gateway.rs`. The normal required format, strict Clippy, all-target
all-feature tests, release binary build and unfiltered dependency audit passed
on Linux without gate changes: 366 tests passed and two existing tests remained
ignored. The 102-case worker recovery matrix also passed. This check has no new
dependency or Mojentic change.

## Restored implementation diagnostic

The independent, no-inference diagnostic is preserved under
`Experiments/StashDiagnostic-2026-10-10-02` in the parent workspace. It applies
r02's saved stash only in a disposable copy and reruns the unchanged 24-feature/
2-preserved verifier. All original source-file hashes remain unchanged.

The restored implementation still scores 0/24 + 2/2. It rejects an include/exclude
overlap that the contract and verifier permit with exclusion taking precedence;
many feature tests fail during initial fit. Therefore missing integration edits
are an observed defect but are not a sufficient explanation of the zero score.
No claim is made that this guard would turn that implementation into a passing
one. The first diagnostic attempt is preserved separately: copied file ownership
caused Git to reject the repository before verification. The second uses
extraction without retaining the source owner's UID; no safety check was disabled.

Keep the guard as protection against accepting an unresolved workspace state.
Broader quality effects remain unmeasured. Next measurement: a separately
registered replicated cell with the existing model settings and this guard as
the only intended runtime change. Do not automatically restart Generation46 or
launch inference. Roll back future use if inventories misclassify unchanged
baseline work or completion feedback causes a preservation/accounting defect.
