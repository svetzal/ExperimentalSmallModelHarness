# Residency measurement

Date: 2026-10-15. Builds: `fde1f57` to `6d028b3`. Baseline: v0.4.0.

## What was added

This is step 2 of [context residency](CONTEXT-RESIDENCY.md): measure, and
change no behaviour.

- Everyday `run` and the conversation ask the provider for the model's context
  window and account against it. The order is `--context-window-tokens`, then
  the provider, then the model table. A failure falls back and the trace says
  why. On Ollama the accounting window is at most the `num_ctx` each request
  already sends.
- Every provider request records time to first token, the kind of that token,
  and its total duration.
- Every item in each everyday or conversation request has a residency record
  on its context ledger, and discrete events mark guidance activation, nested
  guidance references, and each existing compaction as a demotion.
- `standing_context_limit` (0.15) and `guidance_fraction` (0.125) are settings,
  with run flags.
- The context panel shows each element's reason and residency state, the
  status bar's `📜` counts standing items, and both use the known window.

## Benchmark continuity

The six continuity tests pass, and they now also check that benchmark traces
have no residency events, discovered window, or residency ledger fields. The
baseline check against a v0.4.0 build in a throwaway worktree gave equal
provider requests, packets, ledgers, and summary schemas:

| Binary | Mode | Cases | Result |
| --- | --- | --- | --- |
| `microgentic` | benchmark | contract, goal, sequential on oMLX and Ollama | 6 of 6 equal |
| `microgentic` | conversation | contract on oMLX and Ollama | 2 of 2 equal |
| `adaptive-agent-harness` | benchmark | contract, goal, sequential on oMLX and Ollama | 6 of 6 equal |
| `adaptive-agent-harness` | conversation | contract on oMLX and Ollama | 2 of 2 equal |

## Live check

Stacey authorized a few live trials for this change. All trials used local
servers, Qwen models only, effort low, and a scratch Git project under
`/private/tmp/claude-501/residency-trials/` whose `AGENTS.md` asks for one short
paragraph and the calculate tool for arithmetic. The conversation trials ran
the TUI's `conversation-worker` directly with a session file.

| Trial | Provider and model | Window in trace | Requests | Time to first token | Residency | Outcome |
| --- | --- | --- | --- | --- | --- | --- |
| everyday run, "What is 17.5% of 2,340?" | oMLX Qwen3.8-27B-oQ4e-mtp | 262,144, provider, `max_model_len`, 6 ms | 2 | 14,254 ms and 8,890 ms, both thinking | 10 then 12 items; base prompt, conversation turns, root guidance, tool descriptors, tool result; standing 134 tokens of a 39,321 limit | One `calculate` call; answered 409.5 |
| conversation, "Hello! What does this project ask of you?" | oMLX Qwen3.8-27B-oQ4e-mtp | 262,144, provider, kept in the session file | 1 | 9,075 ms, thinking | 10 items; standing 134 tokens; the status event carried `standing_items: 1` | Described the project rule |
| everyday run, same question | Ollama qwen3.8:27b-mlx | 32,768 (advertised 262,144 by `model_info.qwen3_5.context_length`), 29 ms | 2 | 108 ms and 80 ms, both thinking | 10 then 12 items; standing 134 tokens of a 4,915 limit | One `calculate` call; answered 409.5 |
| conversation, same greeting | Ollama qwen3.8:27b-mlx | 32,768, advertised 262,144 | 1 | 9,196 ms, thinking | 10 items | Described the project rule |

Every Ollama request, current and v0.4.0 with the same arguments, sent
`num_ctx` 32,768. Discovery did not change it.

The first Ollama run of the current build accounted against the advertised
262,144 tokens, while Ollama ran with 32,768. That is why the accounting
window on Ollama is now bounded by the `num_ctx` it is sent (`6d028b3`); the
table shows the runs after that change. The oMLX rows are from `ace6497`, the build
before it, which this change does not affect.

## Limits

Four trials on two Qwen models are a check, not a measurement. No trial
exercised nested guidance, references, or demotions; scripted gateway and unit
tests cover those. The baselines in the hypotheses still need registered cells.
