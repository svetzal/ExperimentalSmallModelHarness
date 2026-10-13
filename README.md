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

The [public CLI readiness checklist](PUBLIC_READINESS.md) records the current
interface review and preparation work for provider setup, a conversation TUI,
session recovery, packaging, and a product name.

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
- An oMLX or Ollama server with a Qwen model installed
- Network access for Cargo to fetch the Git-pinned Mojentic prototype

## Provider configuration

`run` and `run-sequential` accept `--provider omlx|ollama` (default: omlx).
The selected gateway handles the worker and its in-run semantic advisories.
Standalone planning commands still use oMLX. Configure connections with
environment variables:

| Variable | Meaning | Default |
| --- | --- | --- |
| `OLLAMA_HOST` | Ollama server URL | `http://localhost:11434` |
| `OMLX_HOST` | Server URL without `/v1` | `http://localhost:8000` |
| `OMLX_API_KEY` | Bearer token for authenticated servers | Unset |
| `OMLX_TIMEOUT` | Timeout for non-streaming calls, in milliseconds | `600000` |

The gateway adds `/v1` to the URL. Streaming requests have no gateway-wide
response timeout. The harness retains authority over stream patience and stops.

The default model is `Qwen3.8-27B-MLX-8bit` for oMLX or
`qwen3.8:27b-mxfp8` for Ollama. Use `--model` to select another
installed model ID from `/v1/models`. The local server advertised this default
model with a 262144-token context window during migration.

For `run` and `run-sequential`:

- `--max-tokens` limits generated tokens per request, including reasoning.
  The default is 16384. Zero is invalid. `--num-predict` remains a compatibility
  alias. The budget maps to oMLX `max_tokens` or Ollama `options.num_predict`.
- `--reasoning-effort disabled|enabled|low|medium|high|xhigh` selects worker
  reasoning behavior.
  Omit it to retain the model default. On oMLX, `disabled` sends
  `enable_thinking: false` and named levels use `reasoning_effort`. On Ollama,
  disabled/enabled send boolean `think` and named levels send string `think`.
  Their effect depends on the
  model's chat template. Advisory calls retain their model defaults.
- `--context-window-tokens` controls harness context accounting and compaction.
  Ollama also receives `options.num_ctx`. Configure the actual oMLX limit on
  the server; its gateway does not send Ollama-specific parameters.
- Thinking-only and repair interruptions are disabled by default. Select the
  model reasoning effort and measure its reasoning instead. Positive explicit
  cap values remain available for registered interruption experiments. The
  `constrained-action-only` repair retry can override worker effort to `disabled`.
- Shell tools use a non-login shell, preserving the inherited PATH and virtual
  environment. Task tools resolve the same executables as the worker environment.

Coding-agent completions use temperature `1.0`, following the user-authorized
Qwen3.8 sampling change. Acceptance-plan and semantic-advisory settings are
independent. Existing registered experiments retain their original binaries
and sampling settings. This change is not evidence of improved quality; future
replicates must measure repetition, reasoning exits and verified outcomes.

Provider snapshots include the requested completion settings and the mapped
provider controls. Run summaries use `max_tokens` in place of `num_predict`.
The summary records the requested budget; provider snapshots record its
resolved value. Credentials are not part of these records.

Mojentic 2.1's tool-capable oMLX stream exposes content, reasoning, and tool
calls. It does not expose token timing metrics or intermediate tool fragments.
The harness retains its fallback patience projection and local process/GPU
checks. Model residency remains unknown. Throughput history is filtered by
selected provider, model, host and context band; runner checks use that provider.
Performance and reasoning-level effectiveness need separate measured
runs; this migration makes no performance claim.

### Native reasoning prototype

The temporary Git-pinned Mojentic prototype fixes the Generation39 transport
limitations. It represents native assistant reasoning separately from content,
keeps named Ollama effort values distinct, and supports xhigh. The lockfile and
Cargo.toml identify the exact library commit; return to a published dependency
when the coordinated library increment is released.

Use `--reasoning-profile qwen3.8 --reasoning-effort medium` to validate the model's
supported controls before HTTP. The profile accepts disabled, low, medium and
xhigh; high is rejected rather than silently translated. Omitted effort still
means an unknown provider default. An omitted profile delegates capability
validation to the server; unknown model names are never matched heuristically.

`--preserve-reasoning` opts into native completed-response history. It is off by
default so retention can be varied independently from effort. The worker keeps
reasoning with its assistant tool request or final response, includes its size
in context accounting, and exposes it in exact snapshots and native transcripts.
File-read consolidation retains reasoning in its original assistant turn while
projecting the tool evidence separately; it does not attach reasoning to an
invented tool request. Existing context rewriting policies still apply.

