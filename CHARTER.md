# Charter for the adaptive agent harness

## Purpose

This Rust application studies how an adaptive harness can make smaller local
models useful. It changes the size, context, validation, and feedback of work
packets according to measured capability.

The harness owns continuity. The model reasons about a scoped packet.
Deterministic checks establish whether work satisfies its contract.
A model claim alone cannot establish success.

The public product is Microgentic: an adaptive coding agent optimized for small
models that need extra help staying on task. Its CLI and terminal conversation
should feel small, simple, and fast, with Pi as an interaction reference and
selectable Mojentic gateways. Build session and presentation around the general
runtime, preserving validation, evidence, and local-model patience.

Design the control surface for end users. Adapt benchmark launchers to its public
non-interactive interface and documented advanced configuration. Verify equivalent
resolved inputs and runtime behavior before new trials; preserve frozen evidence
through pinned binaries, adapter revisions, and configuration. The preparation
backlog is in `PUBLIC_READINESS.md`; proposed features are not implemented behavior.

The initial Microgentic CLI keeps task preparation and presentation outside the
runtime policy. Benchmark adapters supply explicit providers and authoritative
contracts instead of inheriting personal setup defaults. Prove migration fidelity
with provider requests after tool feedback, not only matching option names.
See `MICROGENTIC-CLI-MIGRATION.md` for the implementation boundary and checks.

Conversation continuity retains native user, assistant, and tool history, while
each request resolves fresh task authority and validation state. Never replay old
system/probe/repair instructions as new task policy or incomplete streams as
completed responses. Repair pruning must protect the current user task as well as
earlier conversation prompts. Stream presentation observes the existing runtime;
benchmark entry points never load ambient conversation history. The deterministic
boundary checks are in `MICROGENTIC-CONVERSATION.md`.

## Responsibilities

The harness maintains goals, acceptance criteria, budgets, and stopping rules.
It assembles bounded context and preserves conversation history.
It runs scoped tools, validates their effects, classifies failures, and records
traces that explain each decision.

Mojentic supplies provider completion, messages, tool abstractions, and tracing
primitives. Provider-level recovery belongs in Mojentic. The harness owns resource
admission, experiment policy, session recovery, and trace persistence.

The harness is general. Space Invaders and Igel are benchmark tasks, not its domain.
Do not couple runtime policy to one task, language, model, or repository layout.
Do not hide validation inside an unconstrained model phase.

## Evidence and scope

Use explicit contracts and deterministic probes. Preserve exact provider-bound
context when explaining a model failure. Distinguish model behavior from missing
context, tool defects, and environment failures.

A surprising trace needs a hypothesis and a controlled measurement before a
policy change. Local generation has no fixed wall-clock cutoff. The harness uses
measured progress and throughput-aware patience.

Keep historical experiments unchanged. Never restart a stopped benchmark to
validate a library change. Use a fresh registered experiment instead.

The detailed research charter and operating evidence remain in
`~/Work/Sandbox2/CHARTER.md` and `~/Work/Sandbox2/EXPERIMENTAL_METHOD.md`.
The repository README describes the current runtime and commands.
`RECOVERY-INTEGRATION-2026-10.md` defines the authorized recovery integration.

## Request recovery evidence

Wire attempts belong to one immutable completion and are separate from semantic
interactions and tool depth. Admission requires proof for the exact previous
attempt and encoded request. A syntactically valid provider identifier can still
echo a credential; filter known active credentials before persistence or admission.
Credential fragments across transport or semantic boundaries require buffering;
a redacted capture must never be presented as exact wire evidence.

## Workspace preservation

Capture Git stash object IDs before agent tools run, then compare inventories
before accepting DONE. Existing stashes are baseline evidence, not automatically
agent-owned work. Never automatically restore or discard stashes. Record shared
repository attribution as uncertain. A restored implementation must still pass
its independent verifier; recovery of missing edits alone does not prove quality.
See `STASH-COMPLETION-2026-10.md` for the deterministic checks and diagnostic.
