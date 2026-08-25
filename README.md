# Experimental Small Model Harness

An experimental adaptive agent runtime for studying how smaller local language
models inspect, edit, validate, and repair scoped artifacts.

The harness is built in Rust on top of
[Mojentic](https://github.com/svetzal/mojentic-ru). It emphasizes deterministic
validation, strict workspace boundaries, traceable runtime decisions, and
evidence-backed experimentation. Space Invaders in Bevy is the recurring coding
benchmark, while additional artifact profiles are used to test whether the
runtime generalizes beyond one task or domain.

## Status

This is a research project under active development, not a stable library or
production service. Runtime policies, contracts, trace schemas, and command-line
interfaces may change as experiments reveal better boundaries.

## Capabilities

- Typed run contracts for scope, guidance, probes, budgets, and terminal rules
- Scoped filesystem and tool execution
- Mutation-aware validation freshness
- Structured JSONL traces with exact provider-bound requests and deterministic
  trace analysis
- Native self-contained HTML transcript generation
- Bounded repair and retained-artifact retry policies
- Throughput-aware patience for slow local models
- Domain profiles that keep task-specific policy out of the runtime core

## Requirements

- Rust with Cargo and support for the 2024 edition
- A local Ollama-compatible model runner
- Network access for Cargo to fetch the revision-pinned Mojentic Rust dependency

## Build and Test

```sh
cargo build
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

## Command-Line Interface

List the available commands:

```sh
cargo run -- --help
```

The main commands are:

- `run` — execute one natural agent attempt
- `run-sequential` — execute bounded natural attempts against one retained
  artifact
- `analyze-trace` — summarize one or more JSONL traces
- `render-transcript` — render traces as a self-contained causal HTML report
- `resolve-contract` — validate and resolve a run contract without invoking a
  model
- `summarize-matrix` — reproduce the preserved benchmark baseline

Use `cargo run -- <command> --help` for command-specific options.

### Native initial-context assembly

Every run now constructs one authoritative initial-context packet before the
worker starts. By default that packet contains the resolved task and profile
guidance. `run` and `run-sequential` can also accept an adapter-owned guidance
catalog:

```sh
cargo run -- run \
  --experiment ../Experiments/GenerationN \
  --initial-context-catalog context-catalog.json \
  --semantic-advisor-model qwen3.6:35b-a3b-coding-nvfp4
```

Catalog records have one harness-enforced disposition:

- `required` records always enter the worker packet and never enter the
  advisory call.
- `selectable` records are the only records visible to an isolated structured
  semantic advisory.
- `excluded` records must omit content and enter neither model packet.

The advisory has no tools or mutation authority. It proposes optional record
IDs; the initial-context assembler validates schema, disposition, IDs,
uniqueness, confidence, selection count, and the combined required-plus-selected
guidance budget. Invalid proposals fail closed. The trace records exact advisory
inputs and outputs plus the assembler's authoritative components and decision.
`max_advisory_chars` bounds the complete advisory request, including the task,
instructions, and selectable records; oversized calls are rejected before the
provider is invoked.

```json
{
  "schema_version": "initial_context_catalog.v2",
  "max_selected": 2,
  "max_total_guidance_chars": 6000,
  "max_advisory_chars": 20000,
  "min_confidence": 0.75,
  "records": [
    {
      "id": "release-safety",
      "disposition": "required",
      "description": "Release claims require observed evidence",
      "content": "Never claim validation that was not observed.",
      "source": "release-safety.md"
    },
    {
      "id": "release-format",
      "disposition": "selectable",
      "description": "Release-note layout used only for release tasks",
      "content": "Use a title followed by exactly two bullet points.",
      "source": "release-format.md"
    },
    {
      "id": "private-roadmap",
      "disposition": "excluded",
      "description": "Material prohibited from worker context",
      "source": "private-roadmap.md"
    }
  ]
}
```

Semantic advisories are a reusable proposal-only harness effect with explicit
kinds for initial-context selection, situation analysis, and failure
classification. Initial-context selection is the first active consumer. When
the catalog is omitted—or contains no selectable records—no advisory call is
made. The semantic-selection policy remains experimental even though native
context assembly is now foundational.

### Declared probe delivery

Adapter-owned probes in an explicit run contract are part of the initial worker
packet and terminal authority. Both command probes and artifact assertions are
rendered by stable ID. Their executor-owned command bytes or expected content
stay outside model-facing provider requests. The worker invokes either kind
through `execute_probe`; the returned evidence identifies the probe and command
digest without exposing its registered implementation. The worker is told not
to substitute a synthetic approximation. After a relevant mutation, every
declared probe must pass in the current mutation generation before `DONE` is
accepted.

When a resolved contract declares probes, the harness adds `execute_probe` to
the active model-facing tool set even when the selected domain profile does not
include it by default. This keeps probe reachability tied to the contract while
leaving probe-free runs and their tool surfaces unchanged.

Declared command identity remains stable throughout repair handling. Runtime
repair state and every model-facing repair or escalation prompt use
`probe:<id>` and `declared_command_probe`; the registered command bytes remain
executor-owned even after repeated failures.

Command probes use the same scoped shell executor, finite timeout, process-group
cleanup, mutation sensing, repair policy, and lifecycle-phase tracing as other
shell effects. Their declared stable ID, rather than inferred command text,
grants validation authority. Model-authored `shell_command` calls are opaque
observations: neither a familiar command name nor a zero exit status can clear
pending evidence or trigger terminal readiness. A probe-free contract therefore
does not invent a self-validation gate; independent evaluation remains separate.

The optional `--repair-handoff-policy constrained` mode gives an authoritative
failed declared probe invoked through `execute_probe` immediate control of the
next provider request. It ends the in-flight tool turn, removes superseded
agent-loop user guidance, places the validation-repair action contract after
the failure evidence, and limits the next tool surface to `write_file`,
`edit_file`, `shell_command`, and `execute_probe`. Model-authored
`shell_command` failures do not activate this authority. The default
`text-only` mode preserves the legacy continuation behavior so the two policies
can be compared on one pinned binary.

The `constrained-action-only` variant keeps that first bounded repair request.
If the request reaches its repair thinking cap without an action, the following
request uses Ollama's native `think=false` control, retains the authoritative
failure packet and restricted repair tools, and explicitly states that the
interrupted hidden reasoning was not retained. The native action-only request is
reserved for this post-validation repair boundary. Before source mutation,
ordinary reasoning and tool use remain interleaved; hidden-only no-action turns
receive bounded continuation guidance rather than a forced native mode switch.
The action-only provider snapshot records `reasoning_effort: disabled`,
`thinking_disabled: true`, and the effective cap source `provider_disabled`.
The ordinary `constrained` variant still retries with thinking enabled.

Every `llm.provider_request.assembled` snapshot records the harness-side
thinking limits that govern that request. The `harness_limits` object includes
the ordinary and repair caps, whether validation repair is active, and the
effective cap and source. A failed declared probe activates repair limits
immediately for any following request in the same tool turn as well as for a
constrained handoff.

Probe delivery is trace-visible as `agent.contract.probes.delivered`, including
the delivered IDs, probe kinds, and resulting worker-message size. Empty probe
lists leave the worker message unchanged and emit no delivery event. Probe
expectations remain adapter-owned: model-authored semantic advisories may
propose candidate evidence, but they do not gain terminal authority without
deterministic contract policy.

### Session transcripts

Generate an interactive, single-file transcript from any trace files or
directories:

```sh
cargo run -- render-transcript traces/ \
  --output transcript.html \
  --title "Agent Session Transcript"
```

The report presents exact provider input, streamed reasoning and assistant
text, tool effects, harness decisions, and context measurements as one causal
timeline. Final response tool-call batches include each normalized call's
response index, provider call ID, tool name, bounded canonical arguments, and
full-argument hash, including calls policy stopped before execution. It
recursively discovers harness JSONL traces while ignoring other JSONL files
that do not contain a `run.started` event.

Independent verification is optional and harness-neutral. Supply a
`transcript_evidence.v1` JSON file with `--evidence`; entries match sessions by
canonical trace path or transcript label. Replicate directories shaped like
`rNN-<arm>` supply stable labels for arbitrary arm names. The complete schema
and fidelity rules are documented in [`TRACE_SCHEMA.md`](TRACE_SCHEMA.md).

## Design and Evidence

The evolving architecture and experimental decisions are documented in
[`GENERALIZATION_PLAN.md`](GENERALIZATION_PLAN.md). The repository preserves
deterministic fixtures and a canonical matrix baseline so architectural changes
can be checked against earlier behavior.

The worker trace format and the required context-first performance-review
procedure are documented in [`TRACE_SCHEMA.md`](TRACE_SCHEMA.md). Every worker
provider call has a compact context ledger plus an exact ordered request
snapshot. `analyze-trace` reports snapshot count and whether coverage is
complete, allowing legacy or truncated traces to be identified before drawing
context-sensitive conclusions. Harness hard stops take precedence over earlier
incidental validation success in both analyzer outcomes and transcript status
cards.

Response-side tool-call coverage is measured separately from request coverage.
`analyze-trace` reports batch count, aggregate and normalized call counts,
largest batch, bounded-argument truncations, and whether per-call response
evidence is complete. Stream progress counts remain progress evidence only;
they never create executable calls in the Harness.

Repeated reads of an unchanged file are rendered as one event-sourced canonical
projection in provider context. Version-two projections explicitly report
`content_status` as `complete` or `partial`; partial projections name exact
`missing_ranges`. The content bound scales with the actual provider window and
is capped, while raw read events remain unchanged in the trace. Read-only shell
effects do not split a file epoch, but confirmed workspace mutations do.

Experiments are intentionally run from separate, scoped workspaces. Generated
agents should see their work tree and task-relevant guidance, not experiment
management files or unrelated repository context.

## License

Licensed under the MIT License. See [`LICENSE`](LICENSE).
