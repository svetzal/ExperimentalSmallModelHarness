# Project guidance trials

## Observation and hypothesis

Before this change Microgentic read no `AGENTS.md`. Project conventions reached
the model only through an adapter-owned catalog, which everyday users do not
write.

Hypothesis: with discovered guidance in the system context and nested guidance
delivered on first use, a small model follows distinctive project conventions
without being told them in the prompt, and a nested file's rule wins over a
conflicting root rule inside its directory.

## Method

Stacey authorized a few live trials for this change only. All trials used the
local oMLX server, Qwen models only, and throwaway copies of a scratch project
under `/private/tmp/claude-501/guidance-trials/`. Build: `1825e3d`.

The scratch project is a small Python package in a Git repository. Its root
`AGENTS.md` says:

- run the tests with `make check`, never `cargo test`, `pytest` or
  `python -m unittest` directly;
- never edit files under `generated/`; change `tools/gen_schema.py` and run
  `make generate` instead;
- write documentation in American English;
- put the path of the main file changed first in the final answer.

`docs/AGENTS.md` conflicts with the root: documentation in `docs/` is in
British English, with lines under 72 characters. No prompt mentioned any of
these rules.

## Trials

| Trial | Path | Model | Prompt | Guidance seen | What the model did | Outcome |
|---|---|---|---|---|---|---|
| tests | run | oQ4e-mtp | Add `double(x)` to `src/calc.py` with a test. | root | Ran `make check` twice. Final reply segment starts with `src/calc.py`. | Followed. |
| tests-control | run | oQ4e-mtp | Same, with `--no-project-guidance`. | none | Ran `python3 -m unittest discover -s tests -v`. Final reply does not start with a path. | Convention not known, as expected. |
| generated | run | oQ4e-mtp | "The VERSION in generated/schema.py should be 2. Please update it." | root | Edited `tools/gen_schema.py`, ran `make generate`, then `make check`. Never edited `generated/` directly. Reply starts with `tools/gen_schema.py`. | Followed. |
| docs-nested | run | oQ4e-mtp | Write `docs/colors.md` about three color themes. | root, then `docs/AGENTS.md` delivered after its first `docs/` tool call | Said the nested file conflicts with the root and takes precedence there. Wrote "colour" throughout, all lines under 72 characters (checked with `awk`). Reply starts with `docs/colors.md`. | Nested rule won. |
| chat-tests | chat | MLX-8bit | "How do I run the tests in this project?" | root | Answered `make check` and said not to call the runner directly. | Followed. |

## Results

- All four guided trials followed every applicable convention. The control
  without guidance did not, which shows the behavior came from the guidance
  and not from the project's files alone.
- The nested file arrived when the model first worked in `docs/`, and the model
  named the precedence rule from the framing when it resolved the conflict.
- Each trial also created a `.gitignore`. That comes from the existing task
  guidance for new projects, not from project guidance.

## Limits

Five trials on two Qwen models are a check, not a measurement. The registered
baseline comparison (guidance on versus off, five replicates) remains the
HYP-R-4 cell in [`CONTEXT-RESIDENCY.md`](CONTEXT-RESIDENCY.md).
