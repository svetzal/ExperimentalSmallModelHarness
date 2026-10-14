# Microgentic public CLI readiness checklist

Started: 2026-10-13. Reviewed: 2026-10-14. Status: v0.1.0 published with
Homebrew installation and an ongoing streaming conversation. Remaining proposals
are identified below. See [migration evidence](MICROGENTIC-CLI-MIGRATION.md).

Build Microgentic as an adaptive coding agent optimized for small models that
need extra help staying on task. Give it a small, simple, fast interface with
selectable Mojentic gateways. Preserve bounded context, deterministic validation,
local-model patience, and evidence that explains what happened. Keep the runtime
general.

This checklist starts from the current executable and source. It does not claim
production readiness or a new model capability result. Product work must keep
registered experiments reproducible.

## Product direction

Decision recorded on 2026-10-13: **an adaptive coding agent optimized for small
models that need extra help staying on task**. The harness supplies continuity,
bounded work, relevant context, and fresh feedback so the model can make progress.

Use [Pi's coding agent](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/README.md)
as a reference for a compact terminal conversation and a small everyday control
surface. Pi documents interactive and print/JSON operation. Microgentic should
carry that simplicity through its own adaptive runtime and shell interfaces.

- **Small:** compact installation, few everyday commands, and a focused main
  view. Keep background services optional and justify new dependencies.
- **Simple:** start in a project, select a provider/model, give it a task, inspect
  activity, and review results. Keep experiment setup and policy tuning in
  documented advanced configuration rather than the everyday interaction.
- **Fast:** prompt local startup, help, input, and display response. Measure
  interface overhead separately from model loading/generation and tool execution.
  Retain throughput-aware patience for slow local models.
- **Adaptive:** keep task support, context continuity, validation, and repair
  integrated into the same session engine in terminal and automated modes.
- **User focused:** public CLI/TUI design sets the control surface. Benchmark
  adapters translate their registered runs to that surface and its documented
  advanced controls. Preserve comparable behavior and evidence as the interface
  evolves.

This sets the product promise; model reliability and speed claims still require
measurement. Gateway choice remains open, including larger models when useful.

## Foundations to retain

- One Rust executable, a reusable library, and an MIT license.
- `run --provider omlx|ollama`, provider-specific model defaults, and shared
  gateway selection for the worker and its in-run semantic advisories.
- Typed contracts, scoped file tools, ignored-path filtering, deterministic
  probes, mutation-aware validation, and bounded repair.
- Throughput-aware progress classification and retained cancellation evidence.
- Exact provider-request snapshots, context accounting, native HTML transcripts,
  and credential redaction for known provider environment variables.
- Mock gateway and recovery integration tests, plus preserved benchmark evidence.

Source starting points: [CLI](src/cli.rs), [agent](src/agent.rs),
[tools](src/tools.rs), [runtime](src/runtime.rs),
[context assembly](src/initial_context.rs), and [tracing](src/trace.rs).

## Next improvements after v0.1.0

Review recorded on 2026-10-14 in the primary product checkout, at source revision
`f5cd06cd4d15665d9dfe9aa1457cec7f14fafc57`. This order supersedes the original
implementation sequence below. The broader checklist remains the release backlog.

The published foundation includes the Microgentic name, everyday command grammar,
saved provider/model/effort defaults, ongoing streamed conversation, conditional
evidence footers, isolated benchmark commands, native release checks, and Homebrew.
Publication does not complete the broader usability gates.

This review inspected the installed command's top-level, setup, and run help.
It also inspected configuration, command dispatch, conversation entry, context
assembly, standalone planning, and trace creation. No provider or local model
was started. These findings describe interface and source behavior, not task efficacy.

### 1. Return trustworthy results

**Next implementation slice.** Give users and shell scripts a clear account of
what finished, what changed, and what was checked. The current `run` dispatcher
prints `final_summary` and returns success even when the summary reports failure.
Trace creation also uses second-resolution filenames with exclusive creation.
Rapid attempts sharing a trace directory can therefore fail before work starts.

- [ ] Create unique trace filenames without sleeps or overwriting prior evidence.
  Test rapid and concurrent creation in one directory. Remove sequential-fixture
  pacing.
- [ ] Add a typed terminal outcome at the runtime boundary. Distinguish a completed
  reply, verified task completion, incomplete work, provider/environment failure,
  and cancellation. Use recorded runtime decisions and executor results;
  a model's prose must not establish successful checks.
- [ ] Present the same outcome in plain output, JSON, and the TUI. Report changed
  paths, check results, checks not run, stop reason, and evidence location when
  relevant. Describe changes relative to the starting workspace, including
  pre-existing edits.
  An ordinary chat reply needs neither a task scorecard nor an evidence footer.
- [ ] Map public `run` outcomes to documented exit codes. Exercise returned `FAIL`,
  provider failure, exhausted budgets, unchecked completion, and cancellation
  through the installed binary. Check redirected stdout and stderr separately.
- [ ] Preserve benchmark task behavior and existing summary fields. Treat any
  benchmark exit-code change as an explicit adapter migration with reader tests.
  Compare resolved requests, controls, and terminal evidence with deterministic
  fixtures.

Start with trace uniqueness as a separate small commit, then implement the shared
outcome. Streaming JSONL and downstream cancellation follow once terminal results
have a stable meaning. Keep existing stop, validation, recovery, and patience
policies.

Source seams: [trace creation](src/trace.rs), [run summary](src/agent.rs),
[command dispatch](src/cli.rs), and [conversation events](src/conversation.rs).

### 2. Make setup lead to a working provider

The current setup saves defaults without contacting the provider. Users must know
the exact model ID and configure the endpoint through environment variables.
Worker and in-run advisories share provider selection; standalone planning still
constructs an oMLX gateway.

- [ ] Resolve one effective provider configuration for everyday commands and TUI.
  Route standalone planning through the same selection with explicit overrides.
  Keep benchmark configuration explicit and independent of personal defaults.
- [ ] Add bounded connectivity checks and actual model discovery for oMLX and Ollama.
  Let setup review the selected endpoint, model, and effort before saving.
  Help and local configuration inspection must remain offline.
- [ ] Report unavailable servers, missing models, credential problems, and unknown
  capabilities with an actionable next step. Do not infer effort support from
  an accepted flag or imply that model discovery proves tool/streaming support.
- [ ] Test endpoint/override precedence and secret redaction with scripted servers.
  Confirm that a saved choice reaches both `run` and the TUI unchanged.

Begin with the existing two providers and one saved default. Add named profiles
and hosted gateways after this workflow works; neither is a prerequisite for it.
Source seams: [preferences](src/preferences.rs), [setup](src/cli.rs),
[acceptance planning](src/acceptance_plan.rs), and
[interaction planning](src/acceptance_interactions.rs).

### 3. Understand the project and the request

Plain `run` still enters the historical task runtime; the TUI uses conversational
guidance and natural reply completion. Project instructions are currently supplied
through an explicit initial-context catalog rather than automatic `AGENTS.md` discovery.

- [ ] Specify everyday behavior for greetings, explanations, reviews, and edits
  across `run` and TUI. Keep stdin reference material distinct from instructions.
  Make any prompt or completion-policy change explicit; preserve benchmark mode.
- [ ] Add bounded hierarchical `AGENTS.md` discovery through the existing context
  assembler. Define the ancestor boundary and nested-directory applicability.
  Record paths, hashes, exclusions, precedence, and included text in trace evidence.
- [ ] Treat project guidance as input. It must not silently change credential
  destinations, grant shell permissions, or replace declared validation authority.
  Detect changed guidance between exchanges and account for context limits.
- [ ] Offer reviewable project checks without treating quoted shell commands or
  inferred checks as executor-owned acceptance probes. Report unchecked work honestly.
- [ ] Test an existing dirty repository with nested guidance, ignored files,
  follow-up requests, and explicit benchmark isolation using scripted gateways.

This slice changes model context. Register the hypothesis and comparison cell
before making capability claims; preserve exact provider requests for causal review.
Source seams: [prompt intake](src/cli.rs), [conversation](src/conversation.rs),
[context assembly](src/initial_context.rs), and [contracts](src/contract.rs).

### 4. Make coding effects reviewable

The TUI streams replies and tool activity, but it has no approval view, final diff,
or durable resume command. Agent tools still use the existing execution authority.

- [ ] Define inspection, approved changes, and autonomous execution consistently
  across CLI and TUI. Include shell, network, and validation effects.
- [ ] Retain concrete proposals for approval and reject stale workspace state.
  Show proposed changes, command effects, fresh checks, and the final diff.
  Non-interactive runs must fail clearly when required authority is missing.
- [ ] Build compact tool/check detail and understandable activity states around
  the shared outcome. Test keyboard behavior, narrow terminals, and cancellation.
- [ ] Add durable sessions after checkpoint fidelity and ownership are defined.
  Resume must preserve completed work, detect external changes, and avoid tool replay.

These are wider usability gates. Defer additional platforms, a daemon, MCP,
and extensive TUI styling until the everyday workflows above are dependable.

## Original CLI UX review: adaptive-agent-harness

The following review records the 2026-10-13 pre-UX baseline. Its findings remain
historical evidence; current implementation status is tracked above and below.

### Findings, most user pain first

1. **[§5 Safety, §10 First run] Starting work requires experiment knowledge.**
   `run` requires `--experiment` and defaults to a `task.md` goal; file tools
   actually root at process cwd. Help calls `--experiment` the tool scope,
   which disagrees with `run_agent` in `src/agent.rs`. No prompt argument,
   `setup`, or interactive session exists. Resolve an explicit project root
   once and show it before execution. Accept a natural-language request and
   construct a contract internally.
2. **[§5 Safety] Mutation authority is implicit.** `run`, `run-sequential`, and
   `render-transcript --output` have no preview/apply interface. Planning commands
   call a provider and write traces despite their planning names. Help does not
   label these effects. Define reviewable previews and `--apply`; report both
   project changes and other effects. Arbitrary shell inherits the host
   environment and is not an OS sandbox: scoping cwd does not confine its
   filesystem or network access. Make execution permissions explicit.
3. **[§6 Exit codes] Process success does not establish task success.**
   `run_agent` returns an `AgentRunSummary` with `final_summary`; `cli::run`
   prints it and returns `Ok(())` without mapping its terminal outcome to an
   exit status. Introduce a typed outcome and stable exit codes for verified
   completion, incomplete work, provider/environment failure, and cancellation.
   This finding comes from source inspection, not a new live model run.
4. **[§1 Consistency, §10 Setup] Provider choice is incomplete.** `run` and
   `run-sequential` expose `--provider`; `plan-task`, `plan-interactions`, and
   `author-interaction-evidence` construct an oMLX gateway. Connection settings
   live in environment variables. Unify gateway resolution, saved profiles,
   discovery, health checks, and credential handling across these paths.
5. **[§6 Output, §9 Feedback] Research JSON is the primary user interface.**
   The three read-only commands checked emit pretty JSON without a `--json`
   flag. There is no TUI, human result summary, or public live event stream.
   `src/main.rs` does not explicitly route logging to stderr. Add concise human
   output, clean JSON/JSONL modes, terminal detection, and live status; verify
   stdout stays parseable when dependencies emit logs.
6. **[§7 Help, §8 Errors] Help assumes experiment knowledge.** Top-level help
   has no product description or examples. Seven of nine subcommands have no
   examples. `run --help` exposes 32 long options, many for research policy.
   `resolve-contract` without arguments says only that an input is required.
   The tested typo `rnu` offers no suggestion. Put everyday workflows first,
   disclose advanced controls separately, and give errors concrete next steps.
7. **[§10 Installation] Distribution is unfinished.** README installation is
   a Cargo build; Mojentic is pinned to a Git prototype. Cargo metadata lacks
   description, license, repository, and a declared minimum Rust version.
   No tracked `.github` CI or release workflow was found. Select supported
   platforms, publish reproducible packages, and document clean removal.
8. **[§7 Completion, §12 Companion skill] Discoverability stops at help.**
   There are no shipped shell completions, setup wizard, or companion skill.
   Add them under the same executable, using shared skill-install machinery
   where available. Keep installation scope and effects visible.

### Exemplary: keep doing

- One executable with predictable kebab-case names: `run`, `analyze-trace`,
  `--trace-dir`, and `--reasoning-effort`.
- Run options defined once and reused by `run-sequential`.
- `--provider` rejects unsupported choices before execution and advertises valid
  values. Every command's help works without a provider.
- `resolve-contract` validates explicit contracts without inference.
- Structured read-only summaries, JSONL traces, and native `render-transcript`.

### Evidence and limits

`cargo build --locked` passed. All nine subcommand help pages were exercised,
along with top-level help, missing-input and invalid-provider errors,
`summarize-matrix`, `resolve-contract` with valid and invalid fixtures, and
`analyze-trace` against a preserved Generation41 mock trace. Each valid read-only
command exited zero with parseable JSON. Missing CLI arguments exited 2;
contract-resolution errors exited 1.

The [surface snapshot](docs/public-readiness/cli-surface-2026-10-13.json)
preserves commands, outputs, and the reviewed source revision. No model was
started. Mutating commands were inspected through help and source because they
have no safe preview mode. Live provider behavior and task efficacy remain
separate release checks.

## Preparation checklist

Unchecked items are proposed work. P0 establishes the public contract; P1 makes
the first public alpha usable; P2 extends it after core workflows work.

### P0: product and command contract

- [x] **Choose the name.** Product: Microgentic. Canonical command: `microgentic`.
  Offer `mg` only as an optional alias, with no default installation.
- [x] **Define the product promise.** An adaptive coding agent optimized for
  small models that need extra help staying on task, with a small, simple, fast
  experience inspired by Pi. Describe known limits and measured capabilities
  without presenting screening benchmarks as reliability guarantees.
- [x] **Choose everyday entry points.** Bare `microgentic` opens the interactive
  TUI. `microgentic setup` configures provider, default model, and reasoning
  effort. `microgentic run` requires a prompt through stdin or `--prompt`.
- [ ] **Finish grammar edges.** Define configuration overrides, prompt-source
  errors, output modes, sessions, and skill installation around these entry
  points. Keep research controls in documented advanced configuration, with an
  explicit migration period for existing scripts. Adapt benchmark adapters to
  the public UX; preserve frozen experiments through their pinned artifacts.
- [ ] **Implement the naming migration.** Update package/binary metadata, help,
  documentation, completions, and release artifacts to Microgentic/`microgentic`.
  Preserve historical evidence and document compatibility for existing
  `adaptive-agent-harness` invocations. Alias setup must be explicit and must
  not overwrite an existing command or shell alias.
- [x] **Implement the initial name and grammar.** Cargo package and canonical
  command are `microgentic`; the old executable/library remain compatible.
  Bare invocation opens the first TUI; setup previews/saves provider/model/effort.
  Existing runtime policies remain unchanged. Complete packaging/completions
  and the wider naming migration separately.
- [x] **Implement prompt intake.** Accept `run --prompt "..."` or read the prompt
  from piped/redirected stdin. Read prompt files through normal input redirection.
  Specify precedence and empty-input errors. Convert requests to the existing
  contract type without requiring users to author probe JSON.
- [ ] **Make shell composition a core contract.** Support pipelines and stdin,
  stdout, and stderr redirection without requiring the TUI. Define piped input
  roles, EOF behavior, clean result output, non-interactive execution, and broken
  pipe cleanup. Verify the shell composition cases below before public release.
- [ ] **Resolve project scope once.** Support `--dir`, default to cwd, and use
  the resolved root for tools, shell, instructions, and display. Store session
  artifacts outside model-visible project files by default.
- [ ] **Define plan/apply semantics.** Preview concrete edits and command effects;
  retain the approved proposal instead of regenerating it on apply. Planning
  must not run arbitrary mutating shell. Label provider usage and session writes.
- [ ] **Define typed outcomes.** Share one outcome between runtime, CLI, JSON,
  TUI, and exit status. Include checks passed/failed/not run, changed paths,
  stop reason, and evidence location. Test returned `FAIL` as well as errors.
- [ ] **Budget interface overhead.** Establish startup/help latency, installation
  size, idle memory, and input/render responsiveness baselines; set explicit
  release budgets from those measurements. Keep model/tool timing separate and
  avoid adding a provider call to local help or configuration inspection.

### P0: benchmark compatibility through adapters

The first migration is implemented in the maintained adapters, with an exported
patch and deterministic checks in [migration evidence](MICROGENTIC-CLI-MIGRATION.md).
The source review below describes the pre-migration interface; frozen experiments
remain unchanged. Container installation and future registered cells need the
broader gates below.

At the review baseline, the DeepSWE/Pier and Terminal-Bench/Harbor adapters under
`~/Work/Sandbox2/Benchmarks/deep-swe/adapter/adaptive_harness_agent.py` and
`~/Work/Sandbox2/Benchmarks/terminal-bench-2.1/adapter/adaptive_harness_agent.py`
launched `adaptive-agent-harness run` with `--experiment`, `--contract`,
`--trace-dir`, model budgets, and policy flags. They also inspected `run.failed`
trace records after a nonzero process exit. Harbor used `--max-tool-iterations`;
Pier used `--max-model-interactions`. These were concrete migration points
identified by source review, not newly executed benchmark results.

- [x] **Separate benchmark task mode from conversation UX.** Add
  `microgentic benchmark run` and `benchmark run-sequential` with file contracts,
  historical runtime defaults, and unconditional JSON summaries. Ignore personal
  setup, stdin, and TUI history. Keep existing advanced launch forms available.
  Maintained Pier and Harbor adapters now select this mode explicitly. Compare
  provider requests and context ledgers with the pre-UX source before release.
  See [benchmark mode evidence](MICROGENTIC-BENCHMARK-MODE.md). Container installation
  and live efficacy measurements remain under the broader gates below.
- [ ] **Give rapid attempts distinct trace filenames.** The deterministic
  sequential fixture exposed a pre-existing collision when two attempts start
  within one second. Preserve atomic trace creation while making each filename
  unique. Fix this separately from benchmark mode preservation, then remove
  fixture pacing and check rapid sequential attempts through the public binary.
- [ ] **Define the adapter boundary before changing commands.** Map benchmark
  task input, project root, provider/model, permissions, probe contract, budgets,
  context policy, output, trace destination, and cancellation to the public
  non-interactive interface. Preserve necessary research controls in documented,
  versioned advanced configuration. Keep one runtime execution path.
- [ ] **Migrate maintained launchers and tests.** Update Pier, Harbor, generation
  runners, and collection/analysis integrations for `microgentic` and the new
  flags/configuration. Supply explicit provider settings and authorized execution
  policy; benchmark runs must not enter setup, the TUI, or an approval prompt.
- [ ] **Consume structured outcomes.** Parse documented JSON/JSONL and typed
  terminal status, with trace evidence for diagnosis. Distinguish incomplete work,
  provider failure, environment failure, cancellation, and verified success.
  Preserve batch-stop rules and independent verifier authority.
- [ ] **Prove equivalent resolved runs.** Compare old/new intake and effective
  contracts, model controls, context packets, tool/probe exposure, permissions,
  and stop policies using deterministic fixtures and scripted gateways. Inspect
  exact provider snapshots for model-visible equivalence; do not infer it from
  matching flag names or token counts. Classify intentional changes separately.
- [ ] **Preserve collection and observability.** Retain exact request/context
  evidence, workspace and binary patches, original-test protection, verifier
  isolation, terminal lifecycle, and native transcript generation. Trace and
  result schema changes need explicit versions and reader compatibility.
- [ ] **Pin adapter provenance.** Register binary/version/hash, adapter revision,
  effective configuration, environment, provider, model, and schemas with every
  new cell. Leave historical run scripts, binaries, registrations, and scores
  unchanged. Document any temporary CLI aliases and their retirement plan.
- [ ] **Gate migration before fresh trials.** Pass adapter integration tests
  through the installed public binary: task delivery, cwd, redirection, outcome
  decoding, interruption, cleanup, and collected verifier evidence. Freeze new
  adapter artifacts before registering live replicates; repeat semantic efficacy
  measurement if migration changes model context or runtime policy.

### P0: gateway selection and configuration

- [ ] **Centralize gateway resolution.** Worker, advisory, acceptance planning,
  and standalone commands must use the resolved profile. Show any override in
  the interface and trace.
- [ ] **Add named profiles.** Separate profile name, gateway type, endpoint,
  model, default reasoning effort, credential reference, and capabilities.
  Define and test precedence: flags, environment, project config, user config,
  defaults.
- [ ] **Make setup reach a working state.** Select a gateway, test connectivity,
  list actual models, choose the default model and a supported reasoning effort,
  and review/save effective settings. Persist these defaults for TUI and `run`;
  show explicit per-run overrides and reject unsupported effort choices. Use a
  credential store or environment reference; mask secrets in output and
  diagnostics.
- [x] **Save the initial everyday defaults.** Setup reviews/saves provider,
  model, and effort; TUI and ordinary `run` read those defaults. Endpoints and
  credentials remain environment settings. Connectivity and model discovery
  are still pending.
- [ ] **Establish the support matrix.** Retain oMLX and Ollama first. Assess the
  OpenAI gateway in the pinned Mojentic dependency as the next integration.
  Treat custom OpenAI-compatible endpoints as separately tested profiles;
  protocol similarity does not prove capability parity.
- [ ] **Validate capabilities explicitly.** Cover tool calls, structured output,
  streaming, reasoning controls, context limits, cancellation, and recovery.
  Reject unsupported combinations with a next step. Key throughput history to
  provider, endpoint, model, and context band.
- [ ] **Make hosted behavior visible.** Show source/context destination, usage,
  and costs only when known. Keep unknown metrics visibly unknown. Extend
  redaction to all supported credential sources and export paths.

The inspected Mojentic prototype provides `OpenAIGateway` with configurable
endpoint and credentials. The harness selects only oMLX and Ollama; adding a
gateway still needs recovery, reasoning, telemetry, and tool-history conformance.

### P1: shared session engine

- [x] **Retain native TUI conversation context.** Follow-ups receive prior user
  prompts, completed assistant responses, and tool request/result pairs. Keep
  task-scoped system/probe/repair instructions separate. Scope stored history to
  project and model settings; new TUI invocations and benchmark runs stay isolated.
  Bound history by dropping complete oldest exchanges and report the omissions.
- [ ] **Create a presentation-neutral session API.** Expose typed state, events,
  commands, and outcomes to CLI and TUI. Keep provider calls, validation,
  context policy, and stop authority in the runtime. Adapt existing trace events
  rather than parse display strings or build another work loop.
- [ ] **Support follow-up requests.** Carry corrections into the current task,
  preserve completed work and acceptance criteria, and acknowledge steering.
  Define when a message changes the goal or starts a new task.
- [ ] **Persist recoverable sessions.** Store unique IDs, resolved config,
  contract, conversation/context state, pending approvals, mutation epoch,
  probes, and outcome. A trace or final summary alone is not a resume checkpoint.
  Avoid timestamp collisions for concurrent starts.
- [ ] **Make resume safe.** Detect external file changes, invalidate stale checks,
  and avoid replaying completed tools. Refuse uncertain active-provider ownership.
  Define interruption and process-tree cleanup during planning, tools, checks,
  and recovery admission, as well as worker generation.
- [ ] **Discover project guidance.** Add bounded hierarchical `AGENTS.md`
  discovery through the context assembler. Document scope, precedence, trust,
  and exclusions; trace paths, hashes, and inclusion decisions.
- [ ] **Support existing repositories.** Preserve dirty files and baseline
  stashes; show a diff before discard. Make test/build selection reviewable and
  distinguish inferred checks from user-declared acceptance probes.

### P1: friendly TUI

- [x] **Start with conversation.** No engineering work is assumed at launch or
  for an ordinary greeting/question. Preserve messages literally and let the
  conversation request work. Use natural reply completion without DONE/FAIL;
  keep quoted commands separate from executor-owned acceptance probes.
- [x] **Show evidence with project work.** Display the trace footer when the
  current exchange uses project tools, including read-only inspection. Omit it
  from ordinary chat and later conversational replies. Keep all traces saved.
  Use structured tool activity to decide visibility; do not classify prompt text.
- [x] **Stream the ongoing conversation.** Display full reply chunks as they
  arrive, thinking/progress status, and tool activity through a private event seam
  on the existing runtime. Permit drafting during generation. Forward cancellation
  to the owned worker, retain completed context, and label unfinished replies.
  Deterministic PTY checks cover both gateways; broader resume/steering stays below.
- [x] **Implement launch behavior.** Bare `microgentic` opens the conversation
  TUI when stdin and stdout are terminals. `run` uses the plain interface even
  on a terminal. Missing prompts and redirected bare invocations return clear
  usage errors without entering the TUI or waiting for hidden input.
- [ ] **Design one calm main view.** Use a compact project/provider/model/effort
  header, conversation, and multiline input. Group expandable tool activity
  under its action. Keep detailed context and trace accounting in secondary
  views. Review the everyday flow against the Pi-inspired small, simple, fast
  product direction.
- [ ] **Make state understandable.** Label connecting, generating, inspecting,
  editing, checking, awaiting input, and stopped. Distinguish unknown progress
  from stalled generation. Refresh elapsed time without invented percentages or
  shorter local-model patience.
- [ ] **Expose meaningful decisions.** Offer provider/model selection, proposed
  change review, check output, and final diff. Count changed files and passing
  checks; label unchecked outcomes clearly.
- [ ] **Specify keyboard behavior.** Define send versus newline, history, scroll,
  help, and cancellation. Preserve input during resize. Define Ctrl-C and a
  second interrupt; restore the terminal after exit, errors, or panic.
- [ ] **Test terminal accessibility.** Use consistent spacing, visible focus,
  readable light/dark themes, monochrome status labels, `NO_COLOR`, Unicode-safe
  wrapping, and narrow layouts. Offer screen-reader-friendly plain mode; verify
  paste, copy/selection, SSH, and common terminal emulators.
- [ ] **Choose the TUI library after the event seam.** Evaluate Rust terminal
  libraries against these needs. Keep rendering responsive while providers and
  tools are slow. Display backpressure must not alter stop or validation policy.

### P1: permissions and data boundaries

- [ ] **Specify execution modes.** Define read-only inspection, approved edits,
  and autonomous execution, including network and shell effects. `--yes` skips
  prompts within the chosen policy; `--force` must not imply broad authorization.
- [ ] **Separate file scope from process confinement.** Choose sandbox support
  per platform or clearly identify trusted host execution. Check symlinks,
  descendants, environment access, and out-of-root shell effects. Apply the same
  rules to declared validation probes.
- [ ] **Protect local records.** Document config/state/cache locations, file
  permissions, retention, removal, and sharing. Separate exact research traces
  from redacted exports and label non-exact evidence.
- [ ] **Treat repository content as input.** Define trust for project settings,
  instructions, commands, and external tools. Prevent untrusted configuration
  from silently changing credential destinations or execution permissions.

### P1: CLI finish and distribution

- [ ] **Finish shared output options.** Implement `--json`, JSONL events,
  `--quiet`, and `--verbose` consistently. Results go to stdout; progress and
  diagnostics go to stderr. Test the shell composition contract, non-TTY runs,
  and `NO_COLOR`; output redirection must not change the task's meaning.
- [ ] **Make help teach the workflow.** Add summaries, examples at every level,
  mutation labels, typo suggestions, and actionable errors. Disclose research
  settings progressively.
- [ ] **Ship completions and a companion skill.** Supply completions through
  setup. Provide `init` for globally scoped guidance, with `--local` for project
  scope and concrete installation previews. Use shared install infrastructure.
- [x] **Promote the project home.** Rename the upstream to `svetzal/microgentic`.
  Use `~/Work/Projects/Personal/microgentic` for product development and keep
  `~/Work/Sandbox2/Harness` as a separate benchmark integration clone.
- [x] **Add the initial release pipeline.** Build and test versioned packages
  for Apple Silicon, Intel Mac, and Linux x86-64. Embed source revision provenance,
  preserve binary and archive checksums, and check a copied executable while its
  source checkout is unavailable. Publish through `svetzal/tap`; see `RELEASING.md`.
- [ ] **Complete standalone packaging.** Move from the locked Mojentic Git
  revision to a compatible released dependency after gateway checks pass.
  Expand packages to Windows and Linux ARM through native platform gates.
- [ ] **Document the lifecycle.** Cover install, first task, provider switch,
  change review, interruption/resume, troubleshooting, and uninstall. Add
  contribution, issue, security-reporting, and compatibility policies.
- [x] **Automate initial release checks.** Add CI for formatting, tests, lint, CLI
  contracts, gateway mocks, packaging, and terminal lifecycle. Test the installed
  artifact on native runners and name unsupported platforms openly. Additional
  gateway packaging and compatibility expansion remain preparation work.

### P2: extensions after a usable alpha

- [ ] **Add integrations from observed needs.** Prioritize external tools/MCP,
  reusable skills, editor handoff, attachments, and context selection. Each must
  use the existing permission and evidence seams.
- [ ] **Add a daemon when background work requires it.** Foreground sessions can
  start without one. Detached work, queues, or schedules need a fast client,
  separate daemon, persisted ownership, and `status`/watch operations.
- [ ] **Publish measured capability boundaries.** Keep Qwen comparisons
  controlled. Add real-repository workflow cells with three initial replicates,
  and five or more before boundary claims. Separate provider errors and
  environment-invalidated runs from model outcomes.

## Shell composition contract

Decision recorded on 2026-10-13: Microgentic must participate fully in standard
shell operations. Pipes and redirection are supported workflows alongside the
TUI, with the same session engine, permission rules, validation, and outcomes.
The behaviors and examples in this section are requirements for implementation.

### Input

- `microgentic run --prompt "..."` supplies the task explicitly. With no
  `--prompt`, `run` reads its prompt from piped/redirected stdin through EOF.
  A prompt file uses `microgentic run < prompt.txt`; no separate task-file flag
  or positional prompt is needed in the everyday grammar.
- If `--prompt` is provided with piped/redirected stdin, the directive is the
  instruction and stdin is labeled reference material, such as a diff or build
  log. Never silently replace the explicit prompt or treat reference material
  as execution policy. Read stdin once.
- `run` with terminal stdin and no `--prompt` fails immediately with examples
  of both supported forms. Reject empty/whitespace-only prompts, including an
  empty explicit `--prompt`; do not fall back to another source silently.
- A bare `microgentic` opens the TUI when stdin and stdout are terminals. Bare
  invocations with redirected input/output return a usage error directing the
  user to `run`; do not infer a batch task or open `/dev/tty`.
- Read task/reference input once through EOF. Specify size and encoding limits;
  report unsupported or oversized input rather than silently truncate it.
  Empty reference input is valid when a task exists; an empty task is an error.
- Child tools must not consume the caller's stdin. Close or explicitly supply
  their input so shell commands cannot steal the task stream or block on it.

### Output and interaction

- Stdout contains the requested result: final response text by default, one
  structured result with `--json`, or JSONL events with `--json --stream`.
  Document event schemas and how partial output relates to terminal failure.
  End textual records with a newline.
- Progress, warnings, provider diagnostics, and tool logs go to stderr. Tool
  output enters stdout only when it is deliberately part of the requested
  result. `--quiet` suppresses optional chatter while preserving errors.
- When either stdin or stdout is not a terminal, use plain non-interactive mode.
  Disable the TUI, control sequences, color, and prompts. Respect `NO_COLOR` in
  terminal mode too. Never open `/dev/tty` to work around piped input or redirects.
- Missing credentials, configuration, or required permission must produce an
  actionable error and nonzero exit rather than a prompt. An explicit `--apply`
  and selected permission policy govern mutations; `--yes` does not grant new
  execution authority. Read-only requests must remain usable in pipelines.
- Redirection to files, append redirection, stderr capture, and downstream
  consumers must preserve result content and truthful exit status. Detect write
  failures and a closed downstream pipe, cancel remaining work, and clean up
  provider/tool ownership. Preserve appropriate failure or signal status for
  shell scripts, including pipelines run with `pipefail`.

### Proposed examples and acceptance checks

The basic entry points, prompt intake, text output, and JSON summaries are
implemented. `status`, `--stream`, and richer session commands remain proposals;
those examples are future acceptance targets.

```sh
microgentic
microgentic setup
echo "Hello!" | microgentic run
microgentic run --prompt "Explain this project"
microgentic run < task.txt > result.txt
git diff --no-ext-diff | microgentic run --prompt "Review this diff" > review.md
microgentic run --prompt "Explain this build failure" < build.log > diagnosis.txt
microgentic run < task.txt > result.txt 2> diagnostics.log
microgentic status --json | jq '.outcome'
microgentic run --json --stream < task.txt > events.jsonl
microgentic run --prompt "Summarize the public API" >> summaries.txt
```

- [ ] Verify input-only, output-only, and combined redirection; pipes into and
  out of the agent; append output; and separate stderr capture.
- [ ] Verify prompt-only stdin and `--prompt` plus reference stdin remain
  distinct, including multiline Unicode, empty input, and input-size failures.
  Reject positional prompts, duplicate explicit prompts, and missing prompts;
  verify redirected bare invocations direct users to `run`.
- [ ] Verify stdout is clean under verbose provider/tool activity; parse the
  complete JSON result and every JSONL event independently.
- [ ] Verify non-interactive runs never prompt, launch the TUI, or read from
  `/dev/tty`; test missing credentials and permissions plus stdin from `/dev/null`.
- [ ] Verify slow producers/consumers, early downstream exit, failed writes,
  SIGINT/SIGTERM, partial stream failure, and `pipefail` outcomes. Confirm no
  orphaned model request or tool process remains after termination.
- [ ] Compare terminal and redirected runs with scripted gateways: task intake,
  permitted effects, check evidence, and outcome must agree. Test actual shell
  pipelines and the installed binary, not only in-memory input/output mocks.

## Proposed command map

Decision recorded on 2026-10-13: bare `microgentic` opens the TUI, `setup`
configures provider/default model/effort, and `run` requires stdin or `--prompt`.
The three entry points are implemented. `--apply` currently saves setup only;
run preview/apply and additional commands below remain proposals.

| Command | User purpose |
| --- | --- |
| `microgentic` | Open a terminal conversation in the current project |
| `microgentic setup` | Configure provider, default model, and effort level |
| `microgentic setup --apply` | Save the reviewed setup |
| `echo "Hello!" \| microgentic run` | Read the prompt from stdin in plain mode |
| `microgentic run --prompt "Fix the parser" --dir .` | Run the task in the selected project under the current runtime policy |
| `microgentic run < task.txt` | Read the prompt from a file through stdin |
| `microgentic run --prompt "Fix the parser" --dir . --apply` | Apply the retained proposal and permitted checks |
| `microgentic provider list` | Show configured profiles and destinations |
| `microgentic provider edit local --apply` | Apply a reviewed profile change |
| `microgentic model list --provider local` | Discover models for a profile |
| `microgentic status --json` | Report session outcome and check state |
| `microgentic session list` | Find previous work |
| `microgentic session show SESSION_ID` | Inspect conversation, changes, and evidence |
| `microgentic session resume SESSION_ID` | Reopen work; require approval for further effects |
| `microgentic init --apply` | Install reviewed companion guidance globally |
| `microgentic init --local --apply` | Install reviewed guidance in this project |

TUI approval and CLI `--apply` must authorize the same retained proposal. Bind
non-interactive approval to a session/proposal ID and reject stale workspace
state. Repeating a prompt cannot prove the generated plan is identical.

## Naming decision

Decision recorded on 2026-10-13:

- Product name: **Microgentic**.
- Canonical executable and documented command: **`microgentic`**.
- Optional user-selected shell alias: **`mg`**.

`mg` already names an [Emacs-style editor](https://man.openbsd.org/mg) and resolves
to `/usr/bin/mg` on the reviewed Mac. Do not ship or install an `mg` executable
by default. Users who want the shortcut can explicitly choose an alias:

```sh
alias mg=microgentic
```

Use `microgentic` in official examples and automation so they work independently
of a user's aliases. The Cargo package and primary executable now use the chosen
name, with the old executable retained for compatibility. The upstream is now
`svetzal/microgentic`; v0.1.0 is published through `svetzal/tap`. Brand/domain
availability remains separate preparation work. Preserved CLI review evidence
keeps its original names.

## First implementation slices and release gate

The sequence below records the initial plan. Use the post-v0.1.0 priorities above
for the next product work. The remaining usability gate still applies.

1. **Command/session contract:** project root, intake, permission semantics,
   typed outcomes, shell composition, output channels, and adapter migration.
   Verify fixtures, scripted gateways, and benchmark equivalence before calling
   a local model.
2. **Provider setup:** shared profile resolver, discovery, diagnostics, and
   persistent config. Route standalone planning through it too.
3. **Conversation TUI:** session events, readable progress, input, steering,
   and change/check review. Add durable resume after state fidelity is tested.
4. **Installable alpha:** packages, completions, companion skill, lifecycle docs,
   interface performance budgets, migrated benchmark adapters, and measured
   workflows on declared supported platforms.

A public alpha should let a new user install it, configure either supported
local provider, open an existing repository, request a change, inspect and approve
effects, see fresh checks and a diff, interrupt safely, and recover preserved work.
The same task must work through the plain CLI, pipelines, and redirected input
and output with clean results, parseable machine modes, and truthful exit status.
Publish the remaining limits alongside that alpha.

The alpha must also complete a deterministic benchmark-adapter smoke run through
the public non-interactive interface, preserving registered inputs, run controls,
trace evidence, workspace collection, and independent verification. User-facing
UX improvements and benchmark compatibility must ship together.

Keep sampling, transcript retention, patience, validation, and recovery policy
fixed during interface-only slices. If a feature changes model context or runtime
decisions, register the hypothesis and a measured cell under the experimental
method before making efficacy claims.
