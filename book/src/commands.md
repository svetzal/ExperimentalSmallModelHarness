# Commands

Every command has `--help`, with examples.

## Everyday commands

| Command | What it does |
| --- | --- |
| `microgentic` | Open the conversation view in the current directory |
| `microgentic setup` | Show the provider, model, and effort settings it would save |
| `microgentic setup --apply` | Save those settings |
| `microgentic run` | Send one prompt from stdin or `--prompt`, and write the reply to stdout. Can edit the project |

All commands accept `--config <path>` to read settings from a different file.

## Traces

| Command | What it does |
| --- | --- |
| `microgentic analyze-trace <paths>` | Summarize context assembly and tool payload size from JSONL traces |
| `microgentic render-transcript <paths> --output <file>` | Render traces as one self-contained HTML transcript |

See [Traces and transcripts](traces.md).

## Research commands

These commands support registered experiments. Everyday use does not need them.

| Command | What it does |
| --- | --- |
| `microgentic benchmark run` | Run an isolated task contract with historical defaults and JSON output |
| `microgentic benchmark run-sequential` | Run bounded attempts against one retained artifact |
| `microgentic run-sequential` | The earlier advanced form of sequential runs |
| `microgentic resolve-contract` | Resolve and print a run contract without a provider |
| `microgentic summarize-matrix` | Reproduce the preserved benchmark baseline counts |
| `microgentic plan-task` | Propose a measurement-only acceptance checklist |
| `microgentic plan-interactions` | Propose measurement-only scenarios over an acceptance plan |
| `microgentic author-interaction-evidence` | Author evidence for one interaction candidate |

`run --help` also lists advanced research flags, such as model interaction
budgets, reasoning interruption, and request recovery. They are for registered
experiments.

See [Benchmark mode](benchmark.md).

## Exit status

A usage error exits with 2. Other errors exit with 1. A `run` that finishes
exits with 0, even when the task is incomplete. Typed outcomes and exit codes
for task results are planned.
