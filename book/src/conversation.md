# The conversation

Run `microgentic` with no command to open the conversation view. It needs a
terminal for both input and output. In a pipe or with redirection, use
[`run`](shell.md) instead.

Each conversation starts with no assumed task. Greetings and questions get
ordinary replies. When you ask for work, the model uses tools to inspect, edit,
and check the project. Follow-up messages keep the earlier messages, replies,
and tool results.

## Keys

| Key | Action |
| --- | --- |
| Enter | Send the message |
| Alt-Enter | Insert a newline |
| Page Up, Page Down | Scroll |
| Tab | Complete a slash command |
| Ctrl-C | Cancel the current reply |
| Esc | Exit, when no reply is running |

You can type your next message while a reply streams. Enter sends it when the
current reply is complete.

## Slash commands

Commands that start with `/` run in the view. They are never sent to the model.

| Command | What it does |
| --- | --- |
| `/model` | Show the models the provider has, and pick one |
| `/model <id>` | Switch to that model |
| `/effort` | Pick a reasoning effort |
| `/effort <level>` | Set the effort: `disabled`, `enabled`, `low`, `medium`, `high`, `xhigh`, or `default` |
| `/guidance` | List the project guidance files, with their size and status |
| `/clear` | Start a new conversation |
| `/help` | List the commands |

In a picker, Up and Down move, Home and End go to the first and last option,
Enter chooses, and Esc closes the picker with no change.

`/model` and `/effort` change the setting for this session only. To change your
saved defaults, use `microgentic setup`. A conversation keeps the model and
effort it started with, so after a change your next message starts a new
conversation.

To send a message that starts with `/`, type `//`.

## Status bar

The status bar is below the input:

```text
🤖 qwen3.8:27b-mxfp8  🧠 low  📁 microgentic  🌿 main ●3 …2 ↑1  📜 2  ctx:[███·······]31%
```

| Part | Meaning |
| --- | --- |
| 🤖 | The model |
| 🧠 | The reasoning effort (`default` when unset) |
| 📁 | The project folder |
| 🌿 | The Git branch, changed files (`●`), untracked files (`…`), and commits ahead (`↑`) or behind (`↓`) |
| 📜 | The number of project guidance files in context |
| `ctx:` | The estimated size of the latest request, as a part of the context window |

The context gauge is green below 60%, yellow to 85%, and red above. When the
window size is unknown, the gauge shows only the token estimate, for example
`ctx: 12.4k`. On a narrow terminal, the Git, folder, and effort parts are
removed first.

## Tool activity

Each tool call is one line below the reply that asked for it. A spinner shows
while the call runs. Then `✓` shows success and `✗` shows an error. The line
names the tool and its main argument, such as the file and line range for a
read, or the command for a shell call.

The model's thinking streams in dim italic text. When thinking leads to tool
calls, it shrinks to one `∴` line above those calls. Thinking before a reply is
removed when the reply arrives.

## Evidence

When an exchange uses project tools, the reply ends with an `Evidence:` line.
It gives the location of the trace for that exchange. Ordinary replies have no
evidence line, but every exchange still saves a trace. See
[Traces and transcripts](traces.md).

## Colour and animation

The view uses your terminal's named colours, so it follows a light or dark
theme. Set `NO_COLOR` to any value to turn colour off. Text labels keep their
meaning without colour.

A short animation plays when the view opens. Any key skips it, and the key still
goes to the input. Set `MICROGENTIC_NO_ANIMATION=1` to turn it off.

## Conversation history

Each `microgentic` invocation starts a new conversation. Microgentic saves the
conversation outside the project, in the run-state directory (see
[Files and locations](locations.md)). When the history becomes larger than half
of the input allowance, the oldest complete exchanges are left out, and the
view tells you. Resuming an earlier conversation is not available yet.
