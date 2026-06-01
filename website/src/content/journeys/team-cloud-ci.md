Beyond a single developer on a laptop, LeanCTX can share one context index across a whole team, sync your own stats and knowledge across your machines, contribute to adaptive models, and run headless in CI. This journey covers the server-side and account-level surfaces.

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

## 3. Contributing to adaptive models

```bash
lean-ctx contribute                # send anonymized compression data points
lean-ctx cloud pull-models         # pull refreshed adaptive compression models
lean-ctx upgrade                   # account/plan upgrade flow
```

- `contribute` uploads anonymized compression samples that improve the shared
  adaptive models (it tells you to "use LeanCTX for a while first" if there's
  nothing to send).
- `cloud pull-models` downloads refreshed models and prints an estimated
  compression improvement. Fully optional — local heuristics work without it.

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

## 5. Choosing the right sharing model

| You want… | Use |
|-----------|-----|
| Many devs sharing **one** index | `team serve` + scoped tokens (§1) |
| **Your** data on **your** machines | `login` + `sync` (§2) |
| Help improve compression for everyone | `contribute` (§3) |
| Headless install/verify in pipelines | `bootstrap`, `serve`, `ctx_proof` (§4) |
| Agents coordinating on one repo | Journey 8 (multi-agent) |

## Storage & config (team/cloud)

| Path | Contents |
|------|----------|
| `team.toml` (your path) | team server config + tokens |
| `~/.lean-ctx/cloud/credentials.json` | cloud login credentials |
| `~/.lean-ctx/cloud/` | synced-data staging |
