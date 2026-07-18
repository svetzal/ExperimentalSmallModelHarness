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

**Slice 0 status: complete (2026-07-17).** Harness provenance is now a typed
`HarnessSourceState` (`src/provenance.rs`), captured once per run and carried
in the `run.started` trace event, `AgentRunSummary`/`run.finished`, and
`TraceAnalysis` — one canonical representation instead of an untyped,
single-use JSON blob. The completed 30-cell matrix is preserved as
machine-readable evidence in `baseline/matrix_baseline.json`, loaded and
invariant-checked by `src/baseline.rs`. Five deterministic trace fixtures
(`fixtures/traces/{pass,validation_repair_pass,action_boundary_stop,
hidden_only_no_action_stop,environment_invalid_validation}.jsonl`) exercise a
new canonical `RunOutcome` classification on `TraceAnalysis`, derived only
from event kinds and metrics the runtime already emits (no new runtime
policy). `cargo run -- analyze-trace fixtures/traces/*.jsonl` reproduces the
same five classifications and populated `harness_source_state` byte-for-byte
across repeated invocations, with no narrative-note reading or
matrix-specific classification code involved — the exit condition above.

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

**Slice 1 status: complete (2026-07-17).** `TraceAnalysis` now carries typed,
additive measurements — `first_tool_call`, `first_productive_action`,
`first_source_mutation`, `validation_probe_reached`,
`validation_probe_passed` (each an `Option<Milestone>` naming the evidence
event), `hard_stop` (a typed `HardStopReason`), `environment_stop`, and
`manual_stop` — populated by backward-compatible adapters that prefer the
runtime's existing canonical stage events
(`agent.stage.first_source_mutation`, `agent.stage.first_validation_probe`,
`agent.validation_probe.observed`) and fall back to inferring the same
milestones from raw `tool.*` payloads for legacy traces. `src/runtime_events.rs`
documents the stable event names and payload fields those adapters read,
including two additive-only events (`agent.run.manual_stop`,
`agent.independent_validation.observed`) that are documented but not yet
emitted by the runtime. Three separate typed facts now replace the old
single collapsed status: `harness_completion` (did the harness itself reach
a terminal state, and which kind), `independent_validation` (external
evidence only — `run.finished`/`DONE` never implies a pass; it stays
`Unknown` without an explicit event or matrix record), and
`environment_validity` (was the validation environment itself trustworthy).
Eight new deterministic fixtures and analyzer tests exercise every new field,
including a legacy trace with no canonical events at all (proving `DONE` does
not become a pass) and an explicit manual stop.

The matrix summarizer that used to live only in `Demos/Matrix/summarize.rb`
is now `summarize_matrix` in `src/baseline.rs`, consuming a typed
`MatrixCell` (`harness_completion` / `hard_stop` / `independent_validation` /
`environment_validity` — the same enums `TraceAnalysis` uses) rather than
re-deriving pass/fail from narrative text. `baseline/matrix_baseline.json`
(schema v2) embeds all 30 cells transcribed verbatim from the read-only
`Demos/Matrix/results.tsv` oracle. `cargo run -- summarize-matrix` reproduces,
byte-identically across two consecutive invocations,
`completed_cells: 30`, `independently_validated_passes: 24`,
`independent_validation_failures: 6`, `hard_stops: 6`
(`{"action_boundary": 1, "hidden_only_no_action": 5}`),
`environment_corrections: 6` (all six `ruby-ini-parser` cells), and the exact
six failing-cell identities and per-model/per-task pass tallies from
`Demos/Matrix/results.tsv`/`results.md` — the exit condition above. No
matrix-specific classification code remains necessary for reproduction.
`src/agent.rs`, `src/tools.rs`, and `Cargo.lock` are unchanged by this slice.

### Slice 2: Introduce A Typed Run Contract

- Parse explicit probe IDs and commands instead of scraping them from arbitrary
  shell fences.