Cancelled/capped partial chains remain separate from completed native history;
`--reasoning-checkpoint-tokens` retains its independent experimental meaning.
The oMLX gateway requests chat_template_kwargs.preserve_thinking=true when native
retention is enabled. Forced server settings can override that request, so
benchmark registration must record server settings too. Sampling, stop budgets,
and advisory defaults are unchanged by these flags.

`tests/omlx_gateway.rs` checks production CLI controls and history at mock HTTP
boundaries. Mojentic also tests both local gateways and broker tool loops.
These are deterministic compatibility results, not evidence of improved coding
quality. Generation40 preserves the implementation and validation evidence.

## Build and Test

Git workspaces receive an entry stash inventory before agent tools run. DONE
is withheld for new unresolved stashes, missing baseline stashes or an inventory
that cannot be verified. Existing stashes remain untouched. The agent decides
how to resolve its work; the harness never pops or drops it automatically.
Object IDs identify entries, and shared-repository attribution remains uncertain.
See [stash completion checks](STASH-COMPLETION-2026-10.md) for trace fields,
deterministic tests and the restored-implementation diagnostic.

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

### Request recovery

Recovery is disabled by default, with one wire attempt per completion. Opt in
explicitly; wire attempts do not consume model interactions or replay tools:

```bash
adaptive-agent-harness run --experiment /work/new-cell --provider ollama \
  --recover-requests --recovery-max-attempts 2 \
  --recovery-base-delay-ms 100 --recovery-delay-ceiling-ms 30000 \
  --recovery-budget-ms 60000 \
  --recovery-admission-command /opt/operator/owned-receipt \
  --recovery-capture-dir /work/private-wire
```

The trusted admission executable must be outside the model tool root. It receives
safe failure metadata and the immutable encoded request SHA-256 on stdin and must
return proof bound to the previous attempt and logical request. A plain allow
flag, aggregate activity, cancellation or model residency cannot admit a retry.
Pending admission is cancellable and expires after five seconds. Semantic partial
output ends with interruption and cannot be continued by another wire attempt.

`--recovery-capture-dir` is also allowed without `--recover-requests`, providing
explicit one-attempt capture. Credential checks cover each reasoning/content
channel and partial tool argument, name and ID fields, including JSON-escaped
values. Unrelated content cannot release a held reasoning credential prefix.
Captures exclude headers, withhold credential echoes
and label redaction and exactness. SIGINT and SIGTERM persist terminal lifecycle
and failure evidence before exit. Recovery budgets begin after failure; they are
not total-generation timeouts. See [recovery conformance](RECOVERY-CONFORMANCE.md)
for proof schema, safe capture limits and the external release-worker fixture.

### Native initial-context assembly

Every run now constructs one authoritative initial-context packet before the
worker starts. By default that packet contains the resolved task and profile
guidance. `run` and `run-sequential` can also accept an adapter-owned guidance
catalog:

