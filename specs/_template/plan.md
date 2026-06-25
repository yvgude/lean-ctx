# Plan: <Feature Name>  (refs #<iid>)

> Implementation plan for `./spec.md`. **Review before tasks.** Draft in your
> agent's plan mode (e.g. Cursor Plan Mode), then distill the approved approach here.

**Goal:** <from spec>
**Architecture:** <key approach in 2–4 sentences>
**Tech Stack:** <languages / tools / crates>

## Global Constraints
- <invariants: files NOT to touch, security (PathJail/allowlist), determinism>
- No mock data, no placeholders, no stubs.
- Output determinism: no timestamps/counters in tool output bodies (#498).

## File Structure
| File | Responsibility | New/Modify |
|------|----------------|------------|
| `path/to/file` | … | new / modify |

## Impact (run impact analysis first)
> `ctx_impact` (MCP) or `lean-ctx graph impact <file>` (CLI).
- Affected tests: <list — this is the verification set>
- Affected modules: <…>

## Self-Review (fill before implementing)
- Spec coverage: each EARS criterion ↔ at least one task.
- Placeholder scan: no TODO / TBD / mock / fallback.
- Determinism: stable output for same input.
- Cleanup: temporary/scratch files removed.
