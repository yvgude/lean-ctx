You wired your agent to filesystem, GitHub, Linear, Postgres and two internal MCP servers — and now every connected server injects its *entire* catalog into the system prompt, on every request, whether or not the task needs it. The agent has gotten slower, pricier, and worse at picking the right tool: the well-documented "more tools → less adoption" curve. LeanCTX used to shrink only its *own* surface; this journey is about the *external* one.

---

## 1. Point LeanCTX at your servers — once, globally

This is privileged: it can spawn processes and open connections, so it is **never**
read from a project-local config.

```toml
# ~/.lean-ctx/config.toml
[gateway]
enabled = true
top_n = 5
cache_ttl_secs = 300

[[gateway.servers]]
name = "linear"
transport = "http"
url = "https://mcp.linear.app/mcp"
headers = { Authorization = "Bearer ${LINEAR_TOKEN}" }

[[gateway.servers]]
name = "fs"
transport = "stdio"          # spawned as a child process
command = "mcp-server-filesystem"
args = ["/srv/project"]
```

## 2. The agent sees one tool, not the whole union

Instead of dozens of schemas, the agent sees a single `ctx_tools` meta-tool and
describes the task in natural language:

```jsonc
ctx_tools {"action":"find","query":"open an issue with a title and assignee"}
```

## 3. A ranked shortlist — plus a count of what was shielded

```text
gateway: 3 tool(s) for "open an issue" (catalog: 47 tool(s) across 5 server(s))

1. linear::create_issue — Create a Linear issue   params: title*, assignee, team
2. github::create_issue — Open a GitHub issue      params: repo*, title*, body
3. linear::update_issue — Update an existing issue  params: id*, state
```

## 4. Proxy the chosen call

The agent invokes the chosen handle; LeanCTX **proxies** the call to the owning
server and streams back the result:

```jsonc
ctx_tools {"action":"call","tool":"linear::create_issue",
           "arguments":{"title":"Fix login","assignee":"maya"}}
```

## 5. Under the hood — `rust/src/core/gateway/`

- **`client.rs`** — a real MCP client on the official `rmcp` SDK. `stdio` spawns the
  server; `http` uses streamable-HTTP with custom headers. Every connect/list/call
  is bounded by `call_timeout_secs`; sessions open per-operation and shut down
  cleanly (no orphaned child processes).
- **`catalog.rs`** — aggregates each server's tools into a namespaced `server::tool`
  catalog behind a **TTL cache**. Per-server errors are surfaced, never hidden.
- **`router.rs`** — builds an **ephemeral BM25 index** over the catalog per query (the
  same engine as `ctx_search`) and returns the top-N, deterministically.
- **`ctx_tools.rs`** — gates on config, routes the action, and proxies the call;
  downstream results flow back through the *same* firewall and sensitivity floor as
  native tools.

## Payoff

Connect *as many* MCP servers as you like. The model's per-request tool surface
stays flat at one meta-tool, tool-selection accuracy recovers, and the catalog
refreshes itself on a TTL.
