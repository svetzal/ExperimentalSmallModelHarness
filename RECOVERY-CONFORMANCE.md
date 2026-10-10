# Recovery integration evidence

## Status: prerequisite gap, integration not complete

The reviewed Mojentic revision is
`bc8150a381a540e6320cd4fd91a8bbecadb9a735`. Harness starts at
`86ce284a5cf12f2c506ca4036366166aa179dc11`. This Foundry worktree leaves changes
uncommitted; it has no new reviewed landed Harness commit. The dependency manifest
and lockfile remain unchanged.

The reviewed recovery API cannot preserve all existing Ollama worker progress
behavior. The normal provider emits `StreamChunk::Progress` for every decoded
frame, including an empty nonterminal frame. Harness consumes those events in
`src/agent.rs` to refresh observed activity and distinguish tool generation from
reasoning. That evidence affects stall detection, reasoning interruption and
frame accounting. Losing it would change existing controls and trace evidence.

`RecoveryStreamEvent` exposes content, reasoning, tool fragments, completion and
failure. It has no nonsemantic frame-progress event. Lifecycle observers receive
progress at transitions, not for each frame while an attempt remains active.
`WireObserver::Body` exposes HTTP byte chunks, not decoded provider frames. Adding
another provider parser in Harness would duplicate Mojentic's provider semantics.

The recovery convenience `LlmGateway::complete_stream` adapter also drops metrics.
The raw recovery API retains terminal usage and timing in the completed response's
`ResponseEvidence`; a Harness adapter can map those to existing worker metrics.
**Terminal metrics alone are not a substitute for progress during generation.**
This finding concerns integration evidence; it does not claim a library runtime
defect or full library conformance.

The prerequisite is a reviewed library API exposing provider frame progress through
recovery without weakening semantic interruption or cancellation. The coordinator
must resolve that upstream and supply the reviewed revision, then own any separate
manifest/lockfile commit. This task changes neither Mojentic nor the pin. No live
inference, historical experiment writes, retry loop, agent replay or tool replay
was performed. Recovery flags and production recovery dispatch remain unimplemented.

## Assertion-bearing diagnostics

`actual_worker_provider_paths_preserve_progress_prerequisites` checks both real
CLI provider paths, one HTTP request each, native reasoning, tools and token
settings on wire, and the configured model-interaction limit. It verifies Ollama
empty-frame progress and terminal metrics and checks credentials are absent from
artifacts. Its scope is existing behavior with recovery disabled.

`reviewed_recovery_stream_does_not_expose_worker_frame_progress` checks identical
encoded requests across legacy, raw recovery and convenience APIs, four legacy
progress frames and one legacy metrics frame. It verifies that raw recovery yields
reasoning, content and completion only, that terminal usage and timing survive,
and that the convenience API yields neither progress nor metrics. This is a
pinned public library diagnostic, not worker recovery conformance.

Both diagnostics use loopback scripted HTTP servers and synthetic data. The worker
fixture creates a UUID-shaped credential in memory for oMLX and passes it only
through the subprocess environment. It does not record authorization headers or
environment values. It clears ambient provider credentials and proxy configuration.
This checks absence of an unreflected synthetic credential; it **does not** test
credential echoes, redaction, admission evidence or opt-in capture safety.

Successful runs emit artifact paths. The worker fixture writes `worker.json` with
the actual executable SHA-256 and, for each provider, `request-1.body`,
`response.body`, `stdout.json`, `stderr.log`, the main JSONL trace and
`assertions.json`. The public library diagnostic retains three request bodies,
the scripted response body and `assertions.json` in its emitted temporary directory.
Test artifacts remain available for inspection. No headers are captured.

## Repeat against a frozen Linux release worker

Run in the compatible Ubuntu 24.04 Linux container with Python 3 available. Choose
a new artifact directory outside historical experiments for each invocation:

