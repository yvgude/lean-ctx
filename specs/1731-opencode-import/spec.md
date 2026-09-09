# Spec: OpenCode session-history import (refs #1731)

## Problem / Why
`lean-ctx import` cannot ingest current OpenCode session history, even though OpenCode persists project-scoped sessions, messages, and typed parts in a local SQLite database.

## Goal
Import useful, bounded facts from the current project's OpenCode sessions without leaking host paths or unrelated project history.

## Acceptance Criteria (EARS)
- WHEN a user runs `lean-ctx import opencode`, THE CLI SHALL read the current platform's OpenCode database in read-only mode.
- WHEN `--all` is selected, THE CLI SHALL include OpenCode.
- WHEN an OpenCode database contains multiple projects, THE importer SHALL process only sessions whose project worktree matches the current project root.
- WHEN supported text, tool, or patch parts contain decisions, errors, or touched files, THE importer SHALL emit bounded project knowledge facts.
- WHEN a touched path is absolute, relative, or traverses outside the project, THE importer SHALL retain only normalized project-relative paths inside the current project.
- WHEN storage is absent or rows contain malformed/unknown JSON, THE importer SHALL not panic or disclose transcript content.

## Out of Scope
- Kilo Code: current source exposes incompatible legacy VS Code task storage and newer OpenCode-derived storage; a stable cross-version discovery contract is not established.
- Importing unrelated OpenCode projects.
- Mutating OpenCode storage.

## Verification
- `cargo test --lib core::import::opencode`
- `cargo test --lib cli::import_cmd`
- `scripts/preflight.sh fast`

## Links
- Tracking issue: #1731
- Plan: ./plan.md · Tasks: ./tasks.md
