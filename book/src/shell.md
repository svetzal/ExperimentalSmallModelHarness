# Shell use with `run`

`microgentic run` sends one prompt and writes the reply to stdout. It never
opens the conversation view, so it works in pipes, scripts, and redirections.

## Give the prompt

Give the prompt with `--prompt`, or on stdin:

```sh
microgentic run --prompt 'Explain this project'
echo 'Explain this project' | microgentic run
microgentic run < task.txt
```

When you give `--prompt` and also pipe data to stdin, the prompt is the
instruction and stdin is reference material, for example a diff or a build
log:

```sh
git diff --no-ext-diff | microgentic run --prompt 'Review this diff' > review.md
microgentic run --prompt 'Explain this build failure' < build.log
```

Commands in reference material are text for the model to read. They do not
become validation probes.

These inputs fail before any provider call:

- an empty prompt, or a prompt that has only whitespace
- more than one `--prompt`
- a prompt given as a positional argument
- stdin that is not valid UTF-8, or that is larger than 1 MiB
- no `--prompt` when stdin is a terminal

## Get the result

The reply goes to stdout. Progress and diagnostics go to stderr. Redirect them
separately:

```sh
microgentic run < task.txt > result.txt 2> diagnostics.log
microgentic run --prompt 'Summarize the public API' >> summaries.txt
```

`--json` writes the complete run summary as JSON instead of the reply text.

## Choose the project

`--dir` sets the project root for file and shell tools. The default is the
current directory.

```sh
microgentic run --prompt 'Fix the parser' --dir ./project
```

Run inputs and traces are kept outside the project, in the run-state directory.
Each run has its own directory. See [Files and locations](locations.md).

## Settings for one run

`run` uses your saved setup defaults. These flags override them for one run:

| Flag | Effect |
| --- | --- |
| `--provider omlx\|ollama` | Use this provider |
| `--model <id>` | Use this model |
| `--reasoning-effort <level>` | Use this reasoning effort |
| `--no-project-guidance` | Do not read `AGENTS.md` or `CLAUDE.md` |
| `--config <path>` | Read settings from a different file |

If you change the provider and do not give `--model`, the run uses that
provider's default model.

## Limits

- `run` can edit files and run commands in the project. Preview and approval
  are not available yet.
- The exit status does not report task success yet. A run that finishes with an
  incomplete task can still exit with 0. Read the reply or the JSON summary.
- Streamed JSONL output is not available yet.
