# .ctxpkg v2 Specification

**Version:** 2.0.0
**Status:** Draft
**Date:** 2026-05-22

## 1. Overview

`.ctxpkg v2` is an open, graph-native package format for portable AI-agent context. It extends `.ctxpkg v1` with a knowledge graph core, typed relationships, activation energy (Phi scoring), Hebbian edge weights, temporal decay, and composability through graph-merge operations.

**Key claims:**
- `.ctxpkg is to AI context what npm packages are to code: portable, versioned, composable, and installable.`
- `.ctxpkg is complementary to MCP (runtime), A2A (communication), and KCP (discovery).`

## 2. File Format

A `.ctxpkg` file is a JSON bundle (with optional zstd compression in future versions):

```json
{
  "manifest": { ... },
  "content": { ... }
}
```

In the future, `.ctxpkg` files MAY transition to ZIP-based archives:
```
my-package.ctxpkg (ZIP)
├── ctxpkg.json           # Manifest
├── graph.json             # Knowledge Graph
├── blobs/                 # Content-addressable large objects
│   ├── sha256-abc123...
│   └── sha256-def456...
└── rebuild.json           # Optional: Rebuild instructions
```

## 3. Manifest (`ctxpkg.json`)

### 3.1 Required Fields

| Field | Type | Description |
|-------|------|-------------|
| `schema_version` | `u32` | `2` for v2 packages |
| `name` | `string` | Package name, allows `@scope/name` format |
| `version` | `string` | SemVer version string |
| `description` | `string` | Human-readable description |
| `created_at` | `datetime` | ISO 8601 creation timestamp |
| `layers` | `string[]` | Legacy v1 layer names for backward compat |
| `integrity` | `object` | SHA-256 hashes and byte size |
| `provenance` | `object` | Tool name, version, project hash |

### 3.2 v2-Specific Fields

| Field | Type | Description |
|-------|------|-------------|
| `conformance_level` | `u32` | `1`, `2`, or `3` |
| `kind` | `string?` | What the package delivers: `context` (default), `skills`, `addon`, `grammar` — see §3.4 |
| `scope` | `string?` | Namespace prefix (e.g., `@company`) |
| `graph_summary` | `object?` | Node/edge counts, types, activation mean |
| `marketplace` | `object?` | Categories, badges, license |

### 3.3 Example

```json
{
  "schema_version": 2,
  "conformance_level": 2,
  "name": "@company/auth-service-context",
  "version": "1.3.0",
  "description": "Complete context for the auth service",
  "scope": "@company",
  "graph_summary": {
    "node_count": 342,
    "edge_count": 891,
    "node_types": ["fact", "gotcha", "decision", "code_symbol"],
    "activation_mean": 0.67,
    "freshness": "2026-05-20T00:00:00Z"
  },
  "dependencies": {
    "@company/base-architecture": "^2.0",
    "@verified/jwt-patterns": "^1.0"
  },
  "optional_dependencies": {
    "@verified/security-review": "^1.0"
  },
  "conflicts": ["@outdated/legacy-auth"],
  "provenance": {
    "tool": "lean-ctx",
    "tool_version": "3.7.0",
    "project_hash": "abc123..."
  },
  "integrity": {
    "sha256": "...",
    "content_hash": "..."
  },
  "signature": {
    "algorithm": "ed25519",
    "public_key": "...",
    "value": "..."
  },
  "compatibility": {
    "min_lean_ctx_version": "3.7.0",
    "agents": ["lean-ctx>=3.7", "cursor", "claude-code"],
    "v1_fallback": true
  },
  "marketplace": {
    "categories": ["security", "authentication"],
    "badges": ["verified", "enterprise"],
    "license": "proprietary"
  }
}
```

### 3.4 Package kinds (unified distribution)

Since spec revision 2.1 (2026-07), a package declares **what it delivers** via
the optional `kind` field. This unifies the distribution of agent knowledge and
agent capabilities under one format, one registry and one trust chain (design:
`docs/specs/unified-distribution-v1.md`):

| `kind` | Payload | Notes |
|--------|---------|-------|
| `context` | knowledge/graph/session/patterns/gotchas layers | The default. Behavior of all pre-existing packages, unchanged. |
| `skills` | markdown/script documents as verified content blobs | Content only — no execution semantics in lean-ctx. |
| `addon` | embedded addon manifest + per-platform binary artifact references | Installs through the addon trust chain (capabilities, sandbox, binhash, revocation). |
| `grammar` | embedded grammar manifest (signed tree-sitter dylib references) | In-process loading ⇒ mandatory hash + signature. |

Rules:

