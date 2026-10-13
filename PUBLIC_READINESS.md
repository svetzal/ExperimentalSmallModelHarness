# Public CLI readiness checklist

Date: 2026-10-13. Status: preparation; proposed commands are not implemented.

Build a capable CLI coding agent on the adaptive runtime, with a friendly
terminal interface and selectable Mojentic gateways. Preserve its distinguishing
strengths: bounded context, deterministic validation, local-model patience, and
evidence that explains what happened. Keep the runtime general.

This checklist starts from the current executable and source. It does not claim
production readiness or a new model capability result. Product work must keep
registered experiments reproducible.

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

## CLI UX review: adaptive-agent-harness

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

- [ ] **Choose the promise and name.** Position it as an adaptive coding agent
  with strong local-model support. Describe supported workflows and known
  limits without presenting screening benchmarks as reliability guarantees.
- [ ] **Define everyday grammar.** Use one executable for `setup`, `run`,
  `status`, provider/model selection, sessions, and skill installation. Keep
  experiment controls available through an advanced/research surface, with an
  explicit migration period for existing scripts and benchmark adapters.
- [ ] **Define task intake.** Accept a quoted request, stdin, and an explicit
  task file. Specify precedence and empty-input errors. Convert requests to the
  existing contract type without requiring users to author probe JSON.
- [ ] **Resolve project scope once.** Support `--dir`, default to cwd, and use
  the resolved root for tools, shell, instructions, and display. Store session
  artifacts outside model-visible project files by default.
- [ ] **Define plan/apply semantics.** Preview concrete edits and command effects;
  retain the approved proposal instead of regenerating it on apply. Planning
  must not run arbitrary mutating shell. Label provider usage and session writes.
- [ ] **Define typed outcomes.** Share one outcome between runtime, CLI, JSON,
  TUI, and exit status. Include checks passed/failed/not run, changed paths,
  stop reason, and evidence location. Test returned `FAIL` as well as errors.

### P0: gateway selection and configuration

- [ ] **Centralize gateway resolution.** Worker, advisory, acceptance planning,
  and standalone commands must use the resolved profile. Show any override in
  the interface and trace.
- [ ] **Add named profiles.** Separate profile name, gateway type, endpoint,
  model, credential reference, and capabilities. Define and test precedence:
  flags, environment, project config, user config, defaults.
- [ ] **Make setup reach a working state.** Select a gateway, test connectivity,
  list actual models, select one, and show effective settings. Use a credential
  store or environment reference; mask secrets in output and diagnostics.
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

- [ ] **Define launch behavior.** The bare executable opens a conversation when
  stdin and stdout are terminals. Provide `--no-tui`; pipes, `--json`, and
  automation use a plain interface without interactive prompts.
- [ ] **Design one calm main view.** Use a compact project/provider/model header,
  conversation, and multiline input. Group expandable tool activity under its
  action. Keep detailed context and trace accounting in secondary views.
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
  diagnostics go to stderr. Test pipes, non-TTY runs, and `NO_COLOR`.
- [ ] **Make help teach the workflow.** Add summaries, examples at every level,
  mutation labels, typo suggestions, and actionable errors. Disclose research
  settings progressively.
- [ ] **Ship completions and a companion skill.** Supply completions through
  setup. Provide `init` for globally scoped guidance, with `--local` for project
  scope and concrete installation previews. Use shared install infrastructure.
- [ ] **Package a standalone product.** Set Cargo metadata and minimum Rust
  version, use a suitable released Mojentic dependency, declare platform support,
  and publish versioned packages with checksums. Embed version/revision provenance;
  installed binaries cannot rely on the build machine's Git checkout through
  `CARGO_MANIFEST_DIR`.
- [ ] **Document the lifecycle.** Cover install, first task, provider switch,
  change review, interruption/resume, troubleshooting, and uninstall. Add
  contribution, issue, security-reporting, and compatibility policies.
- [ ] **Automate release checks.** Add CI for formatting, tests, lint, CLI
  contracts, gateway mocks, packaging, and terminal lifecycle. Test the installed
  artifact on a clean machine and name unsupported platforms openly.

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

## Proposed command map

`agent` is a placeholder, not a chosen binary name. These commands do not exist
yet. Review the final grammar before implementation.

| Command | User purpose |
| --- | --- |
| `agent` | Open a terminal conversation in the current project |
| `agent setup` | Preview connection and configuration setup |
| `agent setup --apply` | Save the reviewed setup |
| `agent run "Fix the parser" --dir .` | Inspect and propose a concrete change |
| `agent run "Fix the parser" --dir . --apply` | Apply the retained proposal and permitted checks |
| `agent provider list` | Show configured profiles and destinations |
| `agent provider edit local --apply` | Apply a reviewed profile change |
| `agent model list --provider local` | Discover models for a profile |
| `agent status --json` | Report session outcome and check state |
| `agent session list` | Find previous work |
| `agent session show SESSION_ID` | Inspect conversation, changes, and evidence |
| `agent session resume SESSION_ID` | Reopen work; require approval for further effects |
| `agent init --apply` | Install reviewed companion guidance globally |
| `agent init --local --apply` | Install reviewed guidance in this project |

TUI approval and CLI `--apply` must authorize the same retained proposal. Bind
non-interactive approval to a session/proposal ID and reject stale workspace
state. Repeating a prompt cannot prove the generated plan is identical.

## Naming direction

**Mojent** is the leading working candidate: short to type, tied to Mojentic,
and broad enough to cover coding without defining the product by model size.
**Mojentic Code** is the descriptive alternative if the family name matters more
than a distinct product name. Neither is adopted by this document.

Try realistic commands, then check pronunciation, searchability, executable and
package collisions, repository names, and brand/domain availability. A quick
web search on the review date did not establish a clear Mojent conflict, but it
is not name clearance. Two appealing generic alternatives overlap with AI tools:
[Mettle](https://github.com/Creed-Space/METTLE) and
[Trellis](https://github.com/e-onux/trellis). Keep them off the leading list.

## First implementation slices and release gate

1. **Command/session contract:** project root, intake, permission semantics,
   typed outcomes, output channels, and compatibility. Verify fixtures and
   scripted gateways before calling a local model.
2. **Provider setup:** shared profile resolver, discovery, diagnostics, and
   persistent config. Route standalone planning through it too.
3. **Conversation TUI:** session events, readable progress, input, steering,
   and change/check review. Add durable resume after state fidelity is tested.
4. **Installable alpha:** packages, completions, companion skill, lifecycle docs,
   and measured workflows on declared supported platforms.

A public alpha should let a new user install it, configure either supported
local provider, open an existing repository, request a change, inspect and approve
effects, see fresh checks and a diff, interrupt safely, and recover preserved work.
The same task must work through the plain CLI with parseable output and truthful
exit status. Publish the remaining limits alongside that alpha.

Keep sampling, transcript retention, patience, validation, and recovery policy
fixed during interface-only slices. If a feature changes model context or runtime
decisions, register the hypothesis and a measured cell under the experimental
method before making efficacy claims.
