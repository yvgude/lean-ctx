# Context Package Specification v1

**Status:** Stable — canonical package contract for LeanCTX v4
**Schema Version:** 1  
**Format:** `.ctxpkg` (JSON)  
**Max File Size:** 10 MB  
**License:** CC BY 4.0

## Overview

A `.ctxpkg` file is a single JSON document that packages AI-agent context into a portable, versioned, and verifiable format. It consists of a **manifest** (metadata + integrity) and a **content** section (typed layers).

## File Structure

```json
{
  "manifest": { ... },
  "content": { ... }
}
```

The top-level object has exactly two keys: `manifest` and `content`.

## Manifest

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `schema_version` | `u32` | Yes | Must be `1` |
| `name` | `string` | Yes | Package name. Pattern: `[a-zA-Z0-9._-]`, max 128 chars |
| `version` | `string` | Yes | Package version. Pattern: `[a-zA-Z0-9._+-]`, max 64 chars, must not start with `.` |
| `description` | `string` | Yes | Human-readable description |
| `author` | `string` | No | Package author |
| `created_at` | `string` (ISO 8601) | Yes | Creation timestamp |
| `updated_at` | `string` (ISO 8601) | No | Last update timestamp |
| `layers` | `array<LayerType>` | Yes | Non-empty, no duplicates |
| `dependencies` | `array<Dependency>` | No | Package dependencies (default: `[]`) |
| `tags` | `array<string>` | No | Searchable tags (default: `[]`) |
| `integrity` | `Integrity` | Yes | Hash verification data |
| `provenance` | `Provenance` | Yes | Origin tracking |
| `compatibility` | `Compatibility` | No | Tool/language requirements |
| `stats` | `Stats` | No | Content statistics |

### LayerType

One of: `"knowledge"`, `"graph"`, `"session"`, `"patterns"`, `"gotchas"`

### Dependency

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | `string` | Yes | Dependency package name |
| `version_req` | `string` | Yes | Version requirement |
| `optional` | `bool` | No | Whether the dependency is optional (default: `false`) |

### Integrity

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `sha256` | `string` | Yes | 64-char lowercase hex. Composite hash: `SHA-256("{name}:{version}:{content_hash}")` |
| `content_hash` | `string` | Yes | 64-char lowercase hex. `SHA-256(canonical_json(content))` |
| `byte_size` | `u64` | Yes | Byte length of `JSON.stringify(content)`. Must be > 0 |

#### Integrity Verification Algorithm

1. Serialize the `content` object to canonical JSON (keys sorted, no extra whitespace).
2. Compute `SHA-256` of the serialized content bytes. Compare with `content_hash`.
3. Compute `SHA-256("{name}:{version}:{content_hash}")`. Compare with `sha256`.
4. Verify `byte_size` matches the serialized content length.
5. If any check fails, reject the package.

### Provenance

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `tool` | `string` | Yes | Tool that created the package (e.g., `"lean-ctx"`) |
| `tool_version` | `string` | Yes | Version of the creating tool |
| `project_hash` | `string` | No | Hash of the source project at creation time |
| `source_session_id` | `string` | No | Session ID during creation |

### Compatibility

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `min_lean_ctx_version` | `string` | No | Minimum tool version required to load |
| `target_languages` | `array<string>` | No | Target programming languages |
| `target_frameworks` | `array<string>` | No | Target frameworks |

### Stats

| Field | Type | Description |
|-------|------|-------------|
| `knowledge_facts` | `u32` | Number of knowledge facts |
| `graph_nodes` | `u32` | Number of graph nodes |
| `graph_edges` | `u32` | Number of graph edges |
| `pattern_count` | `u32` | Number of patterns |
| `gotcha_count` | `u32` | Number of gotchas |
| `compression_ratio` | `f64` | Estimated compression ratio |

## Content Layers

The `content` object contains one key per declared layer. Only layers listed in `manifest.layers` should be present.

### knowledge

| Field | Type | Description |
|-------|------|-------------|
| `facts` | `array<KnowledgeFact>` | Structured knowledge entries |
| `patterns` | `array<ProjectPattern>` | Recurring code/architecture patterns |
| `insights` | `array<ConsolidatedInsight>` | Higher-level insights derived from facts |
| `exported_at` | `string` (ISO 8601) | Export timestamp |

#### KnowledgeFact

| Field | Type | Description |
|-------|------|-------------|
| `category` | `string` | Fact category (e.g., `"architecture"`, `"convention"`) |
| `key` | `string` | Fact identifier |
| `value` | `string` | Fact content |
| `confidence` | `f32` | Confidence score (0.0 - 1.0) |
| `source` | `string` | Where the fact was learned |
| `imported_from` | `string` (optional) | Package name if imported |

### graph