- Carry read scope, write scope, mutable artifact classes, evidence
  invalidation, guidance, and terminal tokens in one resolved contract.
- Trace both the supplied contract and the resolved legacy defaults.
- Preserve the existing task format through a legacy coding adapter.

Exit condition: existing demos resolve to contracts equivalent to current
behavior, proven by deterministic contract snapshots.

**Slice 2 status: complete (2026-07-17).** Per-run inputs now flow through one
typed, resolved `ResolvedRunContract` (`src/contract.rs`, `schema_version`
`run_contract.v1`) carrying guidance, read/write `Scope`, mutable artifact
classes (`doc-exempt-v1`, a descriptive snapshot of
`path_requires_validation_after_write`), evidence invalidation, ordered
`Probe`s (id + command, never re-sorted), the budgets the runtime already uses,
terminal `done`/`fail` tokens, an `adapter_kind`, and path-free
`defaults_provenance`. Two adapters resolve into that single type: a `// LEGACY`
coding adapter that wraps the moved shell-fence scraper
(`requested_validation_commands`) and synthesizes stable slugified probe IDs,
and an explicit adapter that parses a declared-probe JSON contract and bypasses
scraping. Explicit contracts are validated before any LLM/tool effect
(duplicate/empty probe IDs, failure-masking or executor-unrecognized commands
cross-checked against `tools::is_validation_probe`, root-escaping scopes,
invalid terminal tokens, inconsistent artifact/invalidation references,
malformed budgets), each with an actionable error.
`run_coding_agent_with_gateway` makes exactly one `resolve_contract` call before
`ToolScope::new` (kept unrestricted -- scope is descriptive data only) or any
gateway call, feeds `contract.probes` into the ledger and `run.started` with
byte-identical ordering, and traces both the supplied and resolved contracts via
the additive `agent.contract.supplied`/`agent.contract.resolved` events
(`src/runtime_events.rs`), consumed as additive `Option` fields on
`TraceAnalysis`. A no-provider `resolve-contract` CLI subcommand doubles as the
snapshot generator/check. `cargo run -- resolve-contract --goal
fixtures/contracts/legacy/<task>/task.md` reproduces each committed snapshot in
`fixtures/contracts/snapshots/*.json` byte-for-byte across repeated invocations
(Ruby resolves to empty probes, exactly as today), an explicit cargo contract
declaring the same probes resolves to the same normalized core as its legacy
equivalent, `cargo run -- summarize-matrix` still reports `30`/`24`/`6` parity,
and `cargo run -- analyze-trace fixtures/traces/*.jsonl` is unchanged -- the exit
condition above, proven without running any model.

### Slice 3: Extract Coding Policy

- Move Rust and Cargo guidance out of the core system prompt.
- Move validation command families into the coding profile.
- Move document-versus-source mutation classification into profile data.
- Keep generic scope, freshness, repair, and terminal state in the core.

Exit condition: the runtime core contains no Rust, Cargo, Bevy, or test-runner
names, while the coding profile preserves current demo behavior.

