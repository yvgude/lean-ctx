# Provider Framework Contract v1

**Status**: Stable  
**Version**: `PROVIDER_FRAMEWORK_V1_SCHEMA_VERSION = 1`  
**Runtime source**: `rust/src/core/providers/`

## Purpose

Provides structured access to external context sources through the MCP tool interface.
All provider data is a **first-class citizen**: it flows through the full consolidation pipeline
into BM25 index, Graph index, Knowledge facts, and Session cache.

## Architecture

```
ContextProvider::execute()
    → ProviderResult
    → consolidation::consolidate()
    → ConsolidationArtifacts { bm25_chunks, edges, facts, cache_entries }
    → apply_artifacts_to_stores() [background thread]
        → BM25Index::ingest()       — searchable via ctx_semantic_search
        → GraphIndex::merge_edges() — cross-source hints in ctx_read
        → Knowledge::remember()     — recallable via ctx_knowledge
        → SessionCache::set()       — fast re-reads
```

### Built-in Providers

| Provider | Auto-activates when | Resources |
|---|---|---|
| GitHub | `GITHUB_TOKEN` set | issues, pull_requests, actions |
| GitLab | `GITLAB_TOKEN` set | issues, merge_requests, pipelines |
| Jira | `JIRA_TOKEN` set | issues, sprints, projects |
| PostgreSQL | `DATABASE_URL` or `PGDATABASE` set | tables, schemas (catalog introspection only) |

GitLab list filters (`state`, issue `labels`, and pipeline `status`) are
percent-encoded as individual query values. Reserved characters, literal plus
signs, spaces and Unicode cannot add parameters or turn query data into a URL
fragment. Encoding does not provide complete pagination or authorization.
Each built-in GitLab acquisition contacts the source: issues and merge requests
no longer return a prior response from the provider request cache. Source errors
are returned without a cached-data fallback. This applies to legacy GitLab actions
and registry queries; discovery advertises a zero request-cache TTL.

PostgreSQL introspection accepts a table limit from 1 to 100 (default 50), with
one global cap of 20 × the table limit on catalog-column records (not a
per-table cap). Reaching that cap marks the result truncated and withholds a
purported complete total.
`psql` runs without startup scripts or password prompting, with a 10-second
process deadline and captured-output bounds of 1 MiB stdout/16 KiB stderr through
the shared process-tree capture implementation. Command failures never return
raw database diagnostics. Oversized, malformed or non-UTF-8 output fails closed.
The capture bound is not an OS disk quota; deliberately detached descendants
remain outside Unix process-group cleanup. Credentials/connection configuration
must still be operator-owned: moving a configured `DATABASE_URL` out of the
process argument list and governed business-row acquisition remain open work,
not capabilities claimed by this catalog-only adapter.

The optional pgvector backend reuses this same bounded `psql` execution path:
1 MiB stdout, 16 KiB stderr, strict UTF-8 and withheld database diagnostics.
Its existing `LEANCTX_PGVECTOR_TIMEOUT_SECS` setting now also bounds child
waiting (default 10 seconds), in addition to connection establishment; shared
process cleanup may add its bounded grace period. Startup scripts, password
prompts and inherited stdin are disabled. A failure or timeout does not assert
that earlier SQL was rolled back, and no automatic retry is added. Invalid
search-row diagnostics and configuration Debug output omit database values and
the connection string. Moving the configured connection string out of argv
remains open for both adapters; no credential-transport fix is claimed here.

### Config-based Providers

Custom REST APIs via TOML/JSON in `~/.config/lean-ctx/providers/` or `.lean-ctx/providers/`.
Supports 6 auth methods (bearer, API key, basic, header, query param, none).

Built-in HTTP providers using `HardenedClient`, Jira data and OAuth requests,
provider health probes, and config-based REST providers do not follow HTTP
redirects. Configure the final
endpoint directly: a 3xx response is an error, including redirects within the
same origin. Custom authentication headers and request bodies therefore stay
at the selected endpoint. Redirect locations and response bodies are withheld
from these errors. Config-based REST requests use the shared platform TLS roots,
connection timeouts, retaining their existing 10 MiB response-body limit and
lossy UTF-8 decoding. This does not govern the
separate MCP bridge transport or the lifecycle of source credentials.

