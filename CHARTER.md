# Adaptive Agent Harness Charter

## Purpose

`~/Work/Sandbox2/Harness` is the home for a Rust project that explores a
general-purpose adaptive harness for LLM-driven work. The harness is not a
coding-agent clone and is not specific to games, Bevy, Rust, or the original
Space Invaders experiment. Its purpose is to discover, enforce, and improve the
shape of work that a given model can complete reliably.

The central research question:

> Can a harness make smaller open-weight and local models useful by adapting
> task size, context, validation, and feedback loops to the model's observed
> capability boundary?

The intended result is an orchestration runtime that gives local models narrow,
typed work packets, observes reality through deterministic checks, classifies
failures, and adapts the next step instead of simply retrying the same prompt.

## Design stance

The harness owns continuity. Models own localized reasoning.

The harness is responsible for:

- Tracking the overall goal, acceptance criteria, budgets, and stopping rules
- Decomposing work into small, valuable, independently verifiable slices
- Building bounded context packets for each slice
- Applying or rejecting model output
- Running deterministic validation probes
- Classifying failure modes
- Adapting the next action based on observed results
- Recording enough trace data to replay and compare runs

The model is responsible for:

- Diagnosing the current slice
- Proposing a bounded plan or patch
- Explaining expected validation impact
- Asking for specific missing context when needed

This split is deliberate. Smaller local models should not need to be architect,
tool protocol expert, build engineer, debugger, and validator in one pass.

## Relationship to Mojentic

The harness should be built on Mojentic Rust, whose source and docs live at:

`~/Work/Projects/Personal/mojentic-unify/mojentic-ru`

Mojentic provides the substrate:

- `LlmBroker` for model/provider abstraction
- oMLX for the current local runtime, plus Ollama and OpenAI gateways for comparison
- Structured output generation for typed model responses
- Tool abstractions where tool use is appropriate
- `AsyncDispatcher`, routers, and agents for event-driven orchestration
- `SharedWorkingMemory` for shared run state
- `TracerSystem` and correlation IDs for observability

The harness should not push problem-specific workflow into Mojentic itself.
Mojentic remains the reusable LLM and agent framework. The harness is an
application built on top of it that experiments with adaptive work routing,
feedback, and validation policy.

## Relationship to the original sandbox

The original experiment artifacts live under:

`~/Work/Sandbox/`

Do not modify that directory. Use it as evidence and regression material only.
The Space Invaders bake-off revealed useful general failure classes:

- Compile success was treated as completion even when runtime behavior failed
- Validation often checked structure instead of behavior
- Plans were not live checklists during execution
- Some models emitted stale claims after the repository state changed
- Some local/open-weight runs failed on protocol or phase-boundary issues
- Recovery loops often worked when the failing signal was precise

Those are harness design inputs, not the harness domain.

## Reference materials

Use these sources when resuming the work. They are reference material, not
inputs to copy wholesale.

Primary project references:

- `~/Work/Projects/Personal/mojentic-unify/mojentic-ru/CHARTER.md`:
  Mojentic Rust purpose and scope.
- `~/Work/Projects/Personal/mojentic-unify/mojentic-ru/README.md`:
  current feature list, architecture overview, examples, and development
  commands.
- `~/Work/Projects/Personal/mojentic-unify/mojentic-ru/AGENTS.md`:
  local Rust conventions, quality gates, feature policy, and release workflow.

Mojentic source surfaces to inspect first:

- `src/llm/broker.rs`: `LlmBroker`, gateway calls, tool loop, tracing hooks.
- `src/llm/models.rs`: message, tool-call, and gateway response types.
- `src/llm/gateway.rs`: completion configuration and provider abstraction.
- `src/llm/gateways/ollama.rs`: local-model gateway behavior.
- `src/llm/gateways/openai.rs`: hosted-model gateway behavior.
- `src/llm/tools/`: tool traits, runners, and built-in tools.
- `src/async_dispatcher.rs`: async event routing and queue lifecycle.
- `src/router.rs`: event-to-agent routing.
- `src/agents/`: existing agent patterns, including iterative problem solving.
- `src/context/shared_working_memory.rs`: shared run-state primitive.
- `src/tracer/`: trace events, event store, tracer system, and null tracer.

Mojentic public docs:

- <https://vetzal.com/mojentic-ru/>: Rust book entry point.
- <https://vetzal.com/mojentic-ru/broker.html>: broker, gateways, structured
  output entry point, and reasoning-effort configuration.
- <https://vetzal.com/mojentic-ru/observability/tracer.html>: tracer design,
  event types, correlation IDs, and query patterns.
- <https://vetzal.com/mojentic/tool_usage/>: original Mojentic tool-use model
  and tool-loop concepts.

Original experiment evidence:

- `~/Work/Sandbox/ANALYSIS.md`: cross-profile bake-off summary, costs, tool
  fingerprints, validation gaps, and process findings.
- `~/Work/Sandbox/AUDIT-qwen.md`: concrete failure evidence for stale claims,
  tool-protocol mismatch, phase-boundary drift, and structural-only validation.
- `~/Work/Sandbox/AUDIT-qwen-coding.md`: additional open-weight coding-model
  failure and recovery patterns.
- `~/Work/Sandbox/AUDIT-ollama.md` and
  `~/Work/Sandbox/AUDIT-ollama-2.md`: local-model hosting and protocol
  behavior evidence.
- `~/Work/Sandbox/AUDIT-glm.md`, `AUDIT-openai.md`, and
  `AUDIT-anthropic.md`: comparison points for successful and partially
  successful harness behavior.
- `~/Work/Sandbox/space-invaders-*`: generated repositories for later
  regression experiments. Do not modify them when building the harness.

Blog context:

- <https://stacey.vetzal.com/2026/2026-05-18-seven-coding-agents-one-space-invaders/>:
  public narrative of the bake-off and the motivation for harness-level
  validation.

## Non-goals

- Do not make a game-specific harness.
- Do not make a Bevy-specific harness.
- Do not use hopper for work.
- Do not require frontier models for normal operation.
- Do not assume model tool use is reliable.
- Do not hide validation behind model judgment.
- Do not turn validation into another free-form agent phase.
- Do not couple the harness to one repository layout or language ecosystem.

