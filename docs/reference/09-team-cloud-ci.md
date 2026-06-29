# Journey 9 — Team, Cloud & CI

> Beyond a single developer on a laptop: sharing a context index across a team,
> syncing your own stats/knowledge across machines, contributing to adaptive
> models, and running lean-ctx headless in CI. This journey covers the
> server-side and account-level surfaces.

Source files referenced here:
- `rust/src/cli/dispatch/network.rs` — `team serve` / `team token` / `team sync`
- `rust/src/cli/cloud.rs` — `login` / `register` / `sync` / `contribute` / `cloud` / `upgrade`
- `rust/src/cli/dispatch/mod.rs` — `serve`, `daemon`, `bootstrap`

---

## 1. Team server — one shared index for many developers

`lean-ctx team serve` runs a shared context server backed by a config file, so a
whole team queries one BM25/graph/artifact index instead of each clone building
its own.

```bash
lean-ctx team serve --config team.toml
```

### Scoped access tokens

Access is gated by tokens with explicit scopes — least-privilege by design:

```bash
lean-ctx team token create --config team.toml --id ci-bot --scopes search,graph
```

Valid scopes: `search`, `graph`, `artifacts`, `index`, `events`,
`sessionmutations`, `knowledge`, `audit`.

| Scope | Grants |
|-------|--------|
| `search` | BM25 / semantic queries |
| `graph` | dependency/impact graph reads |
| `artifacts` | packed context artifacts |
| `index` | trigger/read index builds |
| `events` | event stream subscription |
| `sessionmutations` | write session state |
| `knowledge` | read/write project knowledge |
| `audit` | read the audit trail |

Give a read-only CI bot `search,graph`; give a trusted writer `knowledge` too.

### Keeping the shared index fresh

```bash
lean-ctx team sync --config team.toml [--workspace <id>]
```

This `git fetch`es the configured workspaces so the server's index tracks the
latest commits. Run it on a timer (cron / CI schedule) on the server host.

### Managed connectors — continuous source sync

`team sync` keeps *code* fresh; **managed connectors** keep *external context*
fresh. A connector is a scheduled, in-process sync from GitLab or GitHub into a
workspace's BM25 + graph + knowledge stores. Once it has run, every seat's
`ctx_semantic_search` and `ctx_knowledge` surface that source's issues, merge
requests / PRs and pipelines — with **no per-call credentials** and no manual
`ctx_provider` calls.

Connectors are declared in the team config (`connectors[]`) — typically managed
for you from the hosted **Account → Team → Knowledge connectors** UI rather than
hand-edited:

```jsonc
"connectors": [
  {
    "id": "core-issues",
    "provider": "github",          // or "gitlab"
    "resource": "issues",          // gitlab: issues|merge_requests|pipelines
    "project": "acme/widgets",     // owner/repo (GitHub) or group/project (GitLab)
    "intervalSecs": 3600,           // clamped to a 5-minute floor
    "secret": "<provider token>",  // plaintext only inside the private team.json
    "enabled": true
  }
]
```

Behaviour worth knowing:

- **Cadence floor.** `intervalSecs` is clamped to 300 s so a misconfigured
  connector can't hammer an external API.
- **Quota backstop.** If the hosted index is over its storage quota, ingestion
  pauses — it never deletes data and never gates reads.
- **Secret hygiene.** The credential lives only in the injected `team.json`; it is
  never written to disk by the server and never returned by an API.
- **Status.** `GET /v1/connectors` (audit scope) returns a secret-free roster
  with each connector's last run, status and item count.

See the [Team Server Contract](../contracts/team-server-contract-v2.md#managed-connectors-281)
for the full `ConnectorConfig` schema.

---

## 2. Cloud account — sync your own data across machines

LeanCTX Cloud is an **optional, account-based** sync for a single user's data
across their own machines. It is not required for any local feature.

```bash
lean-ctx register <email>          # create an account (verification email sent)
lean-ctx login <email>             # credentials → ~/.lean-ctx/cloud/credentials.json
lean-ctx forgot-password <email>   # reset link
```

**Golden output — the default, signed-out state.** Cloud is opt-in, so a fresh
install reports exactly that and points you at the first step:

```text
Not connected to LeanCTX Cloud.
Get started: lean-ctx login <email>
```

```bash
lean-ctx sync                      # push your local data to the cloud
```

`sync` covers: stats, command history, CEP scores, knowledge, gotchas, buddy
state, and feedback thresholds. Each section is skipped cleanly if there's
nothing to send ("No … to sync yet").

> Privacy: emails are masked in output; only your own account data is synced.
> This is distinct from §3 (contribute), which is anonymized and aggregate.

---

## 3. Contributing to adaptive models

```bash
lean-ctx contribute                # send anonymized compression data points
lean-ctx cloud pull-models         # pull refreshed adaptive compression models
lean-ctx upgrade                   # account/plan upgrade flow
```

- `contribute` uploads anonymized compression samples that improve the shared
  adaptive models (it tells you to "use lean-ctx for a while first" if there's
  nothing to send).
- `cloud pull-models` downloads refreshed models and prints an estimated
  compression improvement. Fully optional — local heuristics work without it.

---

## 4. Headless / CI usage

For pipelines you want zero prompts and deterministic exit codes.

### One-shot, non-interactive setup

```bash
lean-ctx bootstrap [--json]        # = setup --non-interactive --yes --fix
lean-ctx setup --non-interactive --yes --json
```

Both exit non-zero on failure, so a CI step fails loudly. `--json` emits a
machine-readable report.

### Running the MCP server / daemon in CI

```bash
lean-ctx serve                     # MCP server (stdio) — for agent runners
lean-ctx daemon                    # background daemon (index/event services)
```

### Verifiable context in CI gates

Pair this journey with Journey 7's verification tools:

```text
ctx_proof  …   # cryptographic proof a context was produced as claimed
ctx_verify …   # validate an artifact/ledger
```

Use these as a CI gate ("the context bundle this PR relies on is reproducible").

### Provider tokens in CI

Provider integrations (GitHub/GitLab/Jira/Postgres — Journey 5) read credentials
from environment variables, never from prompts, which is exactly what CI needs.
Store them as CI secrets and the providers run headless.

---

## 5. Choosing the right sharing model

| You want… | Use |
|-----------|-----|
| Many devs sharing **one** index | `team serve` + scoped tokens (§1) |
| **Your** data on **your** machines | `login` + `sync` (§2) |
| Help improve compression for everyone | `contribute` (§3) |
| Headless install/verify in pipelines | `bootstrap`, `serve`, `ctx_proof` (§4) |
| Agents coordinating on one repo | Journey 8 (multi-agent) |

---

## Storage & config (team/cloud)

| Path | Contents |
|------|----------|
| `team.toml` (your path) | team server config + tokens |
| `~/.lean-ctx/cloud/credentials.json` | cloud login credentials |
| `~/.lean-ctx/cloud/` | synced-data staging |

---

## UX notes captured during this walkthrough

- The three "share" concepts (team index / personal cloud sync / anonymized
  contribute) are easy to conflate; §5 gives a one-look decision table.
- Token scopes are the right security primitive but undocumented in `help`;
  enumerated here with concrete recommendations.
- CI users should reach for `bootstrap` (not interactive `setup`) — called out
  explicitly so pipelines don't hang on a prompt.
