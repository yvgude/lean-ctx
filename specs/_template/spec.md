# Spec: <Feature Name>  (refs #<iid>)

> SDD spec-anchored: this file is the source of truth for **intent**.
> Code + tests enforce it. When requirements change, update the spec first.

## Problem / Why
<1–2 short paragraphs: the user/developer problem and why it matters now.>

## Goal
<One sentence: the outcome when this is done.>

## Acceptance Criteria (EARS)
> Easy Approach to Requirements Syntax. One testable line each.
- WHEN <event/condition>, THE <component> SHALL <observable behavior>.
- WHILE <state>, THE <component> SHALL <behavior>.
- THE <component> SHALL <invariant>.

## Out of Scope
> Bound the agent's exploration explicitly.
- <what this deliberately does NOT do>

## Verification (deterministic first)
> How "done" is proven.
- `cargo test -q <name>`
- `scripts/preflight.sh fast`

## Links
- Tracking issue: #<iid>
- Plan: ./plan.md · Tasks: ./tasks.md
- Contracts touched (if any): `docs/contracts/<...>-v1.md`