**Slice 3 status: complete (2026-07-17).** All Rust/Cargo-flavored (and
legacy other-language) literals moved verbatim out of `agent.rs`, `tools.rs`,
and `contract.rs` into a new `src/profile/` module: `src/profile/mod.rs`
defines the `DomainProfile` trait (system/run guidance, post-write
validation-nudge text, repair-ladder suffix, probe recognition, command-family
normalization, path/dir/inspection/mutation classification, failure-detail
parsing, the legacy contract adapter, default artifact classes and evidence
invalidation, and action-intent phrases) plus a stable `ProfileRef { id,
version }` identity and `select_profile()`; `src/profile/coding.rs` owns every
moved behavior for the one profile that exists today
(`CODING_PROFILE_ID = "coding"`, `CODING_PROFILE_VERSION =
"coding_profile.v1"`), including the coding-focused unit tests (legacy
shell-fence scraper, cargo/pytest probe recognition, artifact classification,
family normalization) that are explicitly allowed to name languages and
tools. `ResolvedRunContract` gained an additive `pub profile: ProfileRef`
field (`#[serde(default)]`, so contracts persisted before this field existed
still deserialize with `profile.id == "coding"` via `ProfileRef::default`);
`SCHEMA_VERSION` stayed `run_contract.v1` since the change is additive and
defaulted, not breaking. `contract.rs` now dispatches to
`crate::profile::coding`/`crate::profile::select_profile()` instead of
embedding coding literals or a `MutableArtifactClasses`/`EvidenceInvalidation`
`Default` impl. The five committed snapshots in
`fixtures/contracts/snapshots/*.json` were regenerated via `cargo run --
resolve-contract` and now carry `"profile":{"id":"coding","version":"coding_profile.v1"}`
as the only diff from their Slice 2 contents. New `fixtures/prompts/{system,run,
post_write_nudge,post_write_nudge_after_empty_turn,repair}.txt` capture the
actual runtime worker/system prompt, run guidance, both post-write nudge
wordings, and the repair-ladder suffix verbatim, diffed byte-for-byte by
parity tests in `src/profile/coding.rs`'s `#[cfg(test)] mod tests`. A new
`tests/structural_coupling.rs` reads each core production source file, strips
`#[cfg(test)] mod tests { ... }` blocks, and asserts the case-insensitive,
word-boundary-aware absence of cargo/bevy/rust/rustc/rustfmt/clippy/pytest/
npm/pnpm/yarn/"go test"/gradle/mvn/"mix test"/rspec (excluding
`src/profile/**`, `src/baseline.rs`, `fixtures/**`, and `baseline/**`, and
naturally excluding build-time identifiers like `CARGO_MANIFEST_DIR` since
"cargo" only appears there as a sub-word of a larger identifier, not a
standalone word) — reintroducing a forbidden token in `agent.rs` was verified
to fail this guard before being removed again. The one genuine discrepancy
found during extraction was two copies of `path_requires_validation_after_write`
(`agent.rs` and `tools.rs`) written as different early-return shapes over the
same exemption tables; they were logically identical (verified by inspection
and by the consolidated single copy's existing test coverage passing
unchanged), so they collapsed into one `crate::profile::coding` function
rather than picking one as "more correct." The exit condition —
`cargo test --test structural_coupling` passes, `cargo run --
resolve-contract` is deterministic and now shows the profile field,
`cargo run -- summarize-matrix` still reports `30`/`24`/`6`, and
`cargo run -- analyze-trace fixtures/traces/*.jsonl` is byte-identical across
repeated invocations — was verified with exactly those four commands, plus
`cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D
warnings`, and `cargo test` (177 unit/integration tests plus 3 structural
tests, all green), with `git diff --stat Cargo.toml Cargo.lock` empty
throughout.

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

**Slice 4 status: complete (2026-07-18).** `src/runtime.rs` now defines the
serializable `runtime_event.v1` observation vocabulary, a pure deterministic
`RuntimeState` reducer, and a typed `RuntimePolicy` decision layer. The state
owns mutation/fresh-validation epochs, declared-probe freshness and ordering,
write budget and the one-edit repair allowance, repair reads/no-action state,
action-boundary counts, repeated inspections, true-empty versus tool-only and
hidden-only turns, terminal readiness/tokens, manual and environment stops,
and effect-failure facts. `src/runtime_events.rs` is the explicit adapter from
typed events to the stable pre-Slice-4 trace names and payload shapes; reducer
and policy code do not consume trace strings.

`agent.rs` and `tools.rs` now classify provider/tool outcomes into typed events,
ask policy before mutation effects, reduce accepted observations, and retain
the existing prompt and trace adapters. The former `ToolPolicyState`, repair
and action-boundary tracker structs, and inspection counter storage were
removed. Stable filesystem/shell implementations remain in their existing tool
adapters; the provider remains behind `LlmGateway`. A deterministic
`RuntimeEffects` boundary and fake prove denied mutations and accepted terminal
states cannot execute later effects.

