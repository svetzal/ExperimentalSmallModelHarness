# Microgentic

Microgentic is a coding agent for small language models that run on your own
computer. Small models lose track of a task more easily than large hosted
models. Microgentic gives them more help: it keeps the context small and
relevant, it checks work with deterministic probes, and it records what
happened so that you can examine it later.

You talk to Microgentic in a terminal conversation, or you send it one prompt
from the shell. It connects to a local [oMLX](https://github.com/jundot/omlx)
or [Ollama](https://ollama.com) server through the
[Mojentic](https://github.com/svetzal/mojentic-ru) gateways.

```sh
brew install svetzal/tap/microgentic
microgentic setup --apply
microgentic
```

## What it does

- Holds an ongoing, streamed conversation in your project, with context
  retained between messages.
- Reads, edits, and checks project files through tools that are scoped to the
  project directory.
- Reads `AGENTS.md` project guidance, and gives the model an exact
  `calculate` tool.
- Works in shell pipelines: `run` reads a prompt from stdin or `--prompt`, and
  writes the result to stdout.
- Waits patiently for slow local models, and measures their throughput instead
  of applying a fixed timeout.
- Writes a JSONL trace for each run, with the exact requests sent to the
  provider. A native renderer turns traces into an HTML transcript.

## Status

Microgentic is an early release under active development. Commands, run
contracts, and trace schemas can change between versions. The
[changelog](changelog.md) records each change.

These features are not built yet: approval previews before edits, durable
session resume, providers other than oMLX and Ollama, and exit codes that
report task success. Until then, the runtime can edit files and run commands
when you ask it to do work.

Microgentic also has a research side. A benchmark mode runs isolated task
contracts for registered experiments. The everyday commands and benchmark mode
use the same runtime, but benchmark mode ignores your personal settings.

## License

Microgentic is open source under the MIT License. The source is at
[github.com/svetzal/microgentic](https://github.com/svetzal/microgentic).