The harness itself should own its workloop and trace format.

## Core concepts

### Goal

A user-level desired outcome, expressed independently of implementation
details. A goal includes acceptance criteria and any explicit constraints.

### Work packet

A narrow unit of work generated from the goal and current run state. A packet
should be small enough that a weaker model can reason about it without needing
the whole project in context.

Minimum fields:

- Packet ID
- Goal slice
- Files or artifacts in scope, when applicable
- Non-goals
- Context summary
- Known constraints
- Validation probes
- Maximum allowed change size
- Expected model output schema

### Model output

A typed response from the model. Early versions should prefer structured output
over model-initiated tool calls, especially for local models.

Useful response types:

- `Diagnosis`
- `PlanProposal`
- `PatchProposal`
- `ContextRequest`
- `CannotProceed`

### Probe

A deterministic check the harness can run. Probes may be commands, file
assertions, process launches, API checks, screenshots, log classifiers, or
custom adapters. Probe results are facts.

### Failure classification

A typed label applied after probes run. The classifier is where adaptation
starts.

Initial failure classes:

- `CompileError`
- `TestFailure`
- `RuntimeFailure`
- `BehaviorMissing`
- `Regression`
- `OverBroadChange`
- `ContextInsufficient`
- `ProtocolFailure`
- `StaleEvidence`
- `PhaseBoundaryViolation`
- `InspectionLoop`: repeated equivalent context reads or source-view shell
  commands without a source edit, validation probe, or explicit blocker.
- `NoContentStreamRunaway`: provider metrics report large output-token
  generation while no assistant-visible content, tool calls, or surfaced
  reasoning are produced.
- `ThinkingOnlyStreamRunaway`: surfaced model reasoning consumes a large
  generation budget without transitioning to assistant-visible content, tool
  calls, or tool-call progress.
- `ValidationGap`
- `BudgetExceeded`
- `Unknown`

## Operating lessons

Generation 2 showed that prompt-only narrowing is not enough. When a narrowed
packet exists beside a broader task file, models may read the broader file and
expand scope again. The harness needs packet-owned scope controls: allowed
guidance files, allowed read/write paths, and acceptance probes recorded in the
trace.

Empty model responses should be first-class harness signals, not just another
assistant message. Tracing `agent.turn.empty_response` made the stall visible,
but continuation prompts alone did not repair the default model's behavior.
Repeated empty responses should trigger deterministic adaptation or failure.
After Ollama thinking streams became trace-visible, earlier empty-response
evidence should be read more carefully: old traces prove the model produced no
assistant-visible content and no completed tool call, but they do not prove the
model produced no reasoning. Surface reasoning tokens before deciding whether
the right policy is patience, prompt repair, no-content interruption, or a
thinking-only hard stop.

Invalid patch loops need explicit policy. A model that repeatedly emits invalid
patches after reading a truncated file should be redirected to a smaller
inspection surface or stopped with a `ProtocolFailure`; it should not consume
the remaining run by rereading the same truncated file.

Full-file reads are a weak repair primitive for large generated files. Add a
line-slice or search tool so the model can inspect and patch the exact failing
region after compiler output identifies it.

Context utilization is likely a stronger attention-fade predictor than turn
count alone. Trace approximate context size before every model call, including
message characters, tool descriptor characters, estimated tokens, optional
context-window utilization, and role counts. Use this time series when comparing
Qwen quantizations.

Context measurements are not a transcript. Every attempted worker provider call
must also record the exact ordered messages, active tool descriptors, model, and
completion settings at the provider boundary. Performance review begins with
that exact snapshot and proceeds chronologically through model output, tool
effects, harness decisions, and independent validation. If exact snapshot
coverage is incomplete, context-sensitive attribution is inconclusive; do not
infer missing follow-up verbiage from ledger sizes or policy summaries.

Task-specific reference delivery is a native initial-context assembly concern,
not framework-specific runtime policy or an appended prompt extension. Adapters
may declare bounded guidance as required, selectable, or excluded. The harness
always includes required guidance, reveals only selectable records to semantic
analysis, and prevents excluded content from entering either model packet.

Semantic micro-analysis is a proposal-only harness effect. Callers choose an
explicit advisory kind and supply a structured response schema, bounded input
and output, and an exact evidence packet. The advisory has no tools, mutation
authority, or direct runtime transition. Deterministic policy validates its
proposal before use and records the complete advisory and authority decision.
Use semantic advisories where word matching cannot distinguish meaning; use deterministic
rules wherever the relevant failure class or invariant is already known.

Single local-model runs are boundary probes, not policy verdicts. LLM behavior
is non-deterministic, and harness decisions should be based on replicated cells
and distributions: success rate, validation-reached rate, first-action rate,
thinking tokens before first action, hard-stop reason, context pressure, runtime,
and final semantic quality. A one-run pass can justify more trials; a one-run
failure can identify a failure class; neither should settle a harness policy by
itself.

The project should use a preregistered experimental method for policy changes.
Before adapting the harness, capture the observation, classification,
falsifiable hypothesis, nearest alternative explanation, fixed variables,
changed variable, replicate target, and decision rule. The standing method
lives in `EXPERIMENTAL_METHOD.md`; generation-specific plans can start from
`Experiments/experiment-plan-template.md`.

External benchmarks should evaluate the harness as an agent rather than be
translated into harness-owned tasks. The external runner must retain ownership
of task setup, isolation, limits, and independent verification. Keep benchmark
instructions unchanged, keep oracle solutions and verifier material outside
model context, and store harness traces outside the tool-visible task root. Use
a domain-neutral terminal profile for mixed terminal work so benchmark results
do not measure accidental language or framework instructions inherited from a
coding profile.

Surprising traces should trigger an adaptation embargo, not immediate policy
movement. Preserve the artifacts, inspect the model's retained context and the
analyzer output, add measurement if the distinction cannot be observed, and
change only one variable once the hypothesis is clear. Measurement-first fixes
are allowed, but they are not evidence for the policy being tested.