The exit condition was verified without a local-model run: `cargo fmt --check`,
warning-free `cargo clippy --all-targets --all-features -- -D warnings`, and all
189 unit/integration tests plus five structural tests pass. The 12-test pure
runtime table suite passed twice, and the focused fake-effect test passed. All
five resolved-contract snapshots matched committed bytes across two passes;
coding-profile prompt parity passed twice; `summarize-matrix` was byte-identical
twice at 30 completed / 24 independently validated passes / 6 failures; and
`analyze-trace fixtures/traces/*.jsonl` was byte-identical twice. Structural
guards prove `runtime.rs` imports no provider, filesystem, subprocess, clock,
tracing, or coding-profile implementation and that orchestration adapters do
not recreate the removed transition-state structs. `Cargo.toml` and
`Cargo.lock` are unchanged. Slice 5 and a second domain were not started.

### Slice 5: Prove A Second Domain

Add one small non-code artifact task only after the coding profile reaches
parity. A suitable first domain is a scoped text transformation with explicit
file assertions and no shell-command inference.

Exit condition: the second profile reuses the same state machine, tracing,
scope, freshness, and stop policy without coding-specific branches in the
runtime core.

**Slice 5 status: complete (2026-07-18).** A registered text_transform /
text_transform_profile.v1 profile now supplies stable identity, worker/run/
repair guidance, artifact policy, and minimal tool capabilities. Explicit
contracts retain the byte-compatible command-probe shape used by coding tasks
and add a typed file_text_equals assertion with a stable ID, scoped relative
path, and exact UTF-8 expectation. Validation rejects empty or duplicate IDs,
mixed command/assertion definitions, absolute and parent paths, paths outside
declared read/write scope, and symlink escapes before model or mutation
effects.

The profile-selected tool set exposes bounded tree/read/write/edit effects plus
execute_probe; it does not expose a shell tool. Assertion execution emits the
established validation trace events with additive probe ID, assertion kind,
path, status, and failure details, then reduces through the same validation
event, mutation epoch, stale-evidence, repair, and terminal-readiness
transitions used by coding probes. Fake-effect, real-filesystem, fake-gateway
orchestration, analyzer-compatibility, and structural tests cover pass/fail,
denial, stale-after-mutation, fresh reassertion, and absence of second-domain
identities or benchmark literals from the generic runtime core.

Deterministic parity remained green at Harness commit 85c48c3: 198 unit tests
and six structural tests passed; the five coding contract snapshots and coding
prompt fixtures stayed byte-identical; matrix summarization remained 30
completed / 24 independent passes / 6 failures; preserved trace analysis was
byte-identical; and Cargo.toml/Cargo.lock did not change. Generation26 then
recorded a valid instrumentation smoke and a fixed n=3
qwen3.6:35b-a3b-coding-nvfp4 cell. Distributions were 3/3 valid, 3/3 first tool
call, 3/3 artifact mutation, 3/3 assertion reached, 3/3 assertion passed, 3/3
terminal DONE accepted, 0 hard/manual/environment stops, and 3/3 independent
exact-file passes. This is a Replicated Cell Signal for the narrow benchmark,
not proof of broad cross-domain capability. Full evidence is under
Experiments/Generation26/.

### Slice 6: Eliminate Residual Default-Profile Coupling

- Carry the resolved profile through live orchestration state instead of
  reselecting a coding default inside action, inspection, mutation,
  validation-family, or repair decisions.
- Rename public run entry points so their names do not imply a coding-only
  agent.
- Guard production orchestration against default-profile reselection and
  coding-profile/tool identifiers.
- Replicate the text-transform profile on a structurally distinct task while
  holding model and policy fixed.

Exit condition: deterministic coding parity remains unchanged, production
orchestration has one resolved profile authority, and a pre-registered n=3
within-domain replication is classified without adapting policy from a single
trace.

