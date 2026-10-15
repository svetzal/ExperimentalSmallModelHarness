# Tools

The model works on your project through tools. Each tool call shows as one line
in the conversation, and the trace records it.

## Project tools

The file tools read, list, write, and edit files below the project directory.
They do not reach paths outside it, and they skip ignored paths.

The shell tool runs commands in the project directory. It uses a non-login
shell with your `PATH` and active virtual environment.

The shell tool is not a sandbox. A command runs as you, with your environment
and network access, and can change files outside the project. Use Microgentic in
a Git repository so that you can examine and undo changes.

## Calculator

Small models make arithmetic mistakes, so the conversation and `run` give the
model a `calculate` tool. It evaluates arithmetic exactly. The model is told to
use it for any calculation with more than one simple step.

The tool takes one `expression` string. Separate expressions with newlines or
`;`. The result has one line for each expression, for example
`1474 / 1.1 = 1340`.

| Supported | Examples |
| --- | --- |
| Operators | `+ - * / // %`, and `^` or `**` for powers |
| Functions | `sqrt abs round floor ceil min max ln log10 log2 exp` |
| Constants | `pi`, `e` |
| Number forms | `1_000`, `1,474`, `2.5e3`, `×`, `÷`, a trailing `=` |

A line such as `total = 140 + 90` names a value that later lines in the same
call can use. Names are case-sensitive and last for one call only.

Decimals stay exact through `+ - * /` and integer powers, so `0.1 + 0.2` is
`0.3`. Roots, logarithms, fractional powers, and integers larger than 128 bits
use floating point and show 12 significant digits. Powers associate to the
right (`2^3^2` is 512). Unary minus binds less tightly than a power (`-2^2` is
-4). `%` and `//` round down, as in Python.

An error gives the column and a hint. The calculator cannot run code or read
files, and it accepts at most 4,000 characters.

Benchmark runs and the compatibility executable do not offer the calculator.
