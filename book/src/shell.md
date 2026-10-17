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

Commands in the prompt and in reference material are text for the model to
read. They do not become validation probes.

These inputs fail before any provider call:

- an empty prompt, or a prompt that has only whitespace
- more than one `--prompt`
- a prompt given as a positional argument
- stdin that is not valid UTF-8, or that is larger than 1 MiB
- no `--prompt` when stdin is a terminal

## How a run ends

A run is one exchange, like one message in the conversation view. The prompt
reaches the model as you wrote it. A greeting gets a greeting, and a question
gets an answer.

The model can use its tools, and the run continues while it does. When you ask
for a change, it can read files, edit them, and run a check before it replies.
The first reply that has text and no tool calls ends the run. Before it ends,
the runtime still asks the model to check unvalidated source changes and to fix
a failed check.

The model does not have to end with DONE or FAIL, and the run does not ask it
to continue after it replies.

A termination signal also ends the run: SIGINT from Ctrl-C, SIGTERM from
`kill`, or SIGHUP when the terminal closes. The model request is cancelled, a
project command that is still running is killed, and the run exits with an
error. If the work has not stopped two seconds later, or a second signal
arrives, the process exits at once with the signal's conventional status.

`run --experiment`, `--goal` or `--contract` starts a task run instead. Task
runs keep the task instructions, the DONE/FAIL ending, and declared probes.
See [Benchmark mode](benchmark.md).

## Get the result

The reply goes to stdout, without blank lines before or after it. The streamed
reply, progress, and diagnostics go to stderr. Redirect them separately:

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