**Slice 6 status: complete with a refuting replication (2026-07-18).** Commit
76ea2a3 renamed the live runner to profile-neutral entry points and carries the
resolved `DomainProfile` through action intent, inspection signatures,
meaningful-mutation classification, validation-family labels, and repair
guidance. Default profile selection remains only at explicit legacy/default
construction boundaries. A structural guard strips tests and rejects
`default_profile(`, `profile::coding`, `run_coding_agent`, and `coding_tools`
from production `agent.rs`; a focused test demonstrates that a Markdown write
invalidates text-transform evidence but not coding evidence.

Deterministic parity passed with 200 unit/integration tests and seven
structural tests, unchanged coding and text snapshots, a stable 30/24/6 matrix
summary, byte-stable preserved trace analysis, and unchanged Cargo manifests.
Commit 7c21a02 then added canonical analyzer reasons for already-emitted
thinking-only and no-content stream hard-failure events after the first
Generation27 smoke exposed that measurement gap.

Generation27 changed only the text packet and exact assertion bytes. Its valid
fixed n=3 cell reached first tool call, artifact mutation, and the declared
assertion in 3/3 runs, but passed the assertion, received terminal DONE, and
matched independent exact-file validation in 0/3. All three runs naturally
ended at the classified thinking-only-stream cap with green context pressure
and no manual, environment, or shell-tool event. This refutes the predicted
2/3 success threshold and supports dependence on Generation26's simpler task
shape for this packet. It does not isolate task complexity from a second live
explanation: `file_text_equals` reported the expected byte count but no bounded
mismatch location or excerpt, leaving exact repair under-informed. Policy and
prompt remain unchanged. The next measurement is a deterministic bounded
mismatch diagnostic followed by the same fixed cell under the adaptation
embargo. Full evidence is under Experiments/Generation27/.

### Slice 7: Measure Exact-Repair Diagnostics

- Report actual length and the first differing byte for failed exact-content
  assertions.
- Include deterministic escaped expected/actual excerpts bounded independently
  of artifact size.
- Preserve the full expected artifact outside normal worker context.
- Repeat the refuting Generation27 packet with no prompt, model, cap, policy,
  contract, seed, or expected-byte change.

Exit condition: offset, length, escaping, and maximum excerpt size have
deterministic tests; preserved coding evidence remains unchanged; and the exact
repeat cell distinguishes diagnostic delivery from successful repair.

**Slice 7 status: complete with a refuting replication (2026-07-18).** Commit
7c8fe2e changed only `file_text_equals` failure evidence. A mismatch now reports
expected and actual lengths, the first differing byte, and up to 16 preceding
plus 32 following escaped bytes from each side. Tests cover exact payload
content, bounded disclosure for long secrets, deterministic non-ASCII byte
escaping, pass/fail freshness, and terminal state. All 202 unit/integration
tests and seven structural tests passed; preserved matrix and trace outputs
remained byte-stable; Cargo manifests were unchanged.

Generation28 reused Generation27's contract, task, seed, and expected artifact
byte-for-byte. Its valid fixed n=3 cell reached mutation, assertion, and the
correct bounded byte-15 diagnostic in 3/3 runs. Only 1/3 converted that evidence
into a post-failure repair write, 0/3 executed a second probe, and 0/3 passed the
assertion, received terminal DONE, or matched independent exact validation.
All three naturally reached the thinking-only-stream cap with green context
pressure and no manual, environment, or shell-tool stop.

This refutes the predicted 2/3 diagnostic-assisted success threshold. Bounded
mismatch evidence is a valid and useful measurement—the one repair write fixed
the title and capitalization exposed by the excerpt—but it is insufficient at
the current 4,096-token thinking-only boundary. The next measurement is a
pre-registered one-variable sequential cap assessment at the next protective
tier, not a jump to full-window reasoning and not a prompt/model/policy bundle.
Full evidence is under Experiments/Generation28/.

### Slice 8: Assess The Next Protective Reasoning Tier

