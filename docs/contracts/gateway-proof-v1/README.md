# Gateway proof registry v1

`registry.json` maps the information gateway's 60 acceptance scenarios and 26
bypass paths to the evidence that proves each one. Every entry has one of
three statuses:

| Status | Meaning |
|---|---|
| `proven` | Engine tests named in `tests` prove it. `lib:` refers to `cargo test --lib`, `main:` to `cargo test --test main`. |
| `external` | Proven outside this repository, in the Enterprise Suite or the SDK. The evidence is named in `evidence`. |
| `open` | There is no executable proof yet. `note` says what is missing. |

`scripts/gateway-proof-gate.py` keeps the map honest. It checks that:

- every scenario and every path appears exactly once,
- every proven entry names tests, and those tests exist.

With `--run` it also runs exactly the named tests and requires them to pass.
CI runs this mode in the `gateway-proof` job, which "CI Green" depends on.

With `--release` it also refuses while any entry is open. A release that
claims the full gateway contract runs this mode.

State on 2026-10-03:

- 82 entries proven by 82 named tests, all green,
- 4 entries external,
- 0 entries open. `--release` passes.

Scenario 24, omitting irrelevant sources, needed a planner change. Plans
over caller-supplied sources now apply a relevance floor. A source stays
when it shares a topic term with the task, or with a source that does. Stop
words are dropped and identifiers are split. Everything else is excluded as
`lower_utility`.

Writing the attack tests for the bypass paths found one real bypass. Graph
file summaries stored the first source line raw, including a credential
declared on it. The summaries are now admitted, and the graph index version
moved to 7, so existing indexes are rebuilt.
