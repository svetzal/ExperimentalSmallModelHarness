# Project guidance

The conversation and `run` read project guidance from `AGENTS.md` files and
give it to the model. Use guidance to tell the model about your project's
conventions, commands, and boundaries.

## Where Microgentic looks

1. **Your personal file:** `$XDG_CONFIG_HOME/microgentic/AGENTS.md`, or
   `~/.config/microgentic/AGENTS.md`. It applies to every project.
2. **From the project directory up to the Git root:** one file in each
   directory. A directory that has no `AGENTS.md` can use `CLAUDE.md`.
   Discovery does not go above the repository root or `$HOME`. Outside a Git
   repository, only the project directory is used.
3. **Below the project directory:** nested `AGENTS.md` files. Microgentic skips
   paths that `.gitignore` ignores, and symbolic links that point outside the
   repository.

## How the model gets it

The files go into the system context, the most general first: your personal
file, then the repository root, then deeper directories. When files disagree,
the deeper file wins, and the model is told so.

Nested files are listed by path at the start. Each nested file is sent the
first time a tool call in an exchange works with a file in its directory.

When you send a new message, Microgentic reads the files again. If a file
changed, appeared, or disappeared, the model is told.

`@path` imports in guidance files are not expanded.

## Size limit

All guidance together is limited to 8,000 estimated tokens, or to one eighth of
a smaller known context window, and never less than 500. Each file is limited
to half of that. The advanced `guidance_fraction` setting changes the share of
the window; see [Providers and models](providers.md#advanced-context-settings). When the
limit is reached, the most general files are cut first, and a note names the
file and how much was left out.

## Guidance is information, not permission

Microgentic sets the tools, permissions, validation, and stop rules before it
reads guidance. Guidance text cannot change them.

## See what loaded

- In the conversation, `/guidance` lists the files with their size and status.
- The status bar shows `📜` and the number of files in context.
- Each trace has a `context.guidance.discovered` event. It records every
  candidate file's path, SHA-256, size, and whether it was included, cut, left
  out, or held as nested guidance.

## Turn it off

```sh
microgentic run --no-project-guidance --prompt '...'
```

Benchmark runs, runs with `--experiment`, `--goal`, or `--contract`, and the
`adaptive-agent-harness` compatibility executable never read guidance.