| Field | Type | Description |
|-------|------|-------------|
| `nodes` | `array<GraphNode>` | Code entities |
| `edges` | `array<GraphEdge>` | Relationships between entities |
| `exported_at` | `string` (ISO 8601) | Export timestamp |

#### GraphNode

| Field | Type | Description |
|-------|------|-------------|
| `kind` | `string` | Node type (e.g., `"function"`, `"class"`, `"module"`) |
| `name` | `string` | Entity name |
| `file_path` | `string` | Source file path |
| `line_start` | `u32` (optional) | Start line |
| `line_end` | `u32` (optional) | End line |
| `metadata` | `string` (optional) | Additional context |

#### GraphEdge

| Field | Type | Description |
|-------|------|-------------|
| `source_path` | `string` | Source file path |
| `source_name` | `string` | Source entity name |
| `target_path` | `string` | Target file path |
| `target_name` | `string` | Target entity name |
| `kind` | `string` | Edge type (e.g., `"calls"`, `"imports"`, `"inherits"`) |
| `metadata` | `string` (optional) | Additional context |

### session

| Field | Type | Description |
|-------|------|-------------|
| `task_description` | `string` (optional) | What the session was about |
| `findings` | `array<Finding>` | Discoveries during the session |
| `decisions` | `array<Decision>` | Decisions made |
| `next_steps` | `array<string>` | Planned follow-up actions |
| `files_touched` | `array<string>` | Files modified during the session |
| `exported_at` | `string` (ISO 8601) | Export timestamp |

#### Finding

| Field | Type | Description |
|-------|------|-------------|
| `summary` | `string` | Finding description |
| `file` | `string` (optional) | Related file |
| `line` | `u32` (optional) | Related line number |

#### Decision

| Field | Type | Description |
|-------|------|-------------|
| `summary` | `string` | Decision description |
| `rationale` | `string` (optional) | Why this decision was made |

### patterns

| Field | Type | Description |
|-------|------|-------------|
| `patterns` | `array<ProjectPattern>` | Standalone pattern definitions |
| `exported_at` | `string` (ISO 8601) | Export timestamp |

### gotchas

| Field | Type | Description |
|-------|------|-------------|
| `gotchas` | `array<Gotcha>` | Known pitfalls |
| `exported_at` | `string` (ISO 8601) | Export timestamp |

#### Gotcha

| Field | Type | Description |
|-------|------|-------------|
| `id` | `string` | Unique identifier |
| `category` | `string` | Gotcha category |
| `severity` | `string` | Severity level (e.g., `"high"`, `"medium"`, `"low"`) |
| `trigger` | `string` | What triggers this gotcha |
| `resolution` | `string` | How to resolve it |
| `file_patterns` | `array<string>` | Glob patterns for affected files |
| `confidence` | `f32` | Confidence score (0.0 - 1.0) |

## Transport Envelope

For agent-to-agent transfer, packages can be wrapped in a `TransportEnvelopeV1`:

| Field | Type | Description |
|-------|------|-------------|
| `format_version` | `u32` | Must be `1` |
| `sender` | `AgentIdentity` | Sender agent identity |
| `recipient` | `AgentIdentity` (optional) | Intended recipient |
| `content_type` | `string` | Must be `"context_package"` for .ctxpkg payloads |
| `payload_json` | `string` | JSON-serialized package content |
| `signature` | `string` (optional) | HMAC-SHA256 signature |
| `metadata` | `object` (optional) | Additional transport metadata |

### Signature Algorithm

When `signature` is present, it is computed as:

```
HMAC-SHA256(key=secret, message="v2:{content_type}:{payload_json}")
```

The signature is hex-encoded. Verification uses constant-time comparison.

## Validation Rules

1. `schema_version` must equal `1`.
2. `name` must be non-empty, max 128 chars, matching `[a-zA-Z0-9._-]+`.
3. `version` must be non-empty, max 64 chars, matching `[a-zA-Z0-9._+-]+`, not starting with `.`.
4. `layers` must be non-empty with no duplicates.
5. `integrity.sha256` and `integrity.content_hash` must be 64-char lowercase hex strings.
6. `integrity.byte_size` must be greater than 0.
7. File size must not exceed 10 MB.
8. Transport envelope payload must not exceed 2 MB.

## Legacy Compatibility

Files with the `.lctxpkg` extension (used before v3.6.14) are accepted for import with the same schema.
Use `lean-ctx pack migrate <file.lctxpkg>` to verify and create the canonical
`.ctxpkg` filename without changing package bytes or invalidating signatures.
The source file remains the rollback copy and a SHA-256 migration receipt binds
the two names.

## Reference Implementation

The reference implementation is [LeanCTX](https://leanctx.com). Source code: [github.com/yvgude/lean-ctx](https://github.com/yvgude/lean-ctx).
