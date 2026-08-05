# Experimental Small Model Harness

An experimental adaptive agent runtime for studying how smaller local language
models inspect, edit, validate, and repair scoped artifacts.

The harness is built in Rust on top of Mojentic. It emphasizes deterministic
validation, strict workspace boundaries, traceable runtime decisions, and
evidence-backed experimentation. Space Invaders in Bevy is the recurring coding
benchmark, while additional artifact profiles are used to test whether the
runtime generalizes beyond one task or domain.

## Status

This is a research project under active development, not a stable library or
production service. Runtime policies, contracts, trace schemas, and command-line
interfaces may change as experiments reveal better boundaries.

## Capabilities

- Typed run contracts for scope, guidance, probes, budgets, and terminal rules
- Scoped filesystem and tool execution
- Mutation-aware validation freshness
- Structured JSONL traces and deterministic trace analysis
- Bounded repair and retained-artifact retry policies
- Throughput-aware patience for slow local models
- Domain profiles that keep task-specific policy out of the runtime core

## Requirements

- Rust with Cargo and support for the 2024 edition
- A local Ollama-compatible model runner
- Network access for Cargo to fetch the revision-pinned Mojentic Rust dependency

## Build and Test

```sh
cargo build
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

## Command-Line Interface

List the available commands:

```sh
cargo run -- --help
```

The main commands are:

- `run` — execute one natural agent attempt
- `run-sequential` — execute bounded natural attempts against one retained
  artifact
- `analyze-trace` — summarize one or more JSONL traces
- `resolve-contract` — validate and resolve a run contract without invoking a
  model
- `summarize-matrix` — reproduce the preserved benchmark baseline

Use `cargo run -- <command> --help` for command-specific options.

## Design and Evidence

The evolving architecture and experimental decisions are documented in
[`GENERALIZATION_PLAN.md`](GENERALIZATION_PLAN.md). The repository preserves
deterministic fixtures and a canonical matrix baseline so architectural changes
can be checked against earlier behavior.

Experiments are intentionally run from separate, scoped workspaces. Generated
agents should see their work tree and task-relevant guidance, not experiment
management files or unrelated repository context.

## License

Licensed under the MIT License. See [`LICENSE`](LICENSE).