### MCP Bridge Providers

External MCP servers connected via `[providers.mcp_bridges.<name>]` config.
Each bridge registers with unique ID `mcp:<name>`. Supports:
- HTTP transport (`url = "http://..."`)
- Stdio transport (`command = "npx"`, `args = ["-y", "@mcp/server"]`)
- Actions: `resources` (list), `read_resource` (fetch single), `tools` (list)

## ctx_provider Actions

| Action | Parameters | Description |
|---|---|---|
| `query` | provider, resource, mode | Registry query; `mode=snapshot` returns the bounded versioned JSON contract below |
| `gitlab_issues` | state, labels, limit | List issues (sorted by updated_at desc) |
| `gitlab_issue` | iid | Show single issue with description |
| `gitlab_mrs` | state, limit | List merge requests |
| `gitlab_pipelines` | status, limit | List pipelines |
| `mcp_resources` | — | List all resources from configured MCP bridges |

### `query` acquisition

Compact queries execute the provider once and use that same result for indexing
and display. They do not perform a second request or fall back to chunks from an
earlier request when a later request fails. Chunk and snapshot queries likewise
use one acquisition. This does not make previously indexed source data a fresh
authorization receipt.

### `query` snapshot mode

`action=query, mode=snapshot` is an additive external-consumer projection. It
executes the existing registry action once, consolidates the same returned
`ProviderResult` through the normal MCP output pipeline, and emits a canonical
JSON text result. The machine-readable snapshot suppresses human decorations,
but still passes sensitivity, policy, input-filter, and turn-budget guards; a
post-budget integrity check rejects any rewrite that would invalidate the
digest. The existing compact and chunk output formats remain available.

Successful snapshots have schema version `1` and this shape:

```json
{
  "schema_version": 1,
  "provider": "gitlab",
  "resource": "issues",
  "request": {"project": "group/project", "state": "open", "limit": 100, "query": null, "id": null},
  "result": {"provider": "gitlab", "resource_type": "issues", "items": [], "total_count": 0, "truncated": false},
  "snapshot_digest": "sha256:<64 lowercase hex characters>"
}
```

`snapshot_digest` is SHA-256 over canonical recursively key-sorted JSON of the
same envelope with `snapshot_digest` omitted. Every string field, including
request filters and nested claims, passes the mandatory provider secret
redactor before canonicalization. The projection is bounded to at most 100
items, 100 labels/claims per item, 4 KiB for ordinary string fields, and 32 KiB
for item bodies. Shortened strings/arrays set `result.truncated=true`; clients
requiring a complete snapshot must reject that result. A canonical envelope
over 1 MiB returns a structured error, never partial JSON. Requested limits
must be integers from 1 to 100; the default is 100.

Error responses retain `schema_version: 1` and contain only an `error.code` and
redacted bounded `error.message`; they do not carry a digest. The snapshot is
not an upstream revision, signature, authorization receipt, or proof that a
connector ran against a particular server state. `provider`, `resource`, and
request filters are caller-selected values; access remains governed by the
existing provider configuration and authorization path.

## Configuration

### Explicit GitLab selection in protected Codex sessions

The qualified macOS launcher accepts the three options together:
`--gitlab-host <HTTPS-AUTHORITY> --gitlab-project <NUMERIC-ID>
--gitlab-namespace <GROUP/PROJECT>`. Optional `--glab <PATH>` selects the
trusted credential reader; otherwise its executable is resolved once from PATH.
`--check` validates and prints the configuration without reading a credential
or contacting GitLab.

