# Harness Trace Schema

The harness trace is the evidence needed to explain a run after the fact. It
separates two complementary views of every model call:

- `llm.context_assembly.ledger` is the compact measurement view. It records
  message roles, inclusion reasons, sizes, estimated tokens, pressure, and
  context deltas.
- `llm.provider_request.assembled` is the fidelity view. It records the exact
  ordered messages, active tool descriptors, model, and completion settings
  passed to the worker provider.

Neither event replaces the other. The ledger supports bounded statistical
analysis; the exact snapshot supports causal and forensic review.

Provider responses have a separate fidelity surface. A final non-empty tool
batch emits one aggregate `llm.stream.tool_calls_completed` event followed by
one `llm.response.tool_call.normalized` event per response call. Stream progress
frames do not create executable tool calls.

## Provider-bound request snapshots

Immediately before each worker provider call, the harness emits:

```json
{
  "kind": "llm.provider_request.assembled",
  "payload": {
    "schema_version": "provider_request.v1",
    "turn": 2,
    "llm_call_depth": 0,
    "model": "qwen3.6:35b-a3b-coding-nvfp4",
    "messages": [],
    "tools": [],
    "completion": {}
  }
}
```

`messages` is the exact serialized `LlmMessage` sequence supplied to the
gateway. Its order is authoritative. It includes retained system and user
messages, assistant tool requests, tool results, harness follow-ups, repair
instructions, and any compacted content actually visible to the model.

`tools` contains the exact active tool descriptors. It is empty when a policy
has constrained the call to a final response without tools. `completion`
captures all completion fields supplied to the gateway, including reasoning
effort and response format.

The event is written after deterministic context and repair-depth checks and
immediately before `complete_stream`. A rejected call therefore has a ledger
but no provider-request snapshot. A provider call that was actually attempted
must have both.

## Provider response tool calls

Every call in a final provider tool batch receives a normalized record:

```json
{
  "kind": "llm.response.tool_call.normalized",
  "payload": {
    "schema_version": "response_tool_call.v1",
    "turn": 1,
    "llm_call_depth": 2,
    "response_index": 0,
    "response_tool_call_count": 3,
    "tool_call_id": "call-0",
    "tool_name": "read_file",
    "argument_keys": ["path"],
    "arguments_json": "{\"path\":\"src/lib.rs\"}",
    "arguments_preview": "{\"path\":\"src/lib.rs\"}",
    "arguments_complete": true,
    "arguments_json_chars": 21,
    "arguments_sha256": "..."
  }
}
```

Canonical argument JSON is retained in full through 4,096 characters. Larger
arguments retain their keys, bounded preview, exact character count, and full
SHA-256 while `arguments_json` is null. This keeps ordinary read paths exact
without duplicating an arbitrarily large whole-file write into the trace.

These are response records, not tool-effect records. A hard stop can leave
later response calls unexecuted; their normalized records still show what the
gateway returned. `analyze-trace` compares the aggregate final batch counts
with per-call records and reports `response_tool_call_trace_complete` as false
for a partial response trace.

## Call chronology

The normal causal order is:

1. `llm.context_assembly.ledger`
2. progress projection and initial progress status
3. `llm.provider_request.assembled`
4. streamed reasoning, content, tool-call progress, and provider metrics
5. final tool-batch aggregate and normalized per-call response records
6. `llm.context_assembly.response`
7. assistant tool request and tool-result retention events
8. deterministic tool, validation, and harness-policy events
9. the next context ledger and provider-request snapshot

Turn is the outer harness iteration. LLM call depth is the zero-based provider
call within that turn. Tool results normally increase depth without advancing
the turn; a harness follow-up normally advances the turn and resets depth to
zero.

## Performance assessment

Before attributing a failure to model capability:

1. Locate the failed or repeated action in the chronological trace.
2. Open the matching `llm.provider_request.assembled` event.
3. Read the exact ordered messages, especially new or changed user messages,
   retained tool results, and compaction markers.
4. Compare the request with the preceding snapshot to identify what the
   harness added, retained, changed, or removed.
5. Use the adjacent ledger for context pressure and size, then inspect the
   model stream, tool effects, harness decisions, and independent verifier.
6. Classify the result as model behavior, harness context/policy behavior, an
   environment problem, or inconclusive evidence.

`analyze-trace` reports `provider_request_snapshot_count` and
`provider_request_trace_complete`. Treat a trace as complete for exact-context
analysis only when every context-ledger call has a corresponding snapshot.
Legacy traces remain analyzable, but missing prompt text must not be inferred
from component sizes or policy-event summaries.

For response batching claims, also require
`response_tool_call_trace_complete=true`. Compare
`reported_response_tool_call_count`, `normalized_response_tool_call_count`, and
`max_response_tool_call_batch_size` before interpreting executed tool effects.

## Data handling

Exact snapshots intentionally make trace files larger and more sensitive. They
may contain task text, tool results, generated source embedded in tool-call
arguments, local paths, or other material sent to the provider. Store traces
outside the model-visible workspace, preserve them as experiment evidence, and
apply the same access controls as the source task and generated artifacts.

Tool-effect events remain independently useful because they record bounded
results, exit status, fingerprints, and validation classifications. When a
tool result is retained for another model call, the next exact request snapshot
is the authority for the precise representation the model received.

## Rendering transcripts

The Harness binary can render trace files or recursively discovered trace
directories without invoking a model or requiring Python:

```sh
adaptive-agent-harness render-transcript traces/ \
  --output transcript.html \
  --title "Agent Session Transcript"
```

The output is one portable HTML file with embedded data, CSS, and JavaScript.
It keeps model calls concise by default and progressively reveals exact
messages, tool schemas, completion settings, reasoning, raw tool effects, and
harness-policy payloads.

Optional independent evidence uses this generic shape:

```json
{
  "schema_version": "transcript_evidence.v1",
  "sessions": [
    {
      "label": "session-label",
      "reward": "1",
      "passed": 6,
      "failed": 0,
      "output": "External verifier output"
    }
  ]
}
```

Each session entry requires either `label` or `trace`. `trace` matches the
canonical trace path recorded by the renderer; `label` matches the displayed
session label. Evidence is display-only and never changes the harness trace or
its analyzer classification.
