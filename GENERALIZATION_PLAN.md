# Harness Generalization Plan

Date: 2026-07-16

## Decision

Keep the current runtime policy unchanged while the demo evidence is converted
into an explicit architecture and measurement backlog.

The demos show that the harness already has a reusable coding loop. They do not
yet show that the loop is a general adaptive runtime. The next work should
separate domain policy from orchestration without simultaneously changing model
behavior, patience, transcript retention, or validation policy.

## Evidence Baseline

The cross-model demo matrix completed all 30 planned cells and independently
validated 24. Five bounded tasks covered Rust, Python, Go, JavaScript, and Ruby.
Four of the six screened model configurations passed every task, while the two
remaining configurations failed at distinct action boundaries.

This is screening evidence with one run per cell. It supports architectural
work and replication choices, but not stable capability rankings or a broad
policy change.

Observed reusable behavior:

- Workspace tools stayed rooted in the generated project.
- Deterministic validation prevented unsupported completion claims.
- Failed validation could produce a narrow edit, a fresh probe, and recovery.
- Stream, context, tool, mutation, and validation events preserved useful run
  evidence.
- Throughput-aware patience allowed slow local models to reach natural harness
  terminal states.

Observed boundaries and confounders:

- One configuration reached hidden-only no-action termination in all five
  tasks.
- Qwen NVFP4 reached repeated action-boundary interrupts on the Go task without
  source mutation or validation.
- The original Ruby validation environment was invalid even though the
  harness and generated code were not. Independent revalidation was required.
- Every matrix cell is anecdotal because it has one replicate.

## What Is General Already

These capabilities belong in the runtime core:

- Filesystem scope enforcement and ignored-path filtering
- Bounded tree and file inspection
- Tool-result retention and context assembly accounting
- Streaming progress observation and throughput projection
- Action-boundary, inspection-loop, and no-content detection
- Mutation-to-probe freshness tracking
- Trace recording and deterministic terminal-token handling
- Finite timeouts for non-model subprocesses

Their current implementation is coding-oriented, but their responsibilities
apply to other artifact-producing work.

## Coupling To Remove

The current implementation contains several kinds of domain coupling.

### Prompt Coupling

The core system prompt identifies the worker as a coding harness, tells it to
create Rust files, prescribes Cargo timeouts, and embeds a Rust validation
ladder. Those rules should come from a coding profile or task contract.

### Probe Coupling

Validation discovery and classification rely on command-name allowlists. The
allowlists cover the five demo languages unevenly and duplicate logic between
the agent and tool layers. A probe should be declared by the task contract and
identified by a stable probe ID; command inference should remain only as a
legacy adapter.

### Artifact Coupling

The runtime treats nearly every non-document file as source requiring
validation. This was useful for the demos, but the core concept is a mutation
that invalidates specified evidence. Artifact classes and invalidation rules
should be contract data.

### Control-Loop Coupling

Most orchestration, provider streaming, context retention, validation
bookkeeping, repair policy, prompt construction, and terminalization live in
`agent.rs`. Most scope, filesystem tools, mutation sensing, command execution,
probe inference, and repair state live in `tools.rs`. The two modules total
more than 10,000 lines, making policy boundaries difficult to identify and
test independently.

### Measurement Coupling

The built-in trace analyzer does not yet report all standard experiment
metrics, including first action, first source mutation, validation reach,
validation pass, and hard-stop reason. The demo matrix therefore reconstructs
important outcomes in separate scripts. The runtime needs one canonical event
vocabulary and one canonical analyzer before its architecture is changed.

## Target Boundaries

The next architecture should distinguish five responsibilities.

1. `RunContract` describes the goal, artifact scope, guidance, probes,
   invalidation rules, budgets, and terminal protocol.
2. `RuntimeState` records facts about messages, actions, mutations, evidence,
   budgets, and active failures.
3. `RuntimeEvent` is the canonical input to state transitions and trace
   analysis.
4. `RuntimePolicy` maps state and the latest event to the next allowed action,
   prompt, interruption, or stop.
5. Domain profiles supply worker guidance, artifact classification, probe
   definitions, and optional diagnostic adapters.

The first domain profile remains `coding`. The existing Markdown task format
and command inference become a legacy adapter so current experiments remain
replayable.

The core should not know about Cargo, Bevy, Rust source layout, or any specific
test runner. It may know that a mutation invalidated evidence, a declared probe
ran, the probe passed or failed, and repair is permitted.

## Ordered Backlog

### Slice 0: Freeze The Baseline

- Record the current harness commit in all new runs and comparison summaries.
- Preserve the 30-cell demo matrix as the architectural baseline.
- Add deterministic analyzer fixtures for representative pass, repair, action
  boundary, hidden-only, and environment-invalid traces.

Exit condition: expected metrics can be reproduced without reading narrative
notes or using matrix-specific classification code.

### Slice 1: Canonicalize Measurement

