Hosting LeanCTX yourself gives an entire company one place — your own cloud — where every developer and CI agent draws on the same compressed, governed context. The audited **team server** is built for exactly this: it is Apache-2.0, self-hosting is a free capability, and this journey takes it from a single `team.json` to a hardened service on AWS behind your own TLS and SSO.

## 1. What you actually deploy

`lean-ctx team serve` is one process that fronts many workspaces (repos) and many users behind role-scoped bearer tokens. Everyone points their MCP client at its URL and queries **one** shared BM25 / graph / artifact index instead of each clone rebuilding its own.

```bash
lean-ctx team serve --config team.json
```

> Read-first by design: the team surface authorizes `search`, `graph`, `knowledge`, `events` and `artifacts` — but **denies** code-mutating/exec tools (`ctx_shell`, `ctx_execute`, `ctx_edit`). A shared org server hands out context, not remote shells.

## 2. Describe the org in one config

`team.json` lists the workspaces (repos on the host) and the tokens — stored as SHA-256 hashes, never plaintext:

```json
{
  "host": "0.0.0.0",
  "port": 8080,
  "defaultWorkspaceId": "core",
  "workspaces": [
    { "id": "core", "label": "Core API", "root": "/srv/repos/core-api" },
    { "id": "web",  "label": "Web App",  "root": "/srv/repos/web" }
  ],
  "tokens": [
    { "id": "alice",  "sha256Hex": "<sha256(token)>", "role": "member" },
    { "id": "ci-bot", "sha256Hex": "<sha256(token)>", "scopes": ["search", "graph"] }
  ],
  "auditLogPath": "/var/lib/lean-ctx/audit.jsonl"
}
```

Mint tokens with the CLI — it prints the secret once and stores only its hash:

```bash
lean-ctx team token create --config team.json --id alice --role member
```

Roles expand to scopes — `viewer · member · admin · owner`. A `viewer` may search but is denied mutations and audit (`403 scope_denied`); an `admin` holds the full scope set — and every decision is written to the audit log.

## 3. Run it as a container

The whole runtime is one ~15 MB binary, so the image stays tiny:

```dockerfile
FROM debian:stable-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git curl \
 && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://leanctx.com/install.sh | sh
ENV PATH="/root/.local/bin:${PATH}"   # adjust to your install location
COPY team.json /etc/lean-ctx/team.json
EXPOSE 8080
CMD ["lean-ctx", "team", "serve", "--config", "/etc/lean-ctx/team.json"]
```

> Bind safety is enforced in code: the server **refuses to bind a non-loopback host without a token** (and the team server requires at least one). There is no accidental open endpoint.

## 4. AWS topology

A single, well-provisioned node behind a load balancer is the sweet spot:

| Layer | AWS service | Why |
|-------|-------------|-----|
| TLS + identity | **ALB** (ACM cert) + `authenticate-oidc` | Server speaks plain HTTP — terminate TLS and add SSO at the edge |
| Compute | **ECS Fargate** task (or EC2) | One container; scale the task size, not the count |
| Persistent state | **EFS / EBS** at `/srv/repos` + `/var/lib/lean-ctx` | Indexes, caches and the audit log survive restarts |
| Fresh index | **EventBridge** schedule → `team sync` | `git fetch`es each workspace so the index tracks `main` |
| Secrets | **Secrets Manager / SSM** for `team.json` | Never bake token hashes into the image |

```bash
# Scheduled task (EventBridge / cron) — keep the shared index current
lean-ctx team sync --config /etc/lean-ctx/team.json
```

## 5. Identity & SSO

The team server authenticates **bearer tokens**, not users — tokens are provisioned out-of-band, least-privilege by design. For company SSO, put identity at the edge:

- **ALB `authenticate-oidc`** with Cognito, Okta or Entra ID in front of the listener, or
- an **oauth2-proxy** sidecar.

Map each person and CI job to a role-scoped token; rotate by reissuing and removing the old `id`.

## 6. Production hardening checklist

Every item below is a real, in-code control — set it, don't assume it:

- **TLS** at the ALB; the origin stays on a private subnet.
- **Auth required** — tokens are timing-safe compared; a non-loopback bind without a token is refused.
- **Caps** — `maxRps`, `rateBurst`, `maxConcurrency`, `maxBodyBytes`, `requestTimeoutMs` are config; tune per fleet.
- **DNS-rebinding guard** — `allowedHosts` is enforced; never set `disableHostCheck` in production.
- **Least privilege** — give CI `search,graph`; reserve `knowledge`, `sessionmutations` and `audit` for trusted roles.
- **Audit** — ship `auditLogPath` (JSONL: who · tool · workspace · allowed/denied) to CloudWatch.

## 7. What stays free — and the boundary

Self-hosting the team server is **Apache-2.0 and free**: the Local-Free Invariant, enforced as a CI gate, guarantees no local capability is ever placed behind an account, license or plan. The open repo carries the engine, CLI, SDKs, the self-host team server, plugins and the plan *catalog*. A managed offering — multi-tenant customer data, billing *enforcement*, SSO/SCIM — lives in a separate **private plane** that talks to the open engine over the same `/v1` boundary. Hosting it yourself needs none of that.

## 8. Know the limits

- **Single process, filesystem state.** The context bus, session cache and indexes are in-process — scale **vertically** (bigger task + a persistent volume), not by adding nodes. There is no turnkey multi-node HA fan-out; for very large orgs, run one strong instance or shard by team.
- **Repos live on the host.** Workspaces point at real directories — clone them onto the volume and refresh with `team sync`.
- **TLS is yours.** The server is HTTP behind your load balancer by design.
