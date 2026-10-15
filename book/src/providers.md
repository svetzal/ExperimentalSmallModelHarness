# Providers and models

Microgentic supports two local providers: oMLX and Ollama. The default provider
is oMLX. Choose your default with `microgentic setup`, or for one run with
`--provider`.

## Server addresses and credentials

Microgentic reads server addresses and credentials from environment variables.
Setup does not store them.

| Variable | Meaning | Default |
| --- | --- | --- |
| `OMLX_HOST` | oMLX server URL, without `/v1` | `http://localhost:8000` |
| `OMLX_API_KEY` | Bearer token for an authenticated oMLX server | Not set |
| `OMLX_TIMEOUT` | Timeout for calls that do not stream, in milliseconds | `600000` |
| `OLLAMA_HOST` | Ollama server URL | `http://localhost:11434` |

The gateway adds `/v1` to the URL. Streamed replies have no fixed timeout.
Microgentic decides how long to wait from the model's measured throughput, so a
slow local model gets time to finish.

## Models

| Provider | Default model |
| --- | --- |
| oMLX | `Qwen3.8-27B-MLX-8bit` |
| Ollama | `qwen3.8:27b-mxfp8` |

To use another installed model, give its ID to `setup`, to `run --model`, or to
`/model` in the conversation. `/model` with no ID shows the models the provider
reports.

## Reasoning effort

The effort levels are `disabled`, `enabled`, `low`, `medium`, `high`, and
`xhigh`. If you do not set an effort, the model uses its own default.

The provider receives the effort in its own form. On oMLX, `disabled` turns off
thinking and the named levels set `reasoning_effort`. On Ollama, `disabled` and
`enabled` set `think` to false or true, and the named levels set `think` to the
level name. The real effect depends on the model's chat template.

## Output and context limits

These `run` flags control request size:

| Flag | Default | Effect |
| --- | --- | --- |
| `--max-tokens` | 16384 | Maximum generated tokens per request, including reasoning |
| `--context-window-tokens` | 262144 for the default oMLX model | Context window for Microgentic's accounting; Ollama also receives it as `num_ctx` |

For oMLX, set the real context limit on the server. The oMLX gateway does not
send Ollama-specific settings.

## Settings file

Setup saves its choices in `$XDG_CONFIG_HOME/microgentic/config.json`, or in
`~/.config/microgentic/config.json` when `XDG_CONFIG_HOME` is not set. Use
`--config <path>` to read a different file. Flags on a command override the
saved values.

Benchmark mode and the `adaptive-agent-harness` compatibility executable do not
read this file.

## Advanced context settings

Two settings exist for experiments that measure how much standing context a
model handles well. They are not in setup or the conversation view. Set them in
`config.json`, or with the matching flag for one run. A flag overrides the file,
and the file overrides the default.

| Key in `config.json` | Flag | Default | Effect |
| --- | --- | --- | --- |
| `standing_context_limit` | `--standing-context-limit` | 0.15 | All standing context (project guidance now) as a fraction of the context window. Traces record each request against it. It is not enforced yet. |
| `guidance_fraction` | `--guidance-fraction` | 0.125 | The project guidance budget as a fraction of the window, between 500 and 8,000 tokens |

Each value must be above 0 and at most 1. Traces record the effective values
beside each context ledger. Benchmark mode ignores the file and records a flag
only when you give it.
