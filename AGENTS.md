# Microgentic operating notes

## Product direction

Microgentic is an adaptive coding agent optimized for small models that need help
staying on task. Keep the everyday experience small, simple, and fast.
Treat `CHARTER.md` as the product charter and `PUBLIC_READINESS.md` as the backlog.

The primary development checkout is `~/Work/Projects/Personal/microgentic`.
`~/Work/Sandbox2/Harness` remains a separate clone for benchmark integration.
Both use `git@github.com:svetzal/microgentic.git`. Benchmark experiments and their
operating protocol remain under `~/Work/Sandbox2/`; do not move or rewrite them.

## Runtime and UX boundaries

- Bare `microgentic` starts an ongoing conversation with no assumed work.
- `run` reads a prompt from stdin or `--prompt` and participates in shell pipes.
- `benchmark run` and `benchmark run-sequential` retain isolated task behavior.
- Preserve benchmark contracts, validation authority, model controls, and stop
  rules when changing presentation. Compare exact provider requests when needed.
- Keep user preferences, stdin intake, and TUI history out of benchmark mode.
- Show evidence footers for project tools in the current exchange. Keep traces
  even when ordinary conversation does not display a footer.
- Preserve unrelated user changes and Git stashes.
- Retain the `adaptive_agent_harness` library name and compatibility executable
  until consumers complete an explicit migration. Do not install an `mg` command.

## Validation and commits

Commit after meaningful changes. Use the existing CLI UX conventions.
Run checks appropriate to the change. Before a release, run all required gates:

```sh
cargo fmt --all --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
python3 -m unittest discover -s benchmarks -p 'test_collection_validation.py'
cargo build --locked --release --bins
```

Provider fixtures use mock HTTP servers. Do not start local models for routine
checks. Preserve model patience and let the runtime own stop policy during live
runs. Local-model comparisons stay within Qwen unless the user requests otherwise.

## Local install and CI

Install locally with Cargo, not by waiting for a published release:

```sh
git worktree add --detach /tmp/mg-install <commit>
cargo install --locked --path /tmp/mg-install --target-dir target/install --force
git worktree remove /tmp/mg-install
```

Run every gate in `.hone-gates.json` on that revision immediately before you
install it; a local install is a deployment. Install from a committed revision,
not from a working tree that another agent may be editing. `~/.cargo/bin` comes before Homebrew on the
PATH here, so the Cargo build replaces the Homebrew one without an uninstall.

Every push and tag starts GitHub Actions. Let those runs finish, including for
intermediate versions; never cancel them. Do not wait on them either: push, then
continue local work and local installs. Check a run's result later, when its
outcome decides the next step (for example, before updating the Homebrew tap).

## Release process

See `RELEASING.md`. Public binaries need an embedded source revision, native
platform checks, checksums, and a smoke test outside the build checkout.
Publishing requires user authorization; a direct request to publish supplies it.
Never replace an existing release tag or asset silently. Update the Homebrew tap
only after the corresponding release assets pass validation.