```sh
python3 fixtures/recovery/worker_prerequisites.py \
  --worker /absolute/path/to/adaptive-agent-harness \
  --artifacts /absolute/path/to/new-prerequisite-artifacts
```

The fixture accepts the coordinator's exact frozen executable; it does not build
or substitute another worker. A 30-second subprocess bound protects the scripted
fixture only. It adds no generation timeout to Harness. The fixture never contacts
a live model. This invocation repeats baseline diagnostics, **not** the requested
recovery matrix. `worker.json` records binary identity independently of source HEAD.

To repeat both Rust diagnostics, preserving their emitted artifact paths:

```sh
foundry capture -- cargo test --locked --test recovery_prerequisites -- --nocapture
```

## Trace fields and remaining evidence

The existing main trace records `llm.stream.progress` with turn, call depth,
provider, frame index, terminal state, content/reasoning lengths and tool fragment
counts. `llm.stream.metrics` records provider-reported token counts, timings and
throughput. `llm.stream.thinking` records reasoning progress. The diagnostic verifies
these existing fields; no recovery trace schema has been landed.

The requested logical/attempt IDs, wire accounting, structured final failure,
admission, retry delays, exhaustion, interruption and cancellation are not yet
integrated into the main Harness trace or transcript renderer. No conformance is
claimed for 503 recovery, Retry-After, 504 exhaustion, admission proof, pending
admission, cancellation, partial semantic output, tool execution once, permanent
truncated error bodies, capture failure, credential echoes across chunks or
terminal-only cancellation. CLI stream-drop and paused-consumer behavior are also
unexercised. Dependency diagnostics do not substitute for these worker-path tests.

## Validation

Full command logs are preserved under `target/recovery-gate-logs/`; the first
focused test run also has logs under the Foundry tool-log directory. The first
focused run exposed a fixture error (missing Ollama terminal `done_reason`), which
was corrected to `stop` before final validation. Gate outcomes and the release
worker hash are recorded below after the final runs.

The manifest uses Rust 2024 and Tokio. It has no explicit workspace resolver,
MSRV, default-feature policy, toolchain pin or CI workflows. These settings were
preserved. An explicit MSRV would help future dependency maintenance, but adding
one is outside this integration task.

Final results on October 10, 2026:

- Formatting: passed.
- Locked all-target/all-feature strict Clippy: passed.
- Locked all-target/all-feature tests: passed, 345 tests; 2 existing tests ignored.
- Locked release binary build: passed.
- Locked documentation build without dependencies: passed.
- Modified Markdown lint: passed.
- Unfiltered `cargo audit`: passed, 289 dependencies scanned.
- `cargo deny check`: failed on license requirements. No approved license policy
  is configured. Advisories, bans and sources passed. No policy was weakened and
  no advisory was suppressed.
- Exact release worker prerequisite fixture: passed for Ollama and oMLX.

The release worker SHA-256 is:

```text
daf871ce27560c6ce5e026c60a5accc5a98d34abff29898813ca7cdeb9d8fa29
```

Its bodies, traces and assertion records are under
`target/recovery-release-prerequisites/`. This hash identifies the baseline worker;
it is not evidence of an integrated recovery worker.

Final complete logs within `target/recovery-gate-logs/` have these stems (each has
`.stdout.log` and `.stderr.log` files):

```text
format:      3491683-1791629257745703138
clippy:      3501602-1791629392252949752
test:        3501647-1791629392624894340
release:     3493513-1791629278739035208
doc:         3501439-1791629380681356768
audit:       3492475-1791629271329405625
deny:        3492647-1791629272523272521
fixture:     3501561-1791629391076695677
diagnostics: 3499166-1791629323188749184
```

The Markdown log stem is recorded by the final lint invocation. The first failed
focused test logs are preserved separately in the Foundry tool-log directory:

```text
/home/svetzal/.foundry/tool-logs/3486333-1791629206586256313.stdout.log
/home/svetzal/.foundry/tool-logs/3486333-1791629206586256313.stderr.log
```