Small models need bounded exploration tools. The harness should provide a
directory tree scanner with depth and entry limits, and file reads should support
line ranges plus byte limits. Models should be encouraged to map the workspace
with the tree scanner and then read only the relevant line windows.

The model's filesystem view should be the work, not the experiment wrapper.
Generation 3 showed that exposing `task.md`, `notes.md`, `traces/`, or even a
parent `workspace/` directory adds avoidable cognitive load and encourages wrong
relative paths. Launch benchmark runs from the generated project workspace and
root tools at `.`; keep experiment management files outside the agent-visible
tree.

Local model progress needs observable streaming and throughput-aware patience.
Streaming model chunks made it possible to distinguish visible generation from a
silent stall during Generation 3, but Generation 4 showed that visible chunks
are not enough when a model may be generating non-visible tool-call JSON. Model
patience should be projected from observed tokens-per-second and expected output
tokens, not a fixed wall-clock cutoff. Long Rust build/test probes still need
long shell timeouts; support up to 1800 seconds for first validation runs.

Thinking-token caps should be assessed as a staged statistical sweep. A cap is a
policy tradeoff, not a claim about the model's full capability. Start with
protective caps, run replicated cells, inspect whether reasoning is novel or
looping, then widen the cap only when traces show a meaningful chance of
crossing into tool calls, visible content, validation probes, or terminal
`FAIL`. Larger caps such as 16384, 32768, or a full context-window-scale budget
are valid experiments, but they should be introduced deliberately with compute
budgets, replicate counts, and stop criteria.

### Capability boundary

The largest shape of work that a model can complete reliably under current
harness conditions. The harness should measure and refine this boundary over
time by recording which packet sizes, context shapes, probe types, and failure
classes each model handles.

## Adaptive loop

The first useful loop should be:

1. Accept a goal contract.
2. Build or refresh a compact workspace map.
3. Generate candidate work packets.
4. Select the smallest valuable next packet.
5. Build a bounded context packet.
6. Ask the selected model for typed output.
7. Validate the output against protocol and scope rules.
8. Apply the output in an isolated workspace.
9. Run deterministic probes.
10. Classify the result.
11. Adapt the next action.
12. Stop only on acceptance, explicit escalation, or budget exhaustion.

Adaptation policy should be deterministic at first:

- Compile or syntax failure: return exact diagnostics and retry once.
- Runtime failure: add logs and require diagnosis before another patch.
- Behavior missing: tighten or add a behavioral probe before retrying.
- Over-broad change: reject the patch and split the packet.
- Context insufficient: add one targeted context slice, not the whole repo.
- Protocol failure: reduce output freedom and use a stricter schema.
- Stale evidence: force a fresh probe before accepting any claim.
- Same failure class twice: split the packet or scaffold deterministic code.
- Same failure class three times: escalate to a stronger model or stop with a
  learned boundary.

## Architecture sketch

The initial Rust project should be a CLI plus library crate.

Suggested layout:

```text
Harness/
  Cargo.toml
  src/
    main.rs
    lib.rs
    cli.rs
    config.rs
    goal.rs
    packet.rs
    model.rs
    probes.rs
    classify.rs
    adapt.rs
    workspace.rs
    trace.rs
    orchestrator.rs
  examples/
    echo_goal.yaml
    command_probe.yaml
```

Primary components:

- `GoalContractor`: turns user intent into a typed goal contract.
- `WorkspaceMapper`: summarizes files, commands, docs, and project shape.
- `SlicePlanner`: proposes candidate work packets.
- `ContextPackager`: constructs bounded model context.
- `ModelWorker`: calls Mojentic `LlmBroker` and parses typed output.
- `ScopeGuard`: rejects patches or actions outside packet constraints.
- `PatchApplicator`: applies changes in an isolated copy or worktree.
- `ProbeRunner`: runs deterministic probes with timeouts.
- `FailureClassifier`: maps observed results to failure classes.
- `AdaptationPolicy`: chooses retry, split, context expansion, scaffold,
  model switch, escalation, or stop.
- `TraceRecorder`: persists events and correlation IDs for replay.

## First iteration

Start with the smallest general-purpose harness that can prove the loop:

1. Create the Rust project at `~/Work/Sandbox2/Harness`.
2. Depend on local Mojentic by path during exploration.
3. Implement typed run state, work packets, probe results, and failure classes.
4. Implement a `command` probe with timeout and captured stdout/stderr.
5. Implement a no-edit `diagnosis` mode where the model receives a failing
   probe and returns structured diagnosis.
6. Implement a simple adaptation policy over repeated command-probe failures.
7. Persist one JSONL trace file per run.
8. Add a fixture project that is intentionally trivial and language-neutral.

The first fixture should not be a game. Use something like a tiny text-file or
CLI-output task where success is observable with shell commands. The point is
to validate the harness loop before adding patch application complexity.

After that, add patch proposals and scope-checked application.

## Local-model-first experiment path

The harness should be developed with local models in the loop from the start.
Do not wait until the harness is polished before involving them.

Early local model tasks should be bounded:

- Classify a probe failure from structured logs
- Choose between two candidate next actions
- Produce a diagnosis with no file edits
- Generate a small patch for one file with exact constraints
- Request one missing context artifact from a fixed menu

Measure:

- Whether the model obeys the output schema
- Whether the diagnosis matches the observed failure
- Whether the requested context is specific
- Whether patches stay inside scope
- Whether retries improve after feedback
- How many adaptations are needed before success

This gives local models a fairer role: they are not asked to do unconstrained
autonomy; they are asked to contribute reasoning inside an adaptive control
system.

## Trace and metrics

Every run should be explainable after the fact. Record JSONL events for:

- Goal received
- Packet created
- Context packaged
- Model request and response metadata
- Schema parse success or failure
- Patch accepted or rejected
- Probe started and completed
- Failure classification
- Adaptation decision
- Stop condition

Useful derived metrics:

- Packets attempted
- Packets completed
- Failure classes by model
- Average retries per success
- Context size per packet
- Smallest successful packet size
- Largest reliable packet size
- Probe time versus model time
- Escalation frequency

