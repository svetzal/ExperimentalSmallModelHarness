# Get started

## 1. Start a model server

Start oMLX or Ollama, and install a model on it. The default models are
`Qwen3.8-27B-MLX-8bit` for oMLX and `qwen3.8:27b-mxfp8` for Ollama. You can
choose a different installed model during setup.

If your server is not at its default address, set `OMLX_HOST` or
`OLLAMA_HOST`. See [Providers and models](providers.md).

## 2. Choose your defaults

```sh
microgentic setup
```

On a terminal, setup asks for the provider, the default model, and the
reasoning effort. Without `--apply` it only shows the settings it would save.
When the settings are correct, save them:

```sh
microgentic setup --apply
```

Scripts can give the values directly:

```sh
microgentic setup --provider ollama --model qwen3.8:27b-mxfp8 \
  --reasoning-effort low --apply
```

Setup does not contact the provider and does not store credentials.

## 3. Open a conversation

Go to your project directory and start Microgentic:

```sh
cd ~/code/my-project
microgentic
```

The conversation starts with no task. Say hello, ask a question about the
project, or ask for a change. The model uses its tools when a request needs
them. See [The conversation](conversation.md).

## 4. Send one prompt from the shell

For a single prompt, use `run`. It reads the prompt from stdin or `--prompt`
and writes the reply to stdout:

```sh
echo 'Explain this project' | microgentic run
git diff | microgentic run --prompt 'Review this diff' > review.md
```

See [Shell use with `run`](shell.md).

## Caution

When you ask for work, the runtime can edit files and run shell commands in the
project directory. Shell commands are not sandboxed: they run as you, with
your environment. Use Microgentic in a Git repository so that you can examine
and undo its changes.
