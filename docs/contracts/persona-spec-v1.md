# Persona Spec — v1

Status: **stable (v1)** · Module: `core::persona` · EPIC 12.15

A **persona** is a declarative bundle that shapes the whole context surface for a
domain — not just coding. It lets a developer point lean-ctx at any workflow
(research, lead-gen, support, data analysis, …) by selecting or shipping a
persona, instead of forking behavior.

## What a persona controls

| Field | Type | Default | Meaning |
|-------|------|---------|---------|
| `name` | string | — (required) | Unique persona name (file stem when loaded from disk). |
| `description` | string | `""` | Human-readable summary. |
| `tool_profile` | string | `"power"` | Tool surface tier: `minimal`, `standard`, `power`, or `custom`. |
| `tools` | string[] | `[]` | Explicit tool list when `tool_profile = "custom"`. |
| `default_read_mode` | string | `"auto"` | Default `ctx_read` mode for this domain. |
| `compressor` | string | `"identity"` | Registry compressor name (see `extension-registry-v1`). |
| `chunker` | string | `"lines"` | Registry chunker name. |
| `intent_taxonomy` | string[] | `[]` | Task labels meaningful for the domain (coding default = the `TaskType` set). |
| `sensitivity_floor` | string | `"public"` | Minimum sensitivity classification: `public`, `internal`, `confidential`, `secret`. |

## Built-in presets (EPIC 12.16)

| Preset | Tool surface | Read mode | Intent taxonomy | Sensitivity floor |
|--------|--------------|-----------|-----------------|-------------------|
| `coding` (default) | `power` | `auto` | the `TaskType` set | `public` |
| `research` | `standard` | `map` | explore, summarize, compare, cite, synthesize | `public` |
| `lead-gen` (alias `sales`) | custom (read/search/url/knowledge/semantic) | `map` | prospect, qualify, enrich, outreach | `confidential` |
| `support` | `standard` | `auto` | triage, diagnose, resolve, escalate, document | `internal` |
| `data-analysis` | `standard` | `map` | ingest, clean, analyze, visualize, report | `internal` |

Non-coding personas also append a domain-specific terse output block (vocabulary
+ intent list) to the agent prompt. The `coding` persona leaves the prompt
unchanged (no regression).

## Selection

Resolution order (best-effort; unknown names fall back to `coding`):

1. `LEAN_CTX_PERSONA` environment variable
2. `persona = "…"` in `config.toml`
3. `coding` (the default, reproduces historical behavior)

A name resolves against **built-in presets** first, then a file at
`<personas_dir>/<name>.toml`. `personas_dir` is
`${XDG_CONFIG_HOME:-~/.config}/lean-ctx/personas` and can be overridden with
`LEAN_CTX_PERSONAS_DIR` (containers/CI/tests).

## Tool-surface precedence (backward compatible)

An explicit tool-profile setting always wins over the persona:

```
LEAN_CTX_TOOL_PROFILE  >  config.tool_profile  >  config.tools_enabled  >  persona.tool_profile  >  power
```

The `coding` persona's `tool_profile` is `power`, so installs that set nothing
behave exactly as before.

## Example (`~/.config/lean-ctx/personas/lead-gen.toml`)

```toml
name = "lead-gen"
description = "Outbound sales lead research"
tool_profile = "custom"
tools = ["ctx_read", "ctx_search", "ctx_url_read", "ctx_knowledge"]
default_read_mode = "map"
compressor = "whitespace"
chunker = "paragraph"
sensitivity_floor = "confidential"
intent_taxonomy = ["prospect", "qualify", "enrich", "outreach"]
```

Select it with `LEAN_CTX_PERSONA=lead-gen` or `persona = "lead-gen"` in config.

## Discovery

The active persona is reported at `GET /v1/capabilities` under `server.persona`;
built-in preset names are listed under `presets`.

## Versioning

Additive fields are non-breaking within v1; clients must ignore unknown fields.
Removing/renaming a field or changing selection semantics bumps the version.
