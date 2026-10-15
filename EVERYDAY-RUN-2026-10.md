# Everyday run replies

Date: 2026-10-15. Source baseline: `714c113`.

## Observation

An everyday `microgentic run` used the task path. The prompt was wrapped in the
coding profile's "Complete this benchmark task" template. After a reply without
DONE or FAIL, the runtime sent "Continue from the current experiment state" on
every iteration until `--max-iterations` ran out. One calculator question took
thirteen provider requests, and a greeting took ten.

## Change

Everyday `run` means a run without `--experiment`, `--goal` or `--contract`,
from the `microgentic` executable. It is the same predicate that turns on the
calculator and project guidance. These runs now use the reply path of the TUI
conversation, without its session file:

- The prompt is a scoped message with no inferred probes, as in the TUI. Shell
  fences in it are discussion. Piped stdin with `--prompt` stays labeled
  reference material.
- The system guidance is the conversational guidance. The task template is
  not used.
- Tools stay available. The loop continues while the model calls tools.
- A visible reply without tool calls ends the run, after the Git stash,
  post-write validation, requested-probe and acceptance-ledger checks. The
  trace records `session.reply.completed`.
- Empty and thinking-only replies get the conversational recovery prompts.
- Budgets, patience, cancellation and stop policy are unchanged. The runtime's
  authority rules are unchanged.
- The reply goes to stdout without surrounding blank lines. The `--json`
  summary is unchanged, and the exit code is 0 when the reply completes.

`AgentRunConfig.conversational` selects this path. The TUI conversation worker
always uses it.

Task paths are unchanged: `run` with `--experiment`, `--goal` or `--contract`,
`run-sequential`, `benchmark run`, `benchmark run-sequential`, and every
command of the `adaptive-agent-harness` executable.

## Deterministic evidence

Mock gateway request counts, on both oMLX and Ollama. "Before" is `714c113`.

| Scenario | Before | After |
|---|---|---|
| Plain question by `--prompt` (stdin gives the same after) | 10 | 1 |
| Question that needs one tool | 11 | 2 |
| Edit a file, run a check, report | 12 | 3 |
| Project guidance with two reads | 12 | 3 |
| Three calculator calls, then a reply | 13 | 4 |
| Review with a diff on stdin | not tested | 1 |

The tests in `tests/everyday_runs.rs` assert these counts exactly. They also
check that no everyday request contains the task template, a continuation
prompt or "reply exactly DONE", that stdout holds only the reply, and that the
exit code is 0. A separate test checks that `adaptive-agent-harness run
--prompt` still sends the task template and a continuation prompt.

The continuity tests in `tests/benchmark_continuity.rs` pass. The baseline
check against a v0.3.0 build gave equal provider requests, packets, ledgers
and summary schemas in benchmark mode (contract, goal and sequential cases)
and conversation mode, for both `microgentic` and `adaptive-agent-harness`.

## Live trials

Stacey authorized a few live trials for this change only. All trials used the
local oMLX server, `Qwen3.8-27B-oQ4e-mtp` with effort low, and throwaway
projects under `/private/tmp/claude-501/run-trials/`.

| Trial | Build | Prompt | Requests | Tool calls | Outcome |
|---|---|---|---|---|---|
| greeting | `dfa6551` | "Hello!" | 1 | none | A short greeting. Exit 0, 13 s. |
| arithmetic | `dfa6551` | "What is 17.5% of 2,340, plus 89?" | 2 | `calculate` once | Answered 498.5, which is correct. Exit 0, 25 s. |
| edit and check | `dfa6551` | "Add a function double(x) to calc.py that returns twice x, add a test for it, and run the tests." | 6 | `list_tree`, `read_file` ×2, `edit_file` ×2, `shell_command` ×2 | Added `double` and `test_double`, ran `python3 -m unittest`, and reported 2 tests passing. Exit 0, 62 s. |
| greeting | `55798a4` | "Hello!" | 1 | none | A short greeting, with no blank lines around it on stdout. Exit 0, 10 s. |

No request in any trial contained the task template or a continuation prompt.
The `dfa6551` trials printed blank lines before the reply on stdout: two for
the greeting, four for the arithmetic and fourteen for the edit. `55798a4` removes them.

## Limits

Four trials on one Qwen model are a check, not a measurement. The review
scenario has mock gateway coverage only.
