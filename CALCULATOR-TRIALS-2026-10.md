# Calculator tool trials

## Observation and hypothesis

A real TUI session on October 15 (Qwen3.8-27B-oQ4e-mtp, effort medium, prompt
"I want you to speed up the opening animation by 10% in the tui") did its
arithmetic in its thinking. It summed the eleven stage holds by hand, divided
by 1.1, and carried a wrong intermediate ("Total = 1354/1.1 ≈ 1230?") before it
redid the work. The trace is
`~/.local/state/microgentic/runs/run-R1q399/traces/run-20261015T080403Z.jsonl`.

Hypothesis: with a `calculate` tool and one line of guidance, small models
hand arithmetic to the tool unprompted, their first expression parses, and the
errors they hit tell them how to fix the expression.

## Method

Stacey authorized a small number of live trials for this change only. All
trials used the local oMLX server, Qwen models only, and throwaway projects
under `/private/tmp/claude-501/calc-trials/`. No prompt mentioned the tool.

- `run` trials use `microgentic run`, the task path with its unchanged
  profile guidance.
- `chat` trials use `microgentic conversation-worker`, the TUI conversation
  path.
- `pacing-repo` gives the model a throwaway copy of this repository with
  `PACE` reset to 1.0 and her original prompt, to recreate her session.
- `pacing` (round 1 only) gave the model `launch.rs` alone.

Builds:

| Build | Change |
|---|---|
| e02bfce | The tool with its first description. No guidance line. |
| tuned 1 | Description leads with "do arithmetic here instead of in your head"; one guidance line added wherever the tool is offered. |
| tuned 2 | Batches can name values: `total = 140 + 90; total / 1.1`. |
| tuned 3 | A batch with some good lines answers instead of failing; trailing words after an expression get their own hint. |

## Trials

| Round | Build | Path | Trial | Model | Calls | First expression parsed | Errors hit | Outcome |
|---|---|---|---|---|---|---|---|---|
| 1 | e02bfce | run | pacing (file only) | oQ4e-mtp | 0 | n/a | none | Timed out at 25 min trying to rebuild the missing crate around the lone file. Summed the holds in its head as 1480 (the true sum is 1474). |
| 1b | e02bfce | run | pacing-repo | oQ4e-mtp | 1 | yes | none | Batched the 11-term sum and the division. PACE 1.1, test window 1290-1390 ms. Correct. |
| 1b | e02bfce | run | recipe | oQ4e-mtp | 0 | n/a | none | Built a Rust program to compute the answer, as the task path asks. |
| 1b | e02bfce | run | sales | oQ4e-mtp | 0 | n/a | none | Built a Rust program. Totals correct. |
| 1b | e02bfce | run | interest | oQ4e-mtp | 2 | yes | none | Used the tool, then also built a program. |
| 1c | e02bfce | chat | recipe | oQ4e-mtp | 0 | n/a | none | Mental arithmetic. Answer correct (35.75 cups, 2 bags). |
| 1c | e02bfce | chat | sales | oQ4e-mtp | 1 | yes | none | Batch of three. Correct. |
| 1c | e02bfce | chat | interest | oQ4e-mtp | 1 | yes | none | Wrote the power as `exp(84*ln(1.00375))`. Correct ($3,423.63). |
| 1c | e02bfce | chat | pacing | oQ4e-mtp | 1 | yes | none | 1474, then 1340. Correct. |
| 2 | tuned 1 | chat | recipe | oQ4e-mtp | 1 | yes | none | Now used the tool. Correct. |
| 2 | tuned 1 | chat | sales | oQ4e-mtp | 1 | yes | none | Correct. |
| 2 | tuned 1 | chat | interest | oQ4e-mtp | 2 | no | `A = ...; A - 2500` (naming a value) | Retried with the number pasted in. Correct. |
| 2 | tuned 1 | chat | pacing | oQ4e-mtp | 1 | yes | none | Correct. |
| 3 | tuned 2 | chat | interest | oQ4e-mtp | 1 | yes | none | Named `A` and `interest`. Correct. |
| 3 | tuned 2 | chat | recipe | oQ4e-mtp | 1 | yes | none | Correct. |
| 3 | tuned 2 | run | pacing-repo | oQ4e-mtp | 0 | n/a | none | Took "about 1480 ms" from the doc comment and wrote "about 1345 ms". Test window still passes. |
| 3 | tuned 2 | run | pacing-repo | MLX-8bit | 1 | partly | `(1480/1.1/1000) as secs` (trailing words) | Batch of five with a named total; one line failed, so the call was a tool error, but the model used the good lines. 1474 to 1340. Correct. |
| 4 | tuned 3 | chat | sales | oQ4e-mtp | 1 | yes | none | Named `total`, `avg`, `pct_change`. Correct. |
| 4 | tuned 3 | chat | pacing | oQ4e-mtp | 2 | yes | none | Correct. |
| 4 | tuned 3 | run | pacing-repo | oQ4e-mtp | 1 | yes | none | Used the tool on the doc comment's 1480, not the summed 1474: "about 1345 ms". Test window 1300-1400 passes. |

## Results

- On the conversation path, the model called the tool in 3 of 4 trials
  without guidance, and in 8 of 8 trials after the guidance line.
- Every first expression parsed except two: a value-naming line and trailing
  words. Both led to changes (tuned 2 and tuned 3), and the model then named
  values successfully in three later trials.
- Every answer computed with the tool was arithmetically correct.
- On the `run` task path, the tool was used in 3 of 4 pacing trials. One of
  those runs computed with the doc comment's rounded "about 1480 ms" rather
  than summing the timeline, so it was exact about an inexact input; the run
  without the tool made the same choice. The tool cannot fix the choice of
  input, so the comment now states 1474 ms.
- The task path's profile guidance asks for working code, so arithmetic
  questions sent to `run` produced small Rust programs. That is existing task
  behavior and was not changed.

## Decisions taken from the trials

- Name: `calculate`. The model found and used it on the first call in every
  trial where it chose to; no alternative name was needed.
- Value naming within a batch, because the model reached for it unprompted.
- A batch answers when at least one line succeeds; the failing lines carry
  their own errors. A call with no usable line is a tool error.
- The guidance line is added only where the tool is offered, so benchmark and
  historical runs are unchanged.

## Limits

These are a handful of trials on two Qwen models, not a measurement of model
capability. Timing varies with the local server's load.
