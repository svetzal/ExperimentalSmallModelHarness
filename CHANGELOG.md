# Changelog

<!-- markdownlint-configure-file {"MD024": {"siblings_only": true}} -->

All notable changes to Microgentic are recorded in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
The release workflow publishes each version's section as its GitHub release notes.

## [Unreleased]

### Added

- A context panel in the conversation view. Ctrl-O or `/context` opens it to
  the right of the conversation. It lists every message and tool descriptor in
  the latest model request, in order, with its kind, an estimated token count,
  and its first lines. It says whether the model's earlier thinking was sent
  (by default it is not) and how many older exchanges were left out. It marks
  summarized and consolidated tool output as not verbatim, and marks each
  message `new` or `kept` against the previous request. It updates with every
  request, including tool loops. Alt-Page Up/Down scroll it. Provider requests
  are unchanged.

## [0.3.0] - 2026-10-15

The agent gets a calculator and reads project guidance, and the conversation
view is easier to read. Benchmark mode is unchanged: it offers no calculator and
reads no project guidance, and its provider requests are identical to 0.2.0.

### Added

- Project guidance. Microgentic reads `AGENTS.md` files from the project
  directory up to the Git root, with `CLAUDE.md` as a fallback where a directory
  has no `AGENTS.md`. A personal file at `~/.config/microgentic/AGENTS.md`
  applies everywhere. Deeper files win when guidance conflicts. A nested file
  below the project directory is delivered when the agent first works in its
  directory. Guidance cannot grant permissions or change the available tools.
  `microgentic run --no-project-guidance` turns discovery off.
- `/guidance` shows what guidance loaded, and the status bar shows a 📜 count.
- A `calculate` tool evaluates arithmetic exactly, so the model does not have
  to work sums out in its head. It accepts batches, and a line can name a value
  for later lines. The conversation view shows each result on the tool line.

### Changed

- Text wraps at words and no row starts with a stray space. Indentation and
  list items keep their shape. Tokens too long for a row are split at a natural
  break or shortened in the middle with `…`. The Evidence path is never
  shortened.
- Reply text that follows thinking or tool lines has a blank row above it.
- The launch animation plays in about 1.3 seconds.

## [0.2.0] - 2026-10-15

The conversation view is easier to read and to steer while a small model works.
Benchmark mode, `microgentic run`, and trace records are unchanged.

### Added

- Colour that follows the terminal theme. Set `NO_COLOR` to turn it off; labels
  and glyphs keep their meaning without it.
- Local slash commands that never reach the model. `/model` and `/effort` open
  arrow-key pickers or take a value directly, `/clear` starts a new
  conversation, and `/help` lists the commands. The footer suggests matching
  commands as you type, and Tab completes them. Changes from `/model` and
  `/effort` last for the session; `microgentic setup` changes the defaults.
- One line per tool call, with a short summary of its parameters such as the
  file and line range it read. A spinner marks a running call, then a check or
  a cross shows its result.
- Streamed thinking in dim italics. When the model finishes, thinking that led
  to tool calls shrinks to its last line above those calls, and thinking before
  a reply is removed.
- A status bar with the model, reasoning effort, project folder, Git branch and
  changes, and how full the context window is. The context figure is the
  runtime's own estimate; without a known window size it shows a token count.
- A short launch animation. Any key skips it, and the key still reaches the
  input. Set `MICROGENTIC_NO_ANIMATION=1` to turn it off.

### Changed

- Changing the model or effort after the first message starts a new
  conversation, because a conversation keeps the model and effort it began with.

### Fixed

- Whitespace-only output from the model no longer opens blank rows.

## [0.1.0] - 2026-10-13

The first public package. Microgentic is an adaptive coding agent optimized for
small models that need help staying on task, and this release brings the
experimental harness into a small terminal interface for everyday use.

### Added

- An ongoing, streamed conversation with `microgentic`.
- `microgentic setup` configures oMLX or Ollama, a default model, and reasoning
  effort.
- Prompts through stdin or `--prompt`, with results and diagnostics redirected
  separately.
- Project inspection, editing, and checks through the existing scoped runtime.
- `microgentic benchmark run` keeps benchmark tasks isolated.
- The native trace analyzer and transcript renderer for preserved traces.
- Packages for Apple Silicon, Intel Mac, and Linux x86-64, with checksums,
  binary manifests, and embedded source revisions. `mg` remains an optional
  shell alias, and the `adaptive-agent-harness` compatibility executable remains
  available.

### Known limits

- Work permissions and approval previews, durable session resume, additional
  gateways, and richer tool inspection are not built yet.
- The runtime can edit files and run commands when you request work.
- Provider credentials and server URLs stay in their existing environment
  variables.
- A successful process exit still needs inspection of the native terminal
  result when classifying benchmark outcomes.

[Unreleased]: https://github.com/svetzal/microgentic/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/svetzal/microgentic/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/svetzal/microgentic/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/svetzal/microgentic/releases/tag/v0.1.0
