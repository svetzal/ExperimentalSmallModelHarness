# Traces and transcripts

Every run and every conversation exchange writes a JSONL trace. A trace records
what Microgentic sent to the provider, what came back, each tool call, and each
decision the runtime made. Use traces to find out why a run went the way it
did.

## What a trace contains

- an exact, ordered snapshot of each request sent to the provider
- a context ledger for each request, with the reason each part is included and
  its estimated size
- streamed reasoning and reply text
- each tool call and its result
- validation results, stop decisions, and the final outcome
- the project guidance that was discovered and delivered
- the model's context window and where it came from: your
  `--context-window-tokens`, the provider, or Microgentic's model table
- for each request, the time to the first token, what kind of token it was
  (reply text, thinking, or a tool call), and how long the request took
- for everyday runs and the conversation, the residency of each part of each
  request: why it is there, its size, the turn that added it, whether it is
  pinned, and whether Microgentic has shortened it. The standing context (the
  project guidance) is measured against a limit you can set; see
  [Providers and models](providers.md#advanced-context-settings).

The full schema is in
[`TRACE_SCHEMA.md`](https://github.com/svetzal/microgentic/blob/main/TRACE_SCHEMA.md).

## Find a trace

In the conversation, an exchange that uses project tools ends with an
`Evidence:` line that gives its trace location. All traces are below the
run-state directory. See [Files and locations](locations.md).

## Summarize traces

```sh
microgentic analyze-trace ~/.local/state/microgentic/runs/run-*/traces/
```

The summary reports context assembly, tool payload size, snapshot coverage, and
tool-call counts. It also tells you if a trace is incomplete, so that you do not
draw conclusions from partial evidence.

## Render a transcript

```sh
microgentic render-transcript traces/ \
  --output transcript.html \
  --title "Agent session"
```

The transcript is one self-contained HTML file. It shows the provider input,
reasoning, replies, tool effects, runtime decisions, and context measurements as
one timeline. Give it trace files or directories. It finds the Microgentic
traces in them and ignores other JSONL files.