- `kind` **defaults to `context`** and writers MUST omit the field for that
  default — every pre-`kind` package stays byte-identical.
- Non-`context` kinds REQUIRE `schema_version: 2`; validators reject a v1
  manifest carrying a non-default `kind`.
- Readers MUST tolerate unknown `kind` values by refusing installation with a
  clear "newer lean-ctx required" error (never by misinterpreting the payload).

## 4. Knowledge Graph (`context_graph`)

The knowledge graph is the core of a v2 package. It is stored in the `content.context_graph` field.

### 4.1 Format

```json
{
  "format": "ctxpkg-graph-v2",
  "nodes": [...],
  "edges": [...]
}
```

### 4.2 Node Schema

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | `string` | yes | Unique node identifier within this package |
| `type` | `string` | yes | Node type (see taxonomy below) |
| `content` | `string` | yes | Node content / value |
| `activation` | `f64` | no | Activation energy (Phi), default `1.0` |
| `category` | `string?` | no | Classification category |
| `source` | `string?` | no | Source session or origin |
| `created_at` | `datetime?` | no | Creation timestamp |
| `decay_half_life_days` | `u32?` | no | Temporal decay half-life in days |
| `blob_ref` | `string?` | no | Reference to content-addressable blob |
| `file_path` | `string?` | no | Associated source file |
| `line_start` | `usize?` | no | Start line in source file |
| `line_end` | `usize?` | no | End line in source file |
| `confidence` | `f32?` | no | Confidence score (0.0-1.0) |
| `supersedes` | `string?` | no | ID of node this supersedes |

### 4.3 Node Type Taxonomy (extensible)

| Category | Types |
|----------|-------|
| Semantic | `fact`, `pattern`, `insight`, `convention` |
| Memory | `gotcha`, `decision`, `finding`, `episode` |
| Structure | `code_symbol`, `code_file`, `code_module`, `code_function`, `code_class` |
| Session | `session`, `task`, `evidence`, `procedure` |
| Governance | `policy`, `overlay`, `profile`, `slo` |
| Event | `bus_event`, `handoff`, `tool_call` |
| Custom | Any string (vendor-extensible) |

### 4.4 Edge Schema

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `from` | `string` | yes | Source node ID |
| `to` | `string` | yes | Target node ID |
| `type` | `string` | yes | Edge type (see taxonomy below) |
| `weight` | `f64` | no | Edge weight, default `1.0` |
| `coactivations` | `u32` | no | Hebbian co-activation counter |
| `metadata` | `string?` | no | Additional metadata |

### 4.5 Edge Type Taxonomy (extensible)

| Category | Types |
|----------|-------|
| Semantic | `supports`, `contradicts`, `supersedes`, `elaborates` |
| Causal | `decided_by`, `caused_by`, `followed_by`, `tested_by` |
| Structural | `imports`, `calls`, `exports`, `contains`, `related_to`, `depends_on` |
| Hebbian | `co_activated` (with `weight` + `coactivations`) |
| Dependency | `depends_on`, `extends`, `conflicts_with` |

## 5. Conformance Levels

Three levels enable progressive adoption:

### Level 1: Basic

Any tool can implement this in a single day.

- JSON manifest + flat `nodes` array (only `id`, `type`, `content`)
- No edges required
- Install = merge nodes as facts into agent context
- **Target tools**: Cursor, Claude Code, Copilot, Windsurf

### Level 2: Graph

Requires graph-merge logic.

- Typed nodes + typed edges
- Dependency resolution (SemVer)
- Graph-merge composition with conflict detection
- **Target tools**: lean-ctx, advanced MCP servers

### Level 3: Cognitive

Full lean-ctx cognitive state transfer.

- Activation energy (Phi scoring) on every node
- Hebbian edge weights + co-activation counters
- Temporal decay (nodes/edges age)
- Rebuild protocol for index reconstruction
- Overlay portability
- **Target tools**: lean-ctx (reference implementation)

## 6. Composition (Graph-Merge)

When installing multiple packages, contexts are composed using the Sheaf Gluing algorithm:

1. **Union**: All non-conflicting nodes (ID-based)
2. **Edge Merge**: Shared edges → weight averaging via `f64::midpoint`
3. **Conflict Detection**: Nodes with `contradicts` edges → warning
4. **Activation Propagation**: New edges from dependencies increase activation of connected nodes
5. **Supersedes Resolution**: If Package A node X `supersedes` Y, node Y is deactivated (activation = 0.0)

This is the **Sheaf Gluing Operation**: local sections (packages) glue to a global section (merged context) when compatibility conditions are met.

