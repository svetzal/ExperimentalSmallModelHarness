# Conversational TUI entry

Date: 2026-10-13. Source baseline: `0a65b21`.

## Correction

The continuity slice retained native messages but still wrapped every TUI prompt
in the coding profile's benchmark instructions. That required project inspection,
Rust scaffolding, validation, and DONE/FAIL even for a greeting. A natural reply
could also enter the task continuation loop or be saved as an incomplete request.

The TUI now supplies conversational system guidance and preserves the current
message literally. Opening the view makes no provider request. Greetings,
questions, explanations, and discussion can receive natural replies. Work comes
from the conversation, including clear follow-up authorization. The guidance
asks for clarification when intent is unclear and treats quoted examples as data.
It does not classify task intent through keywords or select an engineering task
at startup. Tools remain available for requests and answers that need them.

Each private worker uses a scoped internal contract with the message as guidance
and no automatically inferred probes. Shell fences in discussion are not parsed
into executor-owned checks. Public task `run` and benchmark contract resolution
retain their existing behavior.

## Reply completion and task authority

A visible conversational response completes the exchange after applicable effect,
repair, and declared-probe checks. It does not require a DONE/FAIL token or create
a synthetic task-completion token. Empty/thinking-only recovery asks for a reply
to the message without assuming missing source files or a project to build.
The existing retry counts, interaction/token budgets, patience, and cancellation
policy remain in use. Natural reply completion is traced as
`session.reply.completed` and does not produce a stopped-request warning on the
next prompt.

Task runs continue to use their original system/run guidance, terminal tokens,
and declared-probe enforcement. Conversational instruction changes do not change
the shared runtime's authority rules. An empty probe list disables enforced
acceptance validation; arbitrary shell checks remain observational even when
they pass. Do not enable a mandatory probe gate without registered executors:
an observational check cannot clear that gate. The TUI asks the model to check
source changes and report observed results, while enforced user-facing work
contracts remain separate preparation work.

For a supplied authoritative contract, the existing mutation freshness, repair,
probe, and acceptance-ledger gates still run before conversational completion.
Validation terminalization can request a natural result summary in conversation
mode; the task path keeps its original exact DONE/FAIL instruction.

## Deterministic evidence

The native integration suite runs `tests/tui_dialogue.py` (now the
`tui_chats_naturally_*` tests in `tests/tui.rs`) through the real PTY
and mock HTTP gateways for oMLX and Ollama. It checks:

- Idle launch contacts no provider and creates no engineering task.
- A greeting is delivered literally and one natural answer finishes the exchange.
- Empty-reply recovery asks for conversation, without invented engineering work.
- A quoted shell example remains literal text with no acceptance probes, shell
  effects, scaffolding, or validation invocation.
- A later explicit work request can write a source file and run an actual offline
  compilation check, then report its observed result naturally.
- The shell check remains observational, and a following thanks/greeting does not
  restart engineering work or inherit an incomplete-task warning.
- Conversation exit restores the original terminal mode.

The existing history, full-content streaming, cancellation, session isolation,
scope, validation, context, and recovery tests remain in the full suite.
The pre-change executable was copied before rebuilding. The reusable
`tests/benchmark_continuity.py` checker (now `tests/benchmark_continuity.rs`) compares benchmark HTTP request JSON,
exact provider snapshots, and context ledgers before and after a scripted tool
result for both gateways; public summary fields also remain equal. See
[comparison evidence](benchmarks/cli-migration/dialogue-isolation-check.json).

Validation passed: 386 native tests, with two existing ignored tests;
`cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` also passed.

No live local-model trial or model-capability measurement is part of this change.

## Evidence footer visibility

Follow-up baseline: `4a5e896`, on 2026-10-13.

The TUI previously appended an `Evidence:` trace path to every completed reply.
This came from presentation code, so ordinary greetings also displayed it.
The footer now appears when the current exchange requests project tools.
Read-only inspection, edits, and checks all qualify. The worker sends a structured
tool-start event; the TUI tracks activity separately for each exchange.
It does not classify prompt wording or keep a persistent engineering mode.

Every exchange retains its trace. Hiding a footer does not change provider
instructions, tool authority, validation, completion, or benchmark output.
The trace path is an activity record, not a claim that validation passed.

The PTY fixtures for both providers check greetings, empty-reply recovery,
quoted engineering discussion, source edits with checks, read-only inspection,
and a conversational follow-up after work. They check footer visibility and
continued trace retention. All eight CLI integration tests pass, including
the comparison of benchmark packets and context ledgers with the task path.
