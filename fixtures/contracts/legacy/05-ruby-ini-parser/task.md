# Ruby INI Parser

Implement `parse_ini(text)` in `ini_parser.rb`.

## Contract

- Return a nested hash mapping section names to key/value hashes.
- Store keys before the first section under the section name `default`.
- Ignore blank lines and full-line comments beginning with `#` or `;` after
  optional leading whitespace.
- Parse section headers such as `[database]`, trimming surrounding whitespace
  inside the brackets.
- Parse `key = value` pairs, trimming whitespace around the key and value.
- Values remain strings; no numeric or boolean coercion is required.
- A repeated key in the same section uses the last value.
- Raise `IniParseError` for an empty section name, an empty key, malformed
  section header, or any non-comment line without `=`.
- Error messages must include the one-based source line number.
- Use only the Ruby standard library.

Only change `ini_parser.rb`. Do not remove or weaken tests.

## Validation

Run from the project root:

```sh
make test
```

Reply `DONE` only after validation passes. Reply `FAIL` only if blocked.
