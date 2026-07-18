# Rust Binary Search Tree

Complete the integer binary search tree implementation in `src/lib.rs`.

## Contract

- `BinarySearchTree::new` creates an empty tree.
- `insert` adds a value and returns `true`; duplicate values are not added and
  return `false`.
- `contains` reports whether a value is present.
- `len` and `is_empty` reflect the number of distinct values.
- `in_order` returns all values in ascending order.
- `height` returns `0` for an empty tree and otherwise counts nodes along the
  longest root-to-leaf path.
- Use an actual linked binary tree. Do not replace the representation with a
  sorted vector, set, or map.

Only change `src/lib.rs`. Do not remove or weaken tests and do not add external
dependencies.

## Validation

Run both commands from the project root:

```sh
cargo fmt --check
cargo test
```

Reply `DONE` only after both commands pass. Reply `FAIL` only if blocked.