- Hold the exact failing text packet, bounded diagnostic, model, prompt,
  transcript, tools, repair policy, and all other budgets fixed.
- Increase only the thinking-only cap from 4,096 to 8,192 tokens.
- Measure post-failure action exit and fresh reprobe separately from assertion
  success.
- Include runtime, output-token cost, denied edits, and terminal stop class in
  the decision.

Exit condition: one valid smoke and a fixed n=3 cell reach natural harness
terminal states; at least 2/3 post-failure actions plus one fresh reprobe are
required to support the tier; no permanent policy change is made at n=3.

**Slice 8 status: complete with an action-exit signal (2026-07-18).**
Generation29 reused Generation28's contract, task, seed, expected bytes, model,
diagnostic, and runtime configuration byte-for-byte except for the registered
4,096-to-8,192 thinking-only cap change. The fixed cell observed correct
bounded diagnostics in 3/3 runs, concrete post-failure action in 2/3, and fresh
second probes in 2/3, meeting the preregistered action gate exactly. In the two
acting runs, the first mismatch progressed from byte 15 to byte 49.

The tier did not improve completed-task efficacy: assertion pass, terminal
DONE, and independent exact-file validation remained 0/3. All three runs
naturally hard-stopped in validation repair rather than at the thinking-only
stream boundary. Runtime rose to 415-1,548 seconds from Generation28's 134-172
seconds, with 25,800-86,467 observed output tokens and up to two denied write
attempts. Context pressure stayed green, and there were no manual,
environment, or shell-tool stops.

This is an Initial Replicated Cell Signal that 8,192 is a useful experimental
action-exit tier for this packet, not evidence that it should become the
default or be widened again. Before any cap policy decision, extend both the
4,096 and 8,192 arms from n=3 to n=5 with two fresh replicates per arm and
compare action exit, reprobe, exact success, stop class, and runtime
distributions. Full evidence is under Experiments/Generation29/.

### Slice 9: Test A Genuine 16K Generation Envelope

- Hold the exact text packet, bounded diagnostic, model, prompt, transcript,
  tools, repair policy, and all other budgets fixed.
- Raise `num_predict` and the thinking-only threshold together from 8,192 to
  16,384; the coupled change is necessary to expose a genuine doubled per-call
  envelope.
- Measure action exit and fresh reprobe separately from assertion, terminal,
  and independent exact-file success.
- Exclude the instrumentation smoke from the fixed efficacy count and report
  runtime, cumulative generation, write denials, and stop classes.

Exit condition: one valid smoke and a fixed n=3 cell reach natural harness
terminal states; at least one fixed-cell exact success is required to support
further 16,384 replication, while 0/3 rejects further widening for this packet.
No permanent policy change is made at n=3.

**Slice 9 status: complete; exact-efficacy threshold refuted (2026-07-18).**
Generation30 reused Generation29's text contract, task, seed, expected bytes,
model, diagnostic, and runtime settings, changing only the coupled generation
envelope from 8,192 to 16,384. Post-failure action and fresh second probes
improved from 2/3 to 3/3, and every run advanced the first differing byte.

The registered fixed cell nevertheless produced 0/3 assertion passes, terminal
DONE acceptances, and independent exact-file successes. Two runs naturally
hard-stopped in validation repair; one exhausted validation-repair call depth
after writing literal `\n` sequences. Runtime was 994-2,501 seconds, observed
output was 56,599-132,478 tokens, and denied writes reached six. Context stayed
green, with no manual, environment, or shell-tool stops.

The excluded smoke did complete exactly after seven probes and eight successful
writes in 1,791 seconds. This proves reachability at 16,384 but not repeatable
efficacy. Under the registered rule, 0/3 rejects another widening step and
removes both 8,192 and 16,384 from default-policy consideration on action-exit
evidence alone. The next slice should classify why bounded exact-file feedback
still produces structurally incorrect full-file rewrites, then test one repair
representation or measurement intervention rather than another cap increase.
Full evidence is under Experiments/Generation30/.

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
