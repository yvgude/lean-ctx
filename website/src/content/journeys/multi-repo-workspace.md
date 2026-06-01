You have a parent folder (e.g. `~/code/` or `~/projects/`) that contains multiple independent git repos, and you want LeanCTX to index and search across all of them. This journey explains the three approaches and when to use each.

## Quick path (auto-detection)

If your parent folder has **2 or more** child directories with project markers (`.git`, `Cargo.toml`, `package.json`, etc.), LeanCTX auto-detects it as a multi-repo workspace:

```bash
cd ~/code
lean-ctx status
# → project_root: /Users/you/code (multi-repo, 5 children detected)
```

No configuration needed. LeanCTX will:
1. Set the parent as the project root
2. Add all child repos to the PathJail allow-list
3. Build a graph index spanning all repos

## Approach 1: Parent as root (recommended for <10 repos)

Open your IDE with the parent folder as workspace. LeanCTX auto-detects:

```
~/code/
├── api-service/        (.git)
├── frontend/           (.git)
├── shared-libs/        (.git)
└── docs-site/          (.git)
```

The graph index covers all files under `~/code/`. Tools like `ctx_search`, `ctx_graph`, and `ctx_overview` work across all repos.

### When this works best

- Less than ~10 repos in the parent
- Repos are related (same product, same team)
- You frequently search/navigate across repos

## Approach 2: `extra_roots` (recommended for independent repos)

If your repos are in different locations, or you want per-repo indexes:

```toml
# ~/.lean-ctx/config.toml
extra_roots = [
    "/Users/you/work/api",
    "/Users/you/work/frontend",
    "/Users/you/oss/cool-lib",
]
```

Or per-project:
```toml
# ~/code/.lean-ctx.toml
extra_roots = [
    "../shared-infra",
    "../common-types",
]
```

Each extra root gets its own graph + BM25 index (up to 8 extra roots). Tools like `ctx_search` and `ctx_tree` automatically span all roots.

### When this works best

- Repos are in different directory trees
- You want fine-grained control over which repos are indexed
- CI/Docker setups where workspaces are mounted at different paths

## Approach 3: Per-repo sessions (default, no config)

If you work in one repo at a time, just `cd` into it. LeanCTX creates a separate session and index per repo automatically. Each repo gets its own knowledge, graph, and session history.

### When this works best

- Repos are fully independent (different projects/teams)
- You rarely need cross-repo search
- You want isolated knowledge per project

## Configuration reference

| Config key | Effect |
|---|---|
| `extra_roots` | Additional directories to index and include in search |
| `graph_index_max_files` | Max files per scan (0 = unlimited, default) |
| `extra_ignore_patterns` | Glob patterns to exclude from all indexes |

## Troubleshooting

### "Not finding all files"

```bash
# Check index status
lean-ctx index status

# Force a full rebuild
lean-ctx index build-full

# Check what's being scanned
lean-ctx doctor
```

### "Had to create a fake .git"

You don't need `.git` in the parent. As long as 2+ child directories have project markers, the parent is recognized as a multi-repo workspace. If you're still seeing issues, check:

```bash
# Verify detection
cd ~/code
lean-ctx status

# If it's not detected, ensure children have markers:
ls -la ~/code/*/  # Should show .git dirs in children
```

### "Graph not building automatically"

The graph builds in the background on first MCP tool call. You can trigger it manually:

```bash
lean-ctx index build          # Background build
lean-ctx index build-full     # Full rebuild (clears cache first)
```

### Extra roots not being indexed

Extra roots are indexed in the background after the primary root. Check:

```bash
lean-ctx config               # Verify extra_roots is set
lean-ctx index status         # Shows per-root index state
```

## How graph builds work per root

- **Primary root** (parent or single repo): Always indexed first, synchronously available
- **Extra roots**: Indexed in background threads (up to 8 in parallel)
- **Deduplication**: If an extra root is inside the primary root, it's skipped (already covered)
- **Per-root storage**: Each root gets its own `~/.lean-ctx/graphs/<hash>/` directory

## IDE-specific tips

### Cursor / VS Code

Open the parent folder as your workspace. Cursor sends `roots/list` with all workspace folders, and LeanCTX picks the best root + registers extras automatically.

### Claude Code

Set the environment variable before launching:
```bash
export LEAN_CTX_PROJECT_ROOT=~/code
```

### Multi-root VS Code workspaces

If you use a `.code-workspace` file with multiple folders, each folder is sent as an MCP root. LeanCTX picks the first one with markers as primary and indexes the rest as extra roots.
