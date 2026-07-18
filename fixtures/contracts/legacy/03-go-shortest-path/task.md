# Go Weighted Shortest Path

Complete the directed weighted graph implementation in `graph.go`.

## Contract

- `AddEdge(from, to, weight)` adds a directed edge.
- `AddEdge` returns an error for a negative weight and must not add that edge.
- `ShortestPath(start, goal)` returns the minimum-total-weight path, its cost,
  and `true` when the goal is reachable.
- The returned path includes both `start` and `goal`.
- When `start == goal`, return `[start]`, cost `0`, and `true`, even for a node
  with no edges.
- For an unreachable goal, return `nil`, cost `0`, and `false`.
- If equal-cost paths exist, return the lexicographically smaller complete path.
- Do not mutate the graph while searching.
- Use only the Go standard library.

Only change `graph.go`. Do not remove or weaken tests.

## Validation

Run from the project root:

```sh
go test ./...
```

Reply `DONE` only after validation passes. Reply `FAIL` only if blocked.