## 7. Scopes and Namespaces

```
@verified/       -- Verified by ctxpkg.com
@official/       -- Framework maintainers (React, Next.js, etc.)
@community/      -- Community-contributed
@<org>/           -- Private organization namespaces
@local/           -- Local/development packages
```

Package names follow the pattern `@scope/name` (e.g., `@company/auth-service`). Names without `@` are treated as unscoped.

## 8. Multi-Scale Support

| Scale | Example | Typical Size |
|-------|---------|-------------|
| Micro | `@verified/react-hooks-gotchas` | ~10 nodes, ~5 KB |
| Topic | `@verified/security-review-playbook` | ~100 nodes, ~50 KB |
| Project | `@company/my-saas-project` | ~1000 nodes, ~500 KB |
| System | `@company/platform-architecture` | ~5000 nodes, ~2 MB |
| Organization | `@company/full-engineering-context` | ~50000 nodes, ~20 MB |

## 9. Backward Compatibility with v1

v1 packages are interpreted as a graph without edges and with uniform activation:

| v1 Source | v2 Graph Mapping |
|-----------|-----------------|
| `knowledge.facts[]` | Nodes of type `fact` (activation: 1.0) |
| `knowledge.patterns[]` | Nodes of type `pattern` (activation: 1.0) |
| `graph.nodes/edges` | Nodes/edges with type mapping |
| `session` | Node of type `session` with blob_ref |
| `gotchas[]` | Nodes of type `gotcha` (activation: 1.0) |

The manifest field `compatibility.v1_fallback: true` signals that this v2 package can be read as v1 (edges and activation are ignored).

## 10. Integrity

```
content_hash = SHA256(content_json_bytes)
sha256 = SHA256("{name}:{version}:{content_hash}")
```

The integrity algorithm is identical to v1. The `content_json` includes the `context_graph` field for v2 packages.

## 11. Signing

Ed25519 signatures. The signing message is:

```
SHA256("ctxpkg-sign-v1:{name}:{version}:{integrity.sha256}")
```

## 12. CLI Interface

```bash
# Level 1: Basics (Knowledge + Gotchas + Session)
lean-ctx pack create --name my-pkg --level 1

# Level 2: + Graph + Relations + Dependencies
lean-ctx pack create --name my-pkg --level 2

# Level 3: Complete Cognitive State
lean-ctx pack create --name my-pkg --level 3

# Scoped packages
lean-ctx pack create --name auth-service --level 2 --scope @company

# Install
lean-ctx pack install @verified/react-patterns
lean-ctx pack install ./colleague-project.ctxpkg

# Info (shows v2 graph summary)
lean-ctx pack info my-pkg

# Publish (coming soon)
lean-ctx pack publish --registry https://registry.ctxpkg.com
```

## 13. Scientific Foundations

The `.ctxpkg v2` format is informed by research across multiple disciplines:

| Discipline | Key Insight | Design Impact |
|-----------|-------------|---------------|
| **Neuroscience** (HeLa-Mem, Kairos, Synapse) | Episodic-semantic dual graph with Hebbian learning | Typed nodes (fact/decision/episode), co-activation edge weights |
| **Physics** (Thermodynamic Transformers) | Softmax as free energy minimum | Activation energy (Phi) = node importance |
| **Information Theory** (COMI, γ-Covering) | Marginal information gain | Edge weights encode relevance minus redundancy |
| **Swarm Behavior** (SwarmSys, SBP) | Pheromone traces with temporal decay | `decay_half_life_days` on nodes |
| **Psychology** (Extended Mind, Situated Cognition) | Knowledge inseparable from relational context | Graph preserves relational structure |
| **Category Theory** (Sheaf Theory) | Local-to-global semantic gluing | Package composition via sheaf-gluing algorithm |

## 14. Ecosystem Positioning

`.ctxpkg` complements existing AI agent standards:

- **MCP** = how agents connect to tools (runtime, synchronous)
- **A2A** = how agents communicate with each other (tasks, events)
- **KCP** = how knowledge is structured for discovery (metadata)
- **.ctxpkg** = how context is packaged, shared, and composed (packaging, offline, composable)

## Appendix A: Differences from v1

| Aspect | v1 | v2 |
|--------|----|----|
| Data model | 5 static layers | Graph-native with typed nodes/edges |
| Composition | No merge semantics | Graph-merge with conflict detection |
| Dynamic state | None | Activation energy, Hebbian weights, temporal decay |
| Naming | Flat names | Scoped `@org/name` |
| Adoption path | All-or-nothing | 3 conformance levels |
| Marketplace | None | Categories, badges, license |
