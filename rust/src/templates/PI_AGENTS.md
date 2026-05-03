# lean-ctx — Token Optimization for Pi

lean-ctx is installed as a Pi Package with first-class MCP support. All bash, read, grep, find, and ls calls are automatically routed through lean-ctx for 60-90% token savings. Additionally, 49 MCP tools are available for advanced operations.

## How it works

- **bash** commands are compressed via lean-ctx's 90+ shell patterns
- **read** uses 10 read modes (auto/full/map/signatures/diff/task/reference/aggressive/entropy/lines) based on file type + task
- **grep** results are grouped and compressed
- **find** and **ls** output is compressed and .gitignore-aware

## MCP tools available

In addition to the built-in tool overrides, lean-ctx provides these MCP tools:

- **ctx_session** — Session state management
- **ctx_knowledge** — Project knowledge graph (cross-session memory)
- **ctx_semantic_search** — Find code by meaning
- **ctx_overview** — Codebase overview
- **ctx_compress** — Manual compression control
- **ctx_metrics** — Token savings dashboard
- **ctx_cost** — Per-tool cost + savings breakdown
- **ctx_heatmap** — File-level savings heatmap
- **ctx_agent** — Multi-agent coordination
- **ctx_workflow** — State machine + evidence + tool gating
- **ctx_graph** — Dependency analysis
- **ctx_discover** — Code discovery
- **ctx_context** — Context management
- **ctx_preload** — Predictive preloading
- **ctx_delta** — Changed-lines-only reads
- **ctx_edit** — Read-modify-write in one call

## No manual prefixing needed

The Pi extension handles routing automatically. Just use tools normally:

```bash
git status          # automatically compressed
cargo test          # automatically compressed
kubectl get pods    # automatically compressed
```

## Checking status

Use `/lean-ctx` in Pi to verify which binary is active and see MCP bridge status.

## Dashboard

Run `lean-ctx dashboard` in a separate terminal to see real-time token savings.
