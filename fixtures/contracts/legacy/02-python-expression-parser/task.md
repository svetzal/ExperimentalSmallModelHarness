# Python Arithmetic Expression Parser

Implement `evaluate(expression)` in `expression_parser.py` as a small recursive
descent parser.

## Grammar and behaviour

- Accept integers, whitespace, parentheses, and `+`, `-`, `*`, `/`.
- Multiplication and division bind more tightly than addition and subtraction.
- Operators of equal precedence are left-associative.
- Unary `+` and unary `-` are supported, including before parentheses.
- Division uses normal Python floating-point division.
- Return an `int` when the result is mathematically integral; otherwise return a
  `float`.
- Raise `ValueError` for empty input, invalid characters, missing operands,
  mismatched parentheses, or trailing tokens.
- Let `ZeroDivisionError` propagate for division by zero.
- Do not use `eval`, `exec`, `ast`, or third-party parsing libraries.

Only change `expression_parser.py`. Do not remove or weaken tests.

## Validation

Run from the project root:

```sh
make test
```

Reply `DONE` only after validation passes. Reply `FAIL` only if blocked.
