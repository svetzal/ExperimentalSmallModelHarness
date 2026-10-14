# Benchmark task mode

Date: 2026-10-13. Implementation baseline: `d44b418`.
Pre-UX comparison source: `bfe3529`.

## Command boundary

`microgentic benchmark run` uses the shared task runtime with historical defaults.
`microgentic benchmark run-sequential` uses the existing coordinator for bounded
retries.
The TUI keeps its conversational entry behavior.

```sh
microgentic benchmark run --experiment ./cell --dir ./cell/workspace
microgentic benchmark run --experiment ./cell --contract contract.json \
  --dir ./cell/workspace --provider ollama --model qwen3.8:27b-mxfp8 \
  > result.json 2> diagnostics.log
microgentic benchmark run-sequential --experiment ./cell \
  --dir ./cell/workspace --artifact answer.txt --expected-artifact expected.txt
```

Benchmark mode requires `--experiment`. Task input comes from `task.md` by default,
`--goal`, or `--contract`. An explicit contract takes precedence when you supply
both files, as it did before the UX migration.
Benchmark mode rejects `--prompt`.

The mode ignores stdin, personal setup, `--config`, and TUI history. It creates
no personal run or session state. It always emits the complete JSON summary to
stdout. Diagnostics and observed runtime progress go to stderr.

Benchmark mode accepts `--json` for compatibility, but the flag is unnecessary.

`--dir` selects the tool root and defaults to the launching directory. This matches
the original cwd behavior. Experiment and trace paths resolve before the cwd changes.
Goal and contract paths remain relative to the experiment directory.

Provider environment variables still supply endpoints and credentials. The default
provider remains oMLX, with its existing model and reasoning defaults. Benchmarks
should supply provider, model, and research controls explicitly.

Task prompting, declared probes, validation authority, DONE/FAIL detection,
context policy, repair rules, patience, budgets, and recovery remain on the shared
task path. Each task run starts fresh. Sequential attempts retain the declared
artifact and use the existing limits and independent exact comparison.

## Adapter migration

Maintained Pier and Harbor adapters now invoke `microgentic benchmark run`.
The uploaded explicit contract remains the authoritative task input. Prompt text
is no longer duplicated in process arguments. JSON output needs no extra flag.
Provider selection, runtime budgets, cancellation, and collection remain unchanged.

The adapters continue to select Ollama explicitly by default. The earlier CLI
migration introduced that transport choice.
The original runtime default remains oMLX.
Frozen generation scripts, registrations, binaries, traces, and scores remain unchanged.
The advanced `run` and `run-sequential` forms and compatibility executable remain
available.

These external adapter sources are outside the Harness Git repository. The
following files preserve their incremental patch and before/after hashes:

- `benchmarks/cli-migration/benchmark-mode-adapters.patch`
- `benchmarks/cli-migration/benchmark-mode-adapter-files.json`

The earlier migration patch and evidence remain separate historical records.

## Deterministic evidence

The comparison rebuilt `bfe3529` with its locked dependencies and preserved that
binary before rebuilding Microgentic. `tests/benchmark_continuity.py` compares the
original task commands with the new benchmark commands through mock HTTP gateways.
The record is `benchmarks/cli-migration/benchmark-mode-check.json`.

Checks cover oMLX and Ollama, explicit contracts, default `task.md` intake, and
two sequential attempts. They compare actual provider requests, exact assembled
snapshots, and adjacent context ledgers before and after tool feedback.

Sequential checks also compare terminal evidence and artifact snapshots and test
fresh context between attempts. Unrelated piped input and a malformed personal
configuration must have no effect. No session events or personal state may
appear.

The sequential fixture paces mock attempts because historical trace filenames have
one-second precision. Two faster attempts can collide. The preparation checklist
records this existing issue separately from mode preservation.

CLI tests check required experiment input, file precedence, defaults, help, and
prompt rejection before provider access. Existing TUI tests retain coverage of
natural conversation, streaming, native history, cancellation, and isolation.
Adapter tests cover contract bytes, controls, provider routing, and cancellation.

These checks establish equivalence for scripted paths, not live-model efficacy.
Container packaging and fresh registered benchmark cells remain preparation work.
The mode preserves the task behavior contract. Exact historical reproduction still
needs pinned binaries, dependencies, adapters, provider configuration, and inputs.
Future task-runtime changes need explicit evidence and compatibility review.