The harness should eventually produce a concise capability profile per model.

## Current operating lessons

Generation 3 added several concrete loop lessons:

- A probe readiness gate must verify capability reachability in the exact
  provider request for the target domain profile. It is insufficient for a
  scripted gateway to emit a tool call that the profile did not advertise.
  Assert the active tool descriptor, execute the effect through the normal
  agent loop, and confirm its result appears in the next provider snapshot.
- Probe confidentiality must survive the full repair lifecycle, not only the
  initial packet and tool result. Store and render the stable `probe:<id>`
  identity in runtime repair state, then inspect escalation provider snapshots
  to prove executor-owned command bytes never re-enter model context.
- Analyzer and transcript terminal labels must give hard-stop events precedence
  over `run.finished` and earlier incidental validation successes. A harness
  that emits its closing event after a repair hard stop did not pass.
- For generated code work, launch the harness with cwd set to the generated
  project workspace and root tools at `.`. The model should see the project
  work tree, not experiment-management files.
- Local-model patience is token-throughput based, not fixed wall-clock based.
  Treat active but unobservable generation as `ProgressUnknown`; only intervene
  when elapsed time exceeds a projection based on expected output tokens and the
  model's conservative observed tokens-per-second profile. Sustained GPU or
  accelerator utilization from the local runner is compute-progress evidence,
  not idle time.
- A write or patch after the most recent shell probe makes the workspace dirty.
  The harness should force another deterministic validation before accepting
  completion.
- Tool-only turns are productive turns, not empty responses. Empty-response
  handling should apply only when the model produced no text and made no tool
  calls.
- Thinking-only turns are not productive assistant turns. Useful early
  reasoning should be allowed when it exits into a tool call or visible answer,
  but a large reasoning-only stream with no content, no tool call, and no
  tool-call progress is a separate failure class from both a true empty
  response and an unobservable stall.
- Harness policy comparisons should be statistical. Use at least three
  replicates for an initial cell and five or more for a serious policy decision.
  Report distributions and representative traces, not only pass/fail examples.
- Normal file-tree summaries should honor the generated workspace's
  `.gitignore`. This avoids assuming a specific stack while still hiding
  build, dependency, cache, and virtual-environment outputs that consume context
  and distract smaller models.
- Generated-code packets should prompt the model to create a
  project-appropriate `.gitignore` early unless the packet explicitly forbids
  additional files.
- Seeded benchmark workspaces should be source-only at launch. If a seed needs
  validation before the harness run, either use an external `CARGO_TARGET_DIR`
  or remove build artifacts before launch so the first `list_tree` does not
  expose `target/` before the model creates `.gitignore`.
- Shell cwd handling should match the workspace-root mental model. Treat `/`
  as the tool root, accept absolute paths that canonicalize inside the tool
  root, and reject only paths that escape the root. Local models may otherwise
  burn several turns on harmless cwd mistakes.
- Packet access rules need end-to-end descendant tests. A literal `.` rule is
  not the same as recursive workspace access; prove that representative root
  files and nested files can be listed, read, written, edited, and sensed as
  mutations before interpreting model fallback behavior. Redundant packet
  rules must not silently narrow an already-contained tool root.
- Write-budget resets should require a meaningful validation-like shell probe,
  not any successful shell command. Observation commands such as `echo`, `pwd`,
  `ls`, `cat`, `sed`, and `rg` should not clear dirty-after-edit pressure.
- Shell probes should run with pipefail enabled so validation commands piped
  through `head` or `tail` do not mask compile or test failures with status 0.
- Terminal status parsing must be token-based. Accept `DONE` as a standalone
  status line and `FAIL` as the leading status token, but never as arbitrary
  substrings inside words like `failing`.
- Long local-model patience does not apply to deterministic helper processes.
  Subprocesses such as `patch` need finite timeouts and kill-on-drop behavior so
  malformed input cannot block the whole harness.
- Timeout completion includes process-tree cleanup. A timed-out shell command
  must terminate its process group, and an externally cancelled harness must
  terminate every process in its isolated session before independent
  verification begins. Otherwise verifier results can race against surviving
  edits, training jobs, or resource consumption and are not capability
  evidence.
- Independent validation remains authoritative over harness terminal status. A
  thinking-cap stop can leave a passing artifact, while a natural `DONE` can
  leave a semantic near miss; record the artifact stage at every stop rather
  than treating stop labels as outcome labels.
- Fresh streamed content, thinking, or tool-call data is direct progress
  evidence even after a throughput projection expires. A stall requires
  distinct, repeated quiet checks; never promote a candidate twice within one
  sampling tick merely because runner telemetry is unavailable.
- More single-call thinking budget is not equivalent to more repair cycles.
  Preserve larger caps as an experimental option, but widen the default only
  when replicated traces show extra reasoning crossing into a productive stage;
  otherwise prefer a measured action-exit intervention that leaves time for
  deterministic feedback.
- A tool invocation is not necessarily artifact progress. Action-exit and
  no-action reducers should distinguish attempted writes from content-changing
  mutations, record bounded before/after fingerprints, and require a fresh
  deterministic probe after a dirty-workspace interruption. Same-size rewrites
  without identity evidence must remain a measured ambiguity, not be credited
  as semantic progress.
- Progress evidence must occur after the policy boundary it claims to satisfy.
  An environment probe or write earlier in an outer turn cannot discharge an
  action obligation opened by a later interrupted model call. Trace and reduce
  the obligation across the following opportunity, then reset it only for the
  stage-appropriate action: a real source mutation on a clean workspace or a
  fresh deterministic probe on a dirty workspace.
- Probe reach is distinct from probe relevance. A syntax check, import check, or
  self-authored test can clear dirty-workspace pressure while omitting a public
  behavioral obligation. Record obligation coverage separately, and distinguish
  repairs to the validation representation from repairs to the deliverable.
- Patch application should dry-run before making changes. A rejected hunk should
  fail without partially editing files or leaving `.orig`/`.rej` artifacts in
  the generated workspace.

