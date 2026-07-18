# JavaScript Topological Sort

Implement `topologicalSort(graph)` in `topological-sort.js`.

## Contract

- `graph` is an object whose keys are node names and whose values are arrays of
  outgoing neighbour names.
- Return every node exactly once in a valid topological order.
- Include nodes that appear only as neighbours.
- For deterministic output, choose the lexicographically smallest currently
  available zero-indegree node.
- Throw an `Error` whose message contains `cycle` when the graph is cyclic,
  including a self-cycle.
- Reject a non-object graph or non-array adjacency list with `TypeError`.
- Do not mutate the graph or its adjacency arrays.
- Use only the Node.js standard library.

Only change `topological-sort.js`. Do not remove or weaken tests.

## Validation

Run from the project root:

```sh
npm test
```

Reply `DONE` only after validation passes. Reply `FAIL` only if blocked.
