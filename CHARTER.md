# Charter for the adaptive agent harness

## Purpose

This Rust application studies how an adaptive harness can make smaller local
models useful. It changes the size, context, validation, and feedback of work
packets according to measured capability.

The harness owns continuity. The model reasons about a scoped packet.
Deterministic checks establish whether work satisfies its contract.
A model claim alone cannot establish success.

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