- Deterministic probes are typed capabilities, not necessarily shell commands.
  Declare non-shell artifact assertions in the run contract, validate paths
  before model or mutation effects, execute them through a scoped effect, and
  feed results through the same mutation-epoch freshness and terminal policy as
  command probes. Never infer assertions by scraping task prose.
- Every emitted hard-failure runtime event needs a canonical analyzer stop
  reason and a fixture proving the mapping. An unclassified terminal event can
  invalidate an otherwise useful experiment cell even when the raw trace is
  preserved.
- Every policy decision used by an experiment's success rule must cross the
  trace adapter at the same orchestration boundary where runtime state is
  reduced. Terminal text and `run.finished` are not substitutes for a missing
  accepted-terminal event; preserve both the typed decision and its canonical
  trace name, and test the emitted trace rather than only reducer state.
- Exact-content assertions need bounded, deterministic mismatch evidence for
  repair, such as the first differing offset and short escaped excerpts. A byte
  count proves failure but does not distinguish targeted repair from blind
  rewriting; keep the full expected artifact out of worker context unless the
  contract explicitly intends to provide it.
- Treat better failure evidence as a measurement intervention, not an assumed
  repair fix. Replicate the unchanged task after adding diagnostics and record
  diagnostic delivery, post-failure mutation, fresh reprobe, and final success
  as separate boundaries. If every replicate still stops at the same
  thinking-only tier, assess the next protective cap as one changed variable.
- A higher thinking-only tier can improve action exit while worsening cost and
  leaving task success unchanged. Report the stop-class transition, successful
  and denied mutations, fresh reprobes, runtime, and cumulative output before
  calling the tier useful. An n=3 threshold signal permits replication at that
  tier; it does not justify a new default or another widening step.
- A larger provider generation envelope can strengthen diagnostic-to-action
  behavior without improving repeatable task efficacy. Count smoke runs as
  reachability and instrumentation evidence, never as fixed-cell successes;
  stop widening when the registered efficacy cell remains 0/3 despite more
  actions, probes, runtime, and output. Repeated structural exact-text failures
  should trigger a one-variable repair-representation or measurement test, not
  another reasoning-cap increase.
- For local models, distinguish hosted API cost from total experimental cost.
  Zero per-token cash charge can justify patient unattended retries, but report
  exact successes per unattended hour together with energy, thermal,
  accelerator-occupancy, and delayed-feedback costs. Retained-artifact retries
  may convert exact-text near-misses into success without widening per-call
  generation, but keep them as a bounded experimental tier until replicated on
  fresh packets. Count natural attempts and completed failed edit-to-probe
  cycles separately, stop repeated unchanged artifacts, and never interrupt an
  active attempt to enforce an outer retry budget.
- Treat acceptance planning as two different problems: inventory atomic public
  requirements, then derive interaction scenarios where satisfying one
  requirement can hide a failure in another. In the first measured planning
  cell, all five raw plans retained interruption cleanup, but only one proposed
  testing it while work was queued. A flat checklist can therefore preserve
  every obvious obligation and still encode the same semantic blind spot as the
  worker. Keep it advisory until interaction coverage is replicated.
- Small-model structured planning benefits materially from bounded repair, but
  repair is not a guarantee. Complete deterministic feedback plus
  field-preserving repair raised exact policy acceptance from 1/5 on the first
  call to 4/5 within three calls; one sample still ended on a formatting-only
  provenance error. Record attempt distributions, preserve rejected proposals,
  and fail closed before granting a checklist terminal authority.
- Isolating interaction discovery from atomic planning can eliminate protocol
  noise without solving interaction selection. In a fixed five-sample cell,
  every interaction proposal was valid on its first call and three linked
  concurrency limiting with interruption cleanup, but only one authored the
  full queued-work interruption probe. When a higher-order interaction matters,
  deterministically enumerate bounded requirement pairs and triples and ask the
  model to classify and operationalize supplied candidates. Do not let the
  model's free-form choice of interaction boundaries define completion
  coverage.
- Supplying the exact higher-order interaction improves model-authored probe
  setup but still does not make expected observations authoritative. A later
  verifier audit showed that the planning cell had promoted “cleanup for work
  that never started” into a requirement even though the benchmark expected
  cleanup only from active work while other work was waiting. Keep model-authored
  evidence advisory, and require every authoritative expected observation to
  be traceable to the task contract or an independent verifier. Generalize the
  interaction shape, not an unsupported interpretation of it.
- A protective reasoning cap should bound one model call without necessarily
  terminating the whole run. Replicated comparison traces showed immediate
  thinking-only hard stops before source edits in two of five runs and after a
  partial implementation in a third. At the cap, interrupt the call, retain the
  bounded reasoning evidence, and hand the next turn a concrete action
  obligation. Let repeated failure to act—not the first capped call—be the hard
  stop authority, and count action transitions separately from hard stops.
- Harness strictness has two independent axes. A run can be too strict about
  how long reasoning may continue while being too permissive about what counts
  as sufficient validation. Relaxing an action boundary must not weaken
  terminal evidence: interaction-specific deterministic probes and evidence
  freshness remain separate controls.
- Probe fidelity is causal, not merely topical. In the first repaired
  stock/Little-Coder/adaptive comparison, model-authored timeout cancellation
  and direct `Task.cancel()` probes repeatedly passed while a process-level
  `SIGINT` with active and waiting work failed. An isolated semantic-advisor
  call may propose the relevant external lifecycle boundary and a candidate
  probe, but the harness must preserve that boundary in execution and ground
  authoritative observations in the contract or an independent verifier.
- An action-transition label is not itself an action constraint. A three-run
  comparison converted every capped adaptive call into later source edits, but
  the continuation could still spend substantial time reasoning. Reduce the
  post-cap obligation against an actual stage-appropriate mutation or fresh
  probe, and evaluate action conversion separately from semantic success.
- Relaxed exploration and bounded effects are compatible. Stock Pi found the
  only passing cancellation implementation in one of three runs, but some of
  that trajectory was spent in repeated model-authored probes containing
  100-second sleeps. Preserve exploratory freedom while retaining finite
  subprocess deadlines, process-tree cleanup, and redundancy detection.