```sh
cargo run -- run \
  --experiment ../Experiments/GenerationN \
  --initial-context-catalog context-catalog.json \
  --semantic-advisor-model Qwen3.8-27B-MLX-8bit
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

### Acceptance coverage ledger

`run --acceptance-ledger` enables an experimental pre-worker planning phase.
The isolated advisor first produces a deterministically validated atomic
acceptance plan, then a validated set of high-risk interactions between those
requirements. The accepted entries are rendered once into the retained worker
packet with their public provenance and suggested evidence.

The treatment adds `submit_acceptance_evidence`. The worker may cite one
observation for several stable ledger IDs. A submission records coverage at the
current workspace-mutation epoch; a later evidence-invalidating mutation makes
that coverage stale. `DONE` is withheld until every atomic and interaction ID
has current coverage.

Ledger coverage is explicitly advisory. It does not convert model-authored
tests, inspections, or claims into validation authority. Adapter-declared
probes retain their existing terminal authority and must still pass after the
latest relevant mutation. If either planning stage exhausts its bounded
structured-output attempts, the treatment run fails closed rather than
silently falling back to a control packet.

### Bounded reasoning continuity

`run --reasoning-checkpoint-tokens N` enables an experimental continuity aid
for hidden-only no-action turns. The harness retains at most the approximate
`N`-token tail of the model's own hidden reasoning and supplies that tail once
in the next ordinary user turn. The checkpoint is explicitly labeled as
incomplete, self-generated continuity state rather than task authority.

The default is zero, which preserves the prior prompt and provider request.
Enabling the treatment does not disable reasoning or narrow the tool surface.
Only the newest checkpoint is retained. Trace events record size, omitted
prefix size, and a SHA-256 digest; the exact checkpoint remains available in
the provider-request snapshot rather than being duplicated into summary
events.

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
request uses native `enable_thinking=false` (oMLX) or `think=false` (Ollama),
retains the authoritative
failure packet and restricted repair tools, and explicitly states that the
interrupted hidden reasoning was not retained. The native action-only request is
reserved for this post-validation repair boundary. Before source mutation,
ordinary reasoning and tool use remain interleaved; hidden-only no-action turns
receive bounded continuation guidance rather than a forced native mode switch.
The action-only provider snapshot records `reasoning_effort: disabled`,
`thinking_disabled: true`, and the effective cap source `provider_disabled`.
The ordinary `constrained` variant retains the configured worker reasoning effort.

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

Every successful read of an unchanged file is rendered as one authoritative,
event-sourced projection in provider context. Version-three projections put
the content in a line-numbered text view (`1|content`) and explicitly report
whether it is `complete` or `partial`; partial projections name exact missing
ranges. Later reads update that same path-and-mutation-epoch view instead of
adding another provider-visible read result. The content bound scales with the
actual provider window and is capped, while raw read events remain unchanged in
the trace only. Read-only shell effects do not split a file epoch, but confirmed
workspace mutations do.

Experiments are intentionally run from separate, scoped workspaces. Generated
agents should see their work tree and task-relevant guidance, not experiment
management files or unrelated repository context.

## License

Licensed under the MIT License. See [`LICENSE`](LICENSE).

## Controlled provider diagnostics

The `provider_benchmark` example exercises both Mojentic gateways without the
adaptive coding policy. It provides a long-prefix reuse probe and a sequential
receipt-tool session. It saves exact requests, per-call timings and native
Harness traces. By default, reasoning is disabled and each response is capped
at 128 tokens, preserving the Generation38 configuration.

Build with `cargo build --release --locked --example provider_benchmark`, then
run `target/release/examples/provider_benchmark CONFIG.json` under the workspace
memory/ownership guard. Example configuration:

```json
{
  "provider": "omlx",
  "model": "Qwen3.8-27B-MLX-8bit",
  "workload": "tool_chain",
  "nonce": "unique-session-id",
  "output": "/absolute/path/to/new-output-directory",
  "prefix_lines": 256,
  "rounds": 24
}
```

Use `prefix` for unchanged, suffix-changed and early-changed prefix probes.
Use `ollama` with its model ID to select the other gateway. Host/authentication
settings come from the gateways' environment variables. The output directory
must not already exist. The receipt tool has no shell or file access.

Generation38 contains the guarded orchestration, raw capture, mock wire tests
and analysis. Cache counters have provider-specific meanings; gateway action
latency is not automatically server prefill time. Unload between sessions does
not clear persistent caches. A semantic failure is recorded in result.json;
process success alone does not mean the task passed.

The diagnostic config also accepts `reasoning_effort` (default disabled, null
for provider default), `preserve_reasoning` (default false), `max_tokens`
(default 128, range 1–8192), `temperature` (default 0), and `reasoning_profile`.
The profile is a library capability object, for example:

```json
{
  "name": "qwen3.8",
  "supported_efforts": ["disabled", "low", "medium", "xhigh"],
  "supports_reasoning_history": true
}
```

Reasoning is permitted by semantic checks only when the configured effort
allows it; the same receipt/order/final-answer checks still apply. Current
production coding runs use oMLX; the controlled diagnostic can select either
provider. Register changed settings before running a new measured cell.

### Local benchmark activity counters

`benchmarks/provider_activity.py` interprets the verified oMLX 0.7 status API.
Its active count includes output collectors for waiting requests. One queued
request can therefore report both active=1 and waiting=1; these counts must not
be added or treated as separate clients. Multiple-request evidence is
`max(active, waiting) > 1`, while idle requires both zero. This helper does not
establish request ownership or replace model, memory and admission checks.
Generation42 preserves installed source evidence and the guarded integration.
Run its deterministic tests with:

```sh
python3 -m unittest discover -s benchmarks -p 'test_provider_activity.py' -v
```

## External runner progress

Missing or incomplete process, residency, or accelerator telemetry remains
`ProgressUnknown` after the throughput projection expires. A quiet stream is
not sufficient to cancel inference. Stall confirmation requires error-free,
affirmative inactivity on every sampled activity channel, followed by repeated
checks beyond the projection. Current process/GPU samplers do not establish
all of those negatives for an external provider, so automatic stall cancellation
fails closed there. This does not prove remote termination or authorize a retry.

`fixtures/progress/worker_pause.py` exercises the public worker with scripted
Ollama and oMLX streams pausing 390 seconds before resuming. It asserts retained
reasoning and tool feedback, one tool execution, two logical requests and two
wire attempts. A synthetic throughput registry makes the normal projection
expire during the pause; it is not real model performance evidence. Run this
fixture with a portable worker wrapper and `PAUSE_WORKER_PORTABLE=1` to verify
unavailable external telemetry. Unit tests separately verify repeated reliable
inactivity and interruption of a genuinely quiet stream.