- Add first action, tool call, artifact mutation, probe reach, probe pass,
  hard-stop, environment-stop, and manual-stop fields to `TraceAnalysis`.
- Define stable event names and payload fields for those measurements.
- Make the matrix summarizer consume analyzer output or the same library types.
- Distinguish harness completion from independent validation and environment
  validity.

This is a measurement-first change. It must not alter model prompts, budgets,
tool availability, or stop policy.

Exit condition: the analyzer accounts for all 30 matrix cells with the same
pass/fail and hard-stop counts as the preserved results.

### Slice 2: Introduce A Typed Run Contract

- Parse explicit probe IDs and commands instead of scraping them from arbitrary
  shell fences.
- Carry read scope, write scope, mutable artifact classes, evidence
  invalidation, guidance, and terminal tokens in one resolved contract.
- Trace both the supplied contract and the resolved legacy defaults.
- Preserve the existing task format through a legacy coding adapter.

Exit condition: existing demos resolve to contracts equivalent to current
behavior, proven by deterministic contract snapshots.

### Slice 3: Extract Coding Policy

- Move Rust and Cargo guidance out of the core system prompt.
- Move validation command families into the coding profile.
- Move document-versus-source mutation classification into profile data.
- Keep generic scope, freshness, repair, and terminal state in the core.

Exit condition: the runtime core contains no Rust, Cargo, Bevy, or test-runner
names, while the coding profile preserves current demo behavior.

### Slice 4: Separate State From Effects

- Represent significant model, tool, mutation, probe, and stop observations as
  typed runtime events.
- Move counters and transition rules into a deterministic state reducer.
- Keep provider streaming, filesystem access, and subprocess execution behind
  effect interfaces.
- Test transition sequences without invoking an LLM or shell.

Exit condition: repair, validation freshness, action-boundary, and terminal
decisions have table-driven transition tests independent of tool
implementations.

### Slice 5: Prove A Second Domain

Add one small non-code artifact task only after the coding profile reaches
parity. A suitable first domain is a scoped text transformation with explicit
file assertions and no shell-command inference.

Exit condition: the second profile reuses the same state machine, tracing,
scope, freshness, and stop policy without coding-specific branches in the
runtime core.

## First Experimental Gate

Hypothesis ID: `HYP-GEN-01`

Observation: the demo loop works across five coding languages, but successful
behavior may depend on implicit coding prompt and command-inference details.

Hypothesis: an explicit typed run contract plus a coding profile can preserve
the current completion and repair behavior while removing coding knowledge
from the runtime core.

Nearest alternative explanation: the current loop succeeds partly because its
implicit prompt and duplicated command heuristics interact in ways that a
resolved contract will not preserve.

Intervention: compare the legacy task adapter with the explicit resolved
contract while changing no other variable.

Initial cell:

- Task: Python expression parser demo
- Model: `qwen3.6:35b-a3b-coding-nvfp4`
- Control: legacy Markdown task adapter
- Treatment: explicit contract containing equivalent scope, guidance, probes,
  budgets, and terminal protocol
- Replicates: three fresh runs per arm
- Fixed variables: seed workspace, model, quantization, context window,
  `num_predict`, transcript policy, packet type, tool budgets, thinking caps,
  repair policy, and validation commands

Confirming measurements:

- Both arms reach source mutation and validation in at least two of three runs.
- Treatment validation-pass count is not lower than control in this first cell.
- Treatment traces record the resolved contract and probe IDs without relying
  on command scraping.
- Hard-stop classes and context pressure do not reveal a new systematic
  failure mode in treatment.

Refuting measurements:

- Treatment repeatedly fails before mutation or validation while control does
  not.
- Treatment loses requested-probe freshness or accepts completion without all
  declared probes passing.
- Resolved contract snapshots differ semantically from the legacy task.

Decision rule: if contract equivalence fails deterministically, repair the
adapter and do not run model cells. If snapshots agree but treatment regresses
in at least two of three paired outcomes, keep the coding policy in place and
classify the missing interaction before further extraction. A positive first
cell permits one changed variable in the next language cell; it does not prove
cross-domain generality.

## Explicit Non-Decisions

- Do not change thinking caps, action-boundary limits, transcript retention, or
  repair allowances during the generalization cell.
- Do not switch model families for the next comparison.
- Do not infer a model capability boundary from the existing one-run matrix.
- Do not split `agent.rs` and `tools.rs` mechanically before state, event, and
  contract boundaries have tests.
- Do not add a second domain until coding-profile parity is measured.

## Rollback Conditions

Revert or narrow the architectural extraction if any of these occur:

- Existing task contracts resolve to different scopes or probes.
- Required validation can become stale or be bypassed.
- Trace analysis can no longer reproduce preserved matrix outcomes.
- The explicit contract adds context pressure that changes action-boundary
  behavior without that change being the tested variable.
- Coding-specific conditions reappear as branches in the runtime core rather
  than profile data.
