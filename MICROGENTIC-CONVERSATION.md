# Microgentic ongoing terminal conversation

Date: 2026-10-13. Source baseline: `84b6fe4`.

This note records the continuity slice committed as `0a65b21`.
The [conversational entry correction](MICROGENTIC-CHAT.md) replaces its default
coding-task framing with natural conversation; retained history and streaming
continue through the same worker path.

## Behavior and boundary

The initial TUI started separate ordinary workers and waited for a complete JSON
summary. This slice retains conversation context between prompts and displays
provider output while the existing worker is running.

Each TUI starts a unique conversation outside the project. Its private worker
receives the session file and emits typed JSONL events. The public `run` grammar,
JSON result schema, benchmark adapters, and historical experiment artifacts are
unchanged. Ordinary `run` still supplies no conversation to the runtime, even
when a TUI has previously worked in the same project.

The conversation retains actual user prompts, completed native assistant
responses, native reasoning only when the existing retention setting requests
it, and complete tool request/result batches. A fresh system message and current
task packet follow the usual contract/context assembler. Prior task-scoped
system, probe, continuation, and repair instructions are not carried as policy.
Each new prompt has its own contract, validation state, budgets, and stash
baseline. Historical tool observations do not prove fresh validation.

Repair pruning protects all conversation user prompts through the current task.
Single runs continue to protect their original first task exactly as before.
Reasoning-checkpoint cleanup also protects real conversation prompts, including
ones quoting checkpoint markers. The context ledger labels previous user prompts
as retained conversation and labels the current packet as task authority.

## State and bounded history

The file `session-*/conversation.json` lives under
`$XDG_STATE_HOME/microgentic/runs` or `~/.local/state/microgentic/runs`. It is
written atomically with private temporary-file permissions after the worker
settles. Reuse with a different canonical project root, provider, model, effort,
or reasoning-retention setting is refused before contacting a provider.

History admission uses the existing four-bytes-per-token approximation. Half of
the input allowance after the requested output reservation is available for
retained history. Oldest complete user exchanges are omitted until history fits;
tool-call batches are never split. Omissions appear in the view and in
`session.context.retained`. Earlier traces remain preserved. The 64 MiB state
read limit prevents an oversized persisted record from being loaded.

This is continuity during an open TUI, rather than a general resume facility.
Opening another TUI creates fresh state. Abrupt process loss, ownership recovery,
and resuming task validation/approvals remain separate preparation work.

## Streaming and cancellation

The worker emits full content chunks from the native provider stream. It reports
thinking character counts, provider progress, completed tool requests, and tool
results from existing events. The view updates as events arrive, without parsing
stderr or polling truncated trace previews. Native traces and exact provider
snapshots remain authoritative evidence.

Enter submits when idle; Alt-Enter adds a newline. Drafting, paste, and scrolling
remain available during generation. Ctrl-C forwards cancellation to the owned
worker. The worker retains completed responses/tool batches and records why the
request stopped; partial streamed responses are marked incomplete and excluded
from replay. A follow-up receives the stopped-request notice and completed
history. The TUI waits for worker cleanup before restoring the terminal.

The terminal also displays a native final summary if it differs from streamed
text, so a runtime stop explanation is not hidden by an earlier assistant reply.
Stream presentation does not create a second tool loop, shorten model patience,
change sampling, invent completion status, or replace deterministic checks.

## Deterministic checks

`tests/tui_conversation.py`, invoked by the native integration suite, exercises
the actual PTY, CLI worker, and mock HTTP gateway for both oMLX and Ollama,
through the canonical and compatibility executables:

- A reply longer than trace previews appears before the server completes it.
- The next prompt receives the original user request, completed assistant reply,
  and native tool request/result pair, including Unicode file content.
- Ctrl-C settles a paused stream; partial assistant text is absent from the
  session and from the follow-up provider request.
- Different project/model settings refuse session reuse before provider access.
- Plain `run` and a second TUI receive fresh two-message provider packets.
- Tool exposure and completion controls match between TUI and plain runs.
- Exit restores the original terminal mode after the conversation.

Unit checks cover complete-batch checkpointing, omission of old task policy,
whole-exchange history trimming, and repair/checkpoint pruning protection.
The existing worker, context, validation, recovery, and transport tests remain
in the full suite. These are delivery/conformance checks, not model efficacy
measurements; no live local-model trial is part of this slice.

The baseline binary was copied before rebuilding this slice. The reusable
`tests/benchmark_continuity.py` checker compares it against the new executable
using an authoritative contract, a scripted inspection tool, and a terminal
reply. Both gateways produce equal HTTP request JSON, exact assembled provider
snapshots, and adjacent context ledgers before and after the tool result. Public
JSON summary fields are unchanged. Binary hashes and the four packet comparisons
are recorded in [isolation evidence](benchmarks/cli-migration/conversation-isolation-check.json).

Validation: `cargo test --all-targets --quiet`, `cargo fmt --check`, and
`cargo clippy --all-targets -- -D warnings` passed: 385 tests passed, with two
existing ignored tests. Linux packaging remains unverified
as recorded in the initial migration; no registered benchmark build was replaced.
