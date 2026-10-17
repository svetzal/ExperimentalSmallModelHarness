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
| Page Up, Page Down | Scroll the conversation |
| Tab | Complete a slash command |
| Ctrl-O | Show or hide the [context panel](#context-panel) |
| Alt-Page Up, Alt-Page Down | Scroll the context panel (Shift works too) |
| Ctrl-C | Cancel the current reply |
| Esc | Exit, when no reply is running |

You can type your next message while a reply streams. Enter sends it when the
current reply is complete.

The conversation also ends when its terminal closes.

## Slash commands

Commands that start with `/` run in the view. They are never sent to the model.

| Command | What it does |
| --- | --- |
| `/model` | Show the models the provider has, and pick one |
| `/model <id>` | Switch to that model |
| `/effort` | Pick a reasoning effort |
| `/effort <level>` | Set the effort: `disabled`, `enabled`, `low`, `medium`, `high`, `xhigh`, or `default` |
| `/guidance` | List the project guidance files, with their size and status |
| `/context` | Show or hide the [context panel](#context-panel), like Ctrl-O |
| `/clear` | Start a new conversation |
| `/help` | List the commands |

In a picker, Up and Down move, Home and End go to the first and last option,
Enter chooses, and Esc closes the picker with no change.

`/model` and `/effort` change the setting for this session only. To change your
saved defaults, use `microgentic setup`. A conversation keeps the model and
effort it started with, so after a change your next message starts a new
conversation.

To send a message that starts with `/`, type `//`.

While a reply runs, only `/help` and `/context` work. Other commands wait for
the reply to finish, so a reply never changes settings while it runs.

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
| 📜 | Standing items in context: the project guidance files in the latest request. Before the first request, the files Microgentic found |
| `ctx:` | The estimated size of the latest request, as a part of the context window |

The context gauge is green below 60%, yellow to 85%, and red above. Microgentic
asks the provider for the model's context window once per conversation. When
the window is still unknown, the gauge shows only the token estimate, for
example `ctx: 12.4k`. On a narrow terminal, the `📜` count goes first, then
the Git, folder, and effort parts.

## Tool activity

Each tool call is one line below the reply that asked for it. A spinner shows
while the call runs. Then `✓` shows success and `✗` shows an error. The line
names the tool and its main argument, such as the file and line range for a
read, or the command for a shell call.

The model's thinking streams in dim italic text. When thinking leads to tool
calls, it shrinks to one `∴` line above those calls. Thinking before a reply is
removed when the reply arrives.

## Context panel

The model has no memory between requests. Each request sends it everything it
can use: the system prompt, the conversation so far, and the tool
descriptions. The context panel shows that request, so you can see what the
model had when it answered.

Press Ctrl-O, or type `/context`, to open the panel to the right of the
conversation. Press it again to close it. The panel uses about 40% of the
width, and the conversation wraps in the rest.

```text
Context of the latest request
Exchange 2 · request 1 · qwen3.8:27b-mxfp8
4 messages · ~1.6k of 262.1k tokens
Earlier reasoning: not retained. Thinking from earlier
requests is not sent; only replies and tool calls are.
Older exchanges omitted: none
──────────────────────────────────────────────────────
1. system · system prompt · ~502 tok · new
  base system prompt · pinned · since turn 1
  You are Microgentic, a helpful conversational assistant
  … 1,703 more chars
2. user · retained user prompt · ~4 tok · kept
  retained conversation user prompt · since turn 1
  Add a slower pace to the launch animation.
3. assistant · assistant reply · ~40 tok · kept
  retained assistant text · since turn 1
  Here is the change: const PACE: f64 = 0.88;
4. user · current user prompt · ~4 tok · new
  current user prompt · pinned · since turn 1
  Write the file.
Tools offered: 6 · ~1.1k tok · pinned
  list_tree ~104, read_file ~85, write_file ~105, ...
```

The header tells you:

- which exchange and which request in it the panel shows. One message can need
  several requests when the model uses tools.
- the model, the number of messages, and the estimated size against the context
  window. The window comes from `--context-window-tokens`, the provider, or
  Microgentic's model table, in that order. It shows as unknown when none of
  them gives it.
- whether the model's earlier thinking was sent. It is not sent by default. If
  the model worked something out only in its thinking, the next request does
  not have it.
- how many older exchanges were left out to keep the history small.

Below the header, each message in the request has a heading with its number,
its role, its kind, and an estimated token count, then its first few lines.
`… N more chars` tells you how much of the text is not shown. Tool requests
name each call, and tool results name the call they answer. The kinds are:

| Kind | What it is |
| --- | --- |
| system prompt | Microgentic's standing instructions |
| project guidance | Your `AGENTS.md` files, part of the system message |
| nested project guidance | A deeper `AGENTS.md`, sent when work reached its folder |
| retained user prompt | One of your earlier messages |
| current user prompt | The message the model is answering |
| session notice | A note Microgentic added, such as "the previous request stopped" |
| assistant reply | A reply the model gave earlier |
| tool request | Tool calls the model made |
| tool result | What a tool returned, word for word |
| summarized tool result, not verbatim | An older, large tool result that Microgentic shortened to a summary |
| consolidated file observation, not verbatim | Earlier reads of one file, merged into one message |
| consolidated reads, written by the harness | The request that goes with merged file reads. The model did not write it |
| reasoning checkpoint, not verbatim | The end of the model's earlier thinking, sent again after a long think |
| harness instruction (repair or loop) | An instruction Microgentic added during the exchange |

Below each heading, a muted line says why the message is in the request and
its residency state: `pinned` for parts that always stay (the system prompt,
project guidance, your current message, and the tools), `since turn N` for the
turn of the exchange that added it, `last referenced turn N` for nested
guidance the model has used since, and `demoted` for a result Microgentic
shortened or merged. The trace records the same residency for every request.

Each heading ends with `new` or `kept`. `new` means the previous request in
this exchange did not have that message. On the first request of an exchange,
the system prompt and your new message are `new`, and the earlier conversation
is `kept`. Labels carry the meaning, so the panel reads the same with
`NO_COLOR`.

The panel updates each time Microgentic sends a request, including the requests
in a tool loop, and it keeps the last one after the reply. It shows the newest
rows first. Use Alt-Page Up and Alt-Page Down (or Shift with Page Up and Page
Down) to scroll it; a new request returns it to the newest rows. Before your
first message, the panel says that there is no request yet. `/clear`, `/model`,
and `/effort` start a new conversation, so they empty the panel.

The panel needs a terminal of at least 80 columns and 12 rows. In a smaller
terminal the conversation keeps the full width, and the key hint line says that
the panel needs more space.

The panel only shows what was sent. It does not change the request.
Microgentic builds it from the exact request it records in the trace, with
credentials removed and long text shortened.

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