- Lifecycle probes must synchronize on the semantic boundary they claim to
  test. A fixed pre-signal sleep can race child startup under active agent load:
  the first declared-probe cell hard-failed a verifier-passing implementation
  after signaling at 500 ms. Wait for bounded, typed readiness evidence before
  sending interruption, then apply a separate finite completion deadline and
  record which phase timed out.
- Repair progress must mean deliverable progress. Incidental command-side
  mutations such as Python bytecode caches can trigger a generic filesystem
  mutation sensor without changing source. Filter ignored/generated paths and
  require a scoped source mutation or fresh probe before crediting a
  post-validation repair action.
- A terminal prompt is weaker than a terminal state transition. In the first
  boundary-probe cell, successful self-validation could be followed by several
  capped reasoning turns and repeated denied edits. Measure a harness-owned
  structural stop after fresh authoritative validation separately from probe
  fidelity; do not combine those interventions in one cell.
- After authoritative validation fails, its repair contract must outrank
  generic tool-only continuation and stale "validate before editing" guidance.
  Boundary Probe V5 retained the failure in exact context, but first repair
  turns received generic or conflicting follow-ups and converted neither a
  16,384-token nor 2,048-token allowance into an edit or fresh probe. A forced
  action handoff must constrain the provider request toward an allowed action;
  merely asking for action can produce another full reasoning-only call.
- External signal probes must verify signal disposition in the actual runner
  topology. A harness launched as a background shell job can pass an ignored
  `SIGINT` disposition to descendants, making a process-level interruption
  probe fail even when the independent verifier passes the implementation.
  Prove signal delivery against known-good and known-bad fixtures before using
  the probe for causal claims.
- Priority is not context hygiene. A constrained repair prompt can be the latest
  message while stale, contradictory user guidance remains in the exact
  provider request. Replace or prune superseded guidance at an authoritative
  handoff, and encode the source of repair authority so model-authored command
  failures cannot trigger a declared-probe-only policy.
- Registered repair caps must be verified at every actual provider boundary.
  A text-only continuation can remain inside a stream whose ordinary cap was
  selected before validation failed, while an outer-turn handoff immediately
  receives the smaller repair cap. Record the effective cap on each provider
  request and update repair state in-flight before treating two handoff arms as
  compute-equivalent.
- A clean action-shaped request can still be too short for a small model to
  reach action. In Boundary Probe V7, all three constrained requests removed
  stale guidance, restricted tools, and retained exact failure evidence, yet
  each consumed the full 2,048-token repair allowance without a tool call.
  Widen repair compute sequentially when replicated traces remain relevant and
  converging at the boundary; keep fresh-probe success separate from action
  conversion.
- Async cleanup requires cancellation ownership, not repeated cancellation.
  A Python 3.13 matrix showed that gather children entered cleanup after the
  first cancellation but a second cancellation interrupted the await inside
  `finally`. `TaskGroup` and an explicit drain without re-cancelling both passed;
  explicit re-cancellation and a competing signal handler failed. Evaluate
  repairs for owned child draining and use `Task.cancelling()` in lifecycle
  diagnostics when cancellation counts are causally relevant.
- A repair retry is not cumulative when an interrupted thinking stream is
  absent from the next exact provider request. Record this explicitly and do
  not interpret repeated restarts as several cycles of retained reasoning.
- A benchmark-specific probe may intentionally be stronger than the official
  verifier. When they disagree, identify the semantic boundary each accepts
  instead of treating either result as globally invalid.
- Native action-only repair can cross a boundary that repeated stateless
  thinking retries cannot. In Boundary Probe V9, two of two trials that reached
  the intervention emitted an immediate source action and fresh probe with
  zero hidden thinking; one recovered to both declared-probe and independent-
  verifier success. Keep action conversion separate from semantic recovery,
  because the other trial spent four fast repair cycles and still failed 1/6
  verifier tests. Trace provider disablement explicitly and bound repeated
  action-only cycles before considering the policy a default.
- Do not generalize native action-only into a pre-source phase boundary. Across
  Terminal-Bench and DeepSWE cells, pre-source handoffs sometimes converted
  thought into a tool request but did not establish better completed-task
  performance; the latest file-observation cell produced two failed action-only
  loops, while its most task-relevant source discovery came from the replicate
  that never entered action-only. Let ordinary reasoning and action interleave.
  Reserve native action-only for a bounded, explicitly measured retry after an
  authoritative failed probe, where Boundary Probe V9 provides the narrow
  positive signal.
- Tool-name restriction is not capability restriction when an action-only turn
  still exposes an unrestricted shell. In DeepSWE Pre-Source Action Exit V1,
  two native-disabled trials withheld read/list descriptors, but both continued
  source inspection through `sed` and `grep`; only one eventually wrote, and
  neither produced a committed patch. When policy intends to force a mutation,
  withhold arbitrary shell as a capability rather than trying to classify every
  possible command as observational or mutating.
- A zero-exit shell command is not automatically validation. The same DeepSWE
  cell classified branch creation as initial validation and treated a
  dependency inventory command as successful post-write validation after an
  import check had failed. That false terminal-readiness transition withdrew
  tools, after which the model repeated unavailable shell calls until the tool
  limit. Treat model-authored shell as opaque, non-authoritative observation;
  only executor-owned declared probes may satisfy post-write validation.
- Intervention lifecycle events must close on error paths. A scheduled or
  started intervention without either a completed or aborted event leaves its
  outcome distribution incomplete. Emit a terminal intervention event with the
  hard-stop reason and observed mutation/probe state before propagating a
  stream error.
- A contract with no declared probes has no self-validation authority. Do not
  infer one from command spelling or a zero exit status, and do not leave the
  worker trapped behind a validation gate it has no authoritative way to
  satisfy. Preserve independent verifier outcomes separately.
- Do not impose a mandatory edit cadence when validation authority is absent.
  Keep model-authored shell commands observational, measure write/check
  behaviour in traces, and rely on independent verification rather than
  rationing edits through inferred operational checkpoints. Add a bounded
  fail-safe only after measuring a demonstrated pathological repetition.