After Codex starts its MCP command, a LeanCTX wrapper reads the existing glab
global host token with a cleared environment. It uses a bounded memory-only
socket capture, disables glab telemetry/update checks and core dumps, then
passes a versioned frame over an anonymous socket directly to the sandboxed MCP.
The credential is not placed in argv, environment, generated configuration or
a temporary credential file; the descriptor does not pass through Codex.
The receiver acknowledges admission and closes the channel before starting tools. Bootstrap failures stop
startup. The selected glab store and known default stores are denied to native
Codex tools, MCP and its children; selected executable/control paths are protected
against modification.

The session pins the built-in GitLab registry entry; other operator-configured
providers retain their existing access rules. Reinitialization and a
same-ID configured provider cannot replace it; legacy actions use the same
session configuration. A supplied project must match the selected numeric ID
or namespace. Each acquisition checks the current project ID/namespace using
the selected credential before requesting the resource. Failures and redirects
return no source data. This acquisition boundary does not reauthorize previously
consolidated context, and is not a complete operating-system credential vault.
The ordinary environment-based configuration below remains compatible.

### Ordinary provider sessions

Token resolution order:
1. `LEAN_CTX_GITLAB_TOKEN`
2. `GITLAB_TOKEN`
3. `CI_JOB_TOKEN`

Host resolution:
1. `GITLAB_HOST`
2. `CI_SERVER_HOST`
3. Default: `gitlab.com`

Project path resolution:
1. `CI_PROJECT_PATH`
2. Auto-detect from `git remote get-url origin`

## ProviderResult Schema

```rust
struct ProviderResult {
    provider: String,       // "gitlab"
    resource_type: String,  // "issues", "merge_requests", "pipelines"
    items: Vec<ProviderItem>,
    total_count: Option<usize>,
    truncated: bool,
}

struct ProviderItem {
    id: String,
    title: String,
    state: Option<String>,
    author: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    url: Option<String>,
    labels: Vec<String>,
    body: Option<String>,
}
```

## Caching & Indexing

- **Provider request cache**: GitHub issue/PR keys bind the provider, exact effective API URL (including project and filters), and credential using a length-framed BLAKE3 digest. Keys contain no raw URL or credential. Built-in GitLab reads do not use this cache, including pre-existing fresh or stale entries.
- **Session cache**: Consolidated context can still be stored separately; removing the GitLab request-cache shortcut does not revoke previously indexed or exported copies.
- **BM25 index**: External chunks indexed with `ChunkKind` metadata (Issue, PullRequest, DbSchema, etc.)
- **Graph index**: Cross-source edges link external URIs to code files (e.g. issue → `src/auth.rs`)
- **Knowledge facts**: Extracted categories: `known_bugs`, `known_features`, `recent_changes`, `data_model`, `documentation`, `file_mentions`
- **`providers.auto_index`**: Controls background indexing (default: `true`)

## Security

- All provider outputs pass through `redact_text_if_enabled`
- CI job logs pass through secret scanner before delivery
- Tokens never appear in tool output
- MCP bridges: optional `auth_env` field for token injection from env vars
- Request-cache partitioning is not reauthorization. Other cache consumers retain their existing TTL and stale-serving behavior. Built-in GitLab acquisitions require a new successful source request; authorization and purge of previously consolidated BM25, graph, knowledge and session data remain separate requirements.
- GitLab configuration Debug output replaces the credential with a fixed redaction marker. Ordinary sessions retain environment-based discovery; the explicit protected launcher uses the handoff described above.

## Shell Compression (`glab` / `gh` CLI)

Patterns for CLI output:
- `glab`/`gh` issue list/view, MR/PR list/view, CI/actions status
- Compression follows pattern-based structure

## Context IR Integration

Provider outputs are tracked as `ContextIrSourceKindV1::Provider` in the evidence ledger, enabling:
- Provenance tracking (which provider data informed a decision)
- Replay verification
- Token attribution

## Diagnostics

`lean-ctx doctor` validates:
- Provider env vars (GITHUB_TOKEN, GITLAB_TOKEN, etc.)
- MCP bridge URLs (reachable, configured)
- `auto_index` status (warns if `false`)
