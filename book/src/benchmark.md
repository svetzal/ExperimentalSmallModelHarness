# Benchmark mode

Benchmark mode runs isolated task contracts for registered experiments. It keeps
the task prompting, validation authority, and runtime defaults that the
experiments were registered with. Everyday use does not need it.

```sh
microgentic benchmark run --experiment ./cell --dir ./cell/workspace
microgentic benchmark run --experiment ./cell --contract contract.json \
  --dir ./cell/workspace --provider ollama --model qwen3.8:27b-mxfp8 \
  > result.json 2> diagnostics.log
microgentic benchmark run-sequential --experiment ./cell \
  --dir ./cell/workspace --artifact answer.txt --expected-artifact expected.txt
```

## How it differs from `run`

- `--experiment` is required. The task comes from `task.md` in the experiment
  directory, or from `--goal` or `--contract`. A contract takes precedence over
  a goal.
- The complete JSON summary always goes to stdout.
- Your saved settings, `--config`, stdin, and conversation history have no
  effect. Give inputs as files.
- Project guidance is not read, and the calculator is not offered, so provider
  requests stay the same as in earlier versions.
- A task ends with `DONE` or `FAIL`, and executor-owned probes decide whether
  work is complete. The model's claim alone does not.
- The default provider is oMLX. Benchmark adapters choose their provider
  explicitly. Server addresses and credentials still come from the environment.

Each run starts with fresh task context. Sequential attempts keep their declared
artifact through the retry policy.

## Compatibility executable

The `adaptive-agent-harness` executable keeps the old flags and JSON output for
existing experiment scripts. Its library is still `adaptive_agent_harness`.