- Treat a provider response containing a very large tool-call batch as its own
  action boundary. Preserve call identifiers, indices, and arguments before
  adapting policy, because the trace must distinguish model-authored batching
  from provider-stream assembly defects.
- When provider assembly is suspect, compare exact raw response frames with the
  normalized final vector by ordered call identity and canonical argument hash,
  not by aggregate progress counts. A byte-preserving DeepSWE capture matched
  all 28 calls across 17 requests and 890 raw frames, which moved the recurring
  failure boundary back to inspection-without-action rather than ordinary
  response assembly. Do not retroactively claim exact provenance for an older
  response whose raw bytes were never captured.
- Repeated reads of one unchanged file are working-memory state, not inherently
  useful chronology. Reduce successful reads into one event-sourced canonical
  file projection per confirmed mutation epoch, while retaining the raw events
  in the trace. Make model-facing completeness explicit: a complete projection
  contains every known line, while a partial projection names exact missing
  ranges against the entire known file. Use observed tool effects rather than
  command names to distinguish read-only shell calls from mutation barriers.
  Bound rendering against the actual provider window, never merge conflicting
  overlap or content across confirmed mutations, and reject a rewrite that
  grows context.
- Provider context should retain only the newest confirmed mutation epoch for
  each canonical file view. Preserve superseded epochs in the event trace for
  audit, but do not make the model distinguish stale and current full-file
  projections in its working context.
- A canonical complete file projection can improve the inspection-to-edit
  transition without improving benchmark reward. In the fixed Igel cell it
  moved source mutation from one of three runs at turn 7 to three of three by
  turn 3, but all runs still exhausted the inner tool budget and none ran
  task-level validation. After this boundary, prioritize dirty-state-to-probe
  progress and distinguish transient writes from a retained patch.
- Dirty-state guidance intended to govern in-turn actions must be measurable at
  the provider boundary immediately after the observed mutation. However,
  advisory text alone is not a policy boundary: in the fixed Igel cell, an
  immediate prompt produced zero of three feedback-or-blocker transitions
  within three calls. One run acknowledged it in prose and resumed reading;
  another used the first three actions for repository administration. Do not
  retain a prompt-only intervention. First provision an executable semantic
  feedback capability, then separately measure a stateful action boundary that
  offers that capability or an explicit blocker. Keep undeclared checks
  separate from completion authority, and trace action categories without
  guessing shell intent.
- Self-authored tests can faithfully preserve a model's semantic
  misunderstanding. In the Qwen3.8 Igel run, 31 authored tests passed while one
  mistaken include/exclude-overlap rule caused 18 of 24 independent feature
  tests to fail. Before adding broader validation commands, measure whether an
  isolated acceptance planner can recover the public requirement interaction.
  If it can, retain that accepted plan as a compact coverage ledger, make later
  mutations stale its evidence citations, and withhold completion until every
  atomic and interaction item is addressed. Keep this explicitly weaker than
  executor-owned validation: coverage prevents forgotten requirements but does
  not prove that the worker interpreted or tested them correctly.
- A tool named as a line-range replacement must preserve line boundaries by
  default. In the Qwen3.8 file-write ergonomics matrix, the flat operation had
  zero rejected mutation calls and the fastest median runtime, but one run
  removed a Rust closing brace because replacement text omitted its trailing
  newline. When the selected range ends at a newline, preserve that boundary
  for any non-empty replacement, disclose the normalization in the mutation
  receipt, and leave EOF-without-newline and empty deletion semantics intact.
  Keep this as an isolated treatment until replicated against the current
  focused-edit control; do not add another visible mutation choice to the
  default surface merely because the primitive is easier to invoke.
- Sibling line-range calls in one provider response share one coordinate
  system. The model chose every range from the same observed file, even though
  the executor previously applied each call to the progressively mutated file.
  Group same-file sibling calls into one transaction, validate all ranges and
  optional hashes against the immutable pre-mutation snapshot, reject overlap
  before writing, apply non-overlapping ranges from bottom to top, and commit
  once. Preserve the original tool-call identities and emit an explicit atomic
  batch receipt so traces can distinguish model-authored ranges from executor
  reconciliation.
- Additive edits should not require replacing a surrounding structural block.
  After atomic coordinates removed executor-induced shift, three of five Qwen3.8
  Rust replicates still selected the `simulate` method's closing brace and
  omitted it from replacement text. Expose an isolated line-insertion treatment
  that anchors immediately before or after one visible numbered line and keeps
  the anchor unchanged. Preflight mixed sibling insertions and replacements in
  the same immutable coordinate space, reject ambiguous overlap before any
  mutation, and preserve one atomic commit and receipt. Treat this as a small
  ergonomic overlap with range replacement, not as a new behavioral constraint
  on the model.
- Do not express that overlap as another peer mutation tool. In the first
  five-replicate insertion treatment, Qwen3.8 never selected insertion for an
  initial additive change; it discovered the separate tool only twice, after a
  compiler error reduced repair to one missing brace. Present one obvious
  mutation tool with a small operation selector for create, replace, insert
  before, and insert after. On that surface, suppress legacy write and line-edit
  tools so the model does not have to choose among competing ways to change a
  file. Keep shared-snapshot atomic batching beneath the unified interface and
  require replicated first-probe improvement before promotion. The first fixed
  Rust cell met that gate: five of five independent validations passed on the
  first probe, all five runs used insertion proactively, and post-mutation reads
  fell to zero. Advance the unified grammar to broader task shapes, but first
  make system guidance surface-aware so it does not name unavailable legacy
  mutation tools.
- Cross-language evidence favors one mutation grammar, not one mandatory edit
  operation. On the Python job-failure task, five of five runs passed semantic
  validation on their first probe, but only two chose insertion; three used
  coherent replacements. Preserve both operations inside the one tool and let
  the model choose based on edit shape. Remove prompt instructions that conflict
  with observed state: if `.gitignore` already exists, guidance should not tell
  the model to create it. Keep `create` fail-closed for existing paths rather
  than restoring a whole-file overwrite fallback.
