# Microgentic CLI migration

Date: 2026-10-13. Baseline source: `bfe3529`.

This note records the initial implementation committed as `84b6fe4`. The later
[TUI conversation slice](MICROGENTIC-CONVERSATION.md) adds retained native history
and streamed presentation; the independent-task limitation below is historical.

## Scope and decision

Implement the Microgentic name and initial everyday grammar around the existing
runtime. Keep model sampling, budgets, prompts for registered contracts, tool
execution, validation, context retention, recovery, and stop policy intact.

The Cargo package and primary executable are `microgentic`. A temporary
`adaptive-agent-harness` executable retains old flags and JSON output. The
library import stays `adaptive_agent_harness`. `mg` remains a user-selected
shell alias; installation does not create it.

## Implemented interface

- `microgentic`: a terminal view with task input, defaults, results, elapsed
  time, and trace locations. Enter sends; Alt-Enter adds a newline; Page Up/Down
  scroll; Ctrl-C requests worker cancellation; Esc exits when idle.
- `microgentic setup`: preview provider, default model, and effort settings.
  The terminal wizard prints the exact command to save the selected values.
  `--apply` writes the reviewed configuration atomically. Explicit flags also
  support scripts; `--json` exposes the before/after configuration.
- `microgentic run`: read a UTF-8 prompt from stdin through EOF, or accept
  `--prompt`. `--dir` sets the actual tool cwd. Text output contains the native
  final summary; `--json` emits the existing full summary. Logs use stderr.
- With `--prompt`, redirected stdin becomes labeled reference material.
  Probe declarations come from the explicit instruction, so reference shell
  fences cannot silently acquire acceptance authority.

User defaults live under `$XDG_CONFIG_HOME/microgentic/config.json`, with
`~/.config` as the fallback base. `--config` selects another file. Run flags
override saved defaults. Advanced contract/experiment runs and compatibility
invocations do not inherit personal settings; their explicit controls retain
their previous meanings.

Normal prompt runs retain input files and traces under
`$XDG_STATE_HOME/microgentic/runs`, with `~/.local/state` as the fallback base.
Each run gets a unique directory outside the tool root. Stdin has a 1 MiB limit;
empty prompts, invalid UTF-8, duplicate directives, and positional prompts fail
before contacting a provider.

## Runtime boundary

The frontend prepares task files and `AgentRunConfig`, then calls the existing
`run_agent` or `run_sequential`. The TUI starts an ordinary public CLI worker
and forwards cancellation to it. It waits for the worker to settle and restores
the terminal on normal exit or errors. It does not invent a model timeout.

No changes are made to `agent.rs`, `runtime.rs`, `tools.rs`, profile code,
contract resolution, context assembly, or recovery. An authoritative benchmark
contract keeps its guidance exactly: supplying `--prompt` with `--contract`
requires byte equality and rejects additional stdin reference content.

The initial TUI submits independent tasks. Previous displayed messages are not
yet replayed as conversation context. Full session continuity, live tool views,
provider discovery, new permission modes, JSONL streaming, and typed outcome
exit codes remain preparation work. Run still permits the existing editing and
shell behavior; no preview/apply policy was introduced for agent work.

## Benchmark adapters

Maintained Pier/DeepSWE and Harbor/Terminal-Bench adapters now install and invoke
`microgentic run --prompt ... --dir . --json`. They retain the authoritative
contract, trace destination, model ID, budgets, transcript policy, guidance
catalogs, cancellation, and workspace collection. They use canonical
`--max-tokens` and `--max-model-interactions` names; the old aliases still parse.

The adapters now explicitly select their documented Ollama route, with an
optional provider setting for oMLX. The old adapters required Ollama-prefixed
model IDs but omitted the provider flag; the current runtime defaults to oMLX.
Making that route explicit is an adapter correction, not proof that every
historical launch used the intended provider. Historical registrations and
scores are unchanged. New provider selections must be registered explicitly.

The build export includes both executables and their checksums, so existing
local runner paths continue to find the compatibility binary. Frozen generation
scripts, manifests, historical builds, and benchmark results are unchanged.
Use pinned adapter copies and binaries to reproduce those historical cells.

The maintained benchmark files live outside the Harness Git repository.
The [migration patch](benchmarks/cli-migration/benchmark-adapters.patch) and
[file hashes](benchmarks/cli-migration/adapter-files.json) preserve the eight
adapter/build/test changes with this implementation. The patch uses paths
relative to the Sandbox2 workspace. Apply it only to the matching pre-migration
files; the current workspace already contains these changes.

## Deterministic evidence

- Grammar checks cover bare invocation, explicit/piped prompts, missing and
  duplicate prompts, Unicode preservation, and reference/probe separation.
- Setup checks cover preview without writes, atomic replacement, persistence,
  invalid effort rejection, and preservation of omitted/model-default effort.
- Public-worker checks use mock HTTP gateways. Saved provider/model/effort
  reaches the wire; plain stdout is free of logs; management artifacts stay
  outside the project.
- Legacy/new CLI comparison uses the same contract and scripted tool response
  for oMLX and Ollama. It compares actual provider request JSON, including the
  request after a tool result. The new invocation also changes process cwd via
  `--dir` and carries a malformed personal config to test benchmark isolation.
- PTY checks cover TUI entry, Unicode input, small resize, idle exit, and raw
  terminal restoration without submitting a model task.
- Pier and Harbor tests cover quoted/multiline prompt delivery, canonical flags,
  provider selection, contract preservation, collection, and cancellation.

Native validation commands:

```sh
cargo fmt --check
cargo test --all-targets --quiet
cargo clippy --all-targets -- -D warnings
```

Results: 380 native tests passed, with two existing ignored tests. Formatting
and Clippy passed. Harbor adapter checks passed all 15 tests; Pier adapter and
cell checks passed all 14 tests. Both build scripts passed shell syntax checks,
the saved patch passed its reverse application check, and all eight recorded
file hashes matched the workspace.

Adapter validation uses the existing Pier and Harbor Python environments.
The two registered Harbor SIGINT boundary fixtures fail when their subprocesses
resolve to the host's Python 3.9.6. They pass with Python 3.13.12 on PATH.
The fixture commands and implementations were preserved; this is an independent
verification-environment correction, not evidence for a runtime policy change.

A cold Linux AMD64 release/export build under Docker emulation progressed
through dependencies but reached the finite 600-second subprocess limit.
It was cancelled without exporting a binary. Linux packaging remains
unverified; no registered benchmark binary or historical build was replaced.

No live local-model trial or efficacy cell is part of this migration.
Before new trials, freeze the new binary, adapters, effective controls, and
environment; preserve the existing memory, admission, and cleanup checks.
