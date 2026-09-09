# Plan: OpenCode session-history import

1. Add synthetic SQLite fixture tests that define the supported OpenCode schema contract and fail before implementation.
2. Add a focused OpenCode importer using read-only SQLite queries, current-project joins, bounded rows, tolerant JSON decoding, and project-relative path filtering.
3. Wire `opencode` into source names, CLI dispatch/help, and `--all`.
4. Run targeted tests, sabotage the implementation to prove the regression test bites, then run canonical formatting, clippy, test, and preflight gates.
5. Push one focused branch and open a PR closing #1731.