- Creating an absent file does not require a second mutation surface. In the
  corrected mixed create-and-edit cell, five of five Qwen3.8 runs used
  `edit_file` once to create the new module and used the same tool to integrate
  three existing files; every run passed its first probe and strict independent
  verification. Keep creation as one operation in the unified grammar. When a
  requested replacement is byte-identical to current content, consider an
  explicit no-op receipt rather than advancing the mutation epoch, but measure
  that behavior before changing validation-state semantics.
- Name cumulative limits after what they count. The former
  `max_tool_iterations` limit counts model interaction cycles inside one agent
  turn, not individual tool calls; expose it as `max_model_interactions` and
  retain the old spelling only as a compatibility alias. In the Qwen3.8 Igel
  run, the model's final import probe returned a new `IndentationError`, but the
  50-interaction boundary terminated before that result reached another
  provider request. Raise the default to 75 for the next measured cell. Treat
  this as additional room, not evidence that a fixed cumulative cutoff is the
  correct long-term stop policy; measure a progress-aware repair lease
  separately.

## Provider migration lesson

The oMLX migration uses the published Mojentic 2.1 release and its `OmlxGateway`.
Provider settings must be checked at the HTTP boundary: the oMLX gateway sends
`max_tokens`, omits `num_predict` and `num_ctx`, and maps disabled reasoning to
`enable_thinking: false`. Named effort levels pass through to the model template.
Keep model-native effort separate from harness thinking caps. Preserve the
resolved dependency in `Cargo.lock`, and test provider controls and tool-result
continuity when upgrading. A successful transport migration does not establish
a performance improvement. Mojentic 2.1's oMLX tool stream lacks token timing
metrics; keep that measurement gap explicit and exclude old Ollama throughput
samples from oMLX projections.

## Benchmark collection readiness

Generation35 completed model work but lost independent verification because the
workspace collection parent was absent. Test the full adapter lifecycle before
long model cells: copy a known fixture through the real collection path, preserve
its Git workspace and patch, and run the independent verifier. Create destination
parents explicitly. Keep partial downloads identifiable and refuse overwrites.
A model terminal status and an outer zero exit do not prove artifact collection
or benchmark verification succeeded.

## Open design questions

- Should the first patch representation be unified diff, file replacement, or
  structured edit operations?
- Should isolated application use copied workspaces first, then git worktrees
  later?
- How much of `WorkspaceMapper` should be deterministic before the model is
  asked to summarize anything?
- Should adaptation policy be configured as data, Rust code, or both?
- What is the minimal trace schema that is stable enough to compare runs?
- How should the harness decide when to switch models versus split smaller?
- What counts as a reusable learned capability boundary rather than one-run
  trivia?

## Immediate next actions

1. Scaffold `~/Work/Sandbox2/Harness` as a Rust CLI/library crate.
2. Add a path dependency on Mojentic Rust.
3. Define the core data types before writing orchestration logic.
4. Implement command probes and JSONL tracing.
5. Run the first local-model diagnosis loop against a trivial failing fixture.
6. Only then add patch application and scope guards.

## Binary benchmark artifact verification

Generation37 preserved the complete workspace and verifier patch, but its
coordinator compared plain git diff against Pier's git diff --binary. Binary
joblib artifacts made those byte strings differ despite matching repository
contents. Compare artifacts using the producer's exact diff options. Include
a binary file in preservation-readiness tests before long coding cells. Keep
post-run measurement failures distinct from the agent's terminal state and
independent verifier score; never restart a stopped registered cell implicitly.

## Model reasoning contracts

Generation40 separates model capabilities, provider serialization, and runtime
history policy. Select known model profiles explicitly; never infer support from
similar names or equate omitted provider defaults. Named effort must survive the
HTTP boundary unchanged. Native assistant reasoning is distinct from visible
content and from partial capped-call checkpoints. Include retained reasoning in
context accounting and preserve it deliberately during context projection.
Transport tests establish compatibility; replicated semantic benchmarks establish
whether retention helps. Prototype Git pins are temporary, with source-breaking
Rust API changes documented before a coordinated library release.

### Provider selection includes measurement and patience

Generation41 made the local provider a run-level choice. Route worker and
in-run advisory calls through the selected gateway, record its mapped controls,
and select runner telemetry and throughput history by provider as well as model.
A gateway switch with stale provider labels or another provider's throughput
history is an instrumentation defect. Verify actual HTTP requests for both
providers before attributing benchmark differences to model behavior.

### Verify whether provider activity counters overlap

Generation42 found that oMLX 0.7 counts queued requests among active output
collectors. A single owned request can report active=1 and waiting=1. Verify
counter semantics against the installed implementation before using them as an
ownership stop signal. For these verified counters, max(active, waiting)>1
indicates multiple requests; idle requires both zero. Preserve the separate
model-identity, cross-provider, admission, memory and cleanup checks. Aggregate
counts alone do not establish client identity.

## Preserve the task execution environment

Generation43's login shell selected a Python interpreter outside the task virtual
environment, producing missing-dependency errors even though the image contained
the required packages. Shell tools must preserve inherited PATH and environment.
Readiness must exercise a dependency import through the actual worker tool path;
a known-solution verifier and collection fixture alone cannot establish worker
execution readiness. Generation44's real worker fixture verifies the corrected
interpreter and imports before local model inference.

## Reasoning effort and terminal evidence

At the user's direction, Generation44 disables default ordinary and repair thinking
interruptions and compares explicit model-native medium versus low effort. Record
reasoning volume and time to action; any provider total-output exhaustion is a
truncated observation. Positive thinking interruption budgets remain explicit
experimental controls. Do not infer their necessity from older runs with omitted
native effort or a compromised execution environment.

A Harness policy stop can be emitted as run.finished with process exit zero.
Benchmark coordinators must inspect authoritative policy events and the native
analyzer, not assume that process success means task success. Cover observed
hidden-only endpoints in readiness regressions and preserve their independent
verifier results before deciding whether the batch should continue.
