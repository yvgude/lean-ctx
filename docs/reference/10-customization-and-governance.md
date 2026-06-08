# Journey 10 — Customization & Governance

> lean-ctx ships with sensible defaults, but everything is tunable: how
> aggressively it compresses, which tools your agent sees, how output looks, and
> what rules your team enforces. This journey covers every knob and every
> governance surface — the "make it behave exactly how we want" journey.

> **Looking for the security surface** (PathJail, shell allowlist, secret
> redaction, sandbox, `harden`, role policies)? That's
> [Journey 13 — Security & Governance](13-security-and-governance.md). This
> journey is about *behavior* tuning; Journey 13 is about *guardrails*.

Source files referenced here:
- `rust/src/cli/cheatsheet_cmd.rs` — `compression` / `terse` levels
- `rust/src/cli/profile_cmd.rs` — `tools` (MCP profiles) + `profile` (context profiles)
- `rust/src/cli/config_cmd.rs` — `config`
- `rust/src/cli/theme_cmd.rs` — `theme`
- `rust/src/cli/tee_cmd.rs` — `filter` (custom compression filters)
- `rust/src/cli/harden.rs` — `harden`
- `rust/src/core/contextops/` + `rules` command — governance

---

## 1. Compression level — the master dial

One command sets how hard lean-ctx works to save tokens.

```bash
lean-ctx compression               # show current level + active components
lean-ctx compression standard      # set it (alias: lean-ctx terse standard)
```

| Level | When to use |
|-------|-------------|
| `off` | debugging lean-ctx itself; you want raw output |
| `lite` | **default** — plain, concise prose; maximum fidelity, light savings |
| `standard` | balanced — denser symbolic "power mode" output |
| `max` | aggressive — smallest context, densest agent prompts |

> **Default:** `lite` (`compression_level = "lite"`). `lite` keeps the model's
> prose plain and readable; `standard`/`max` switch on the denser symbolic
> styles. This dial controls the **model's output style**, not lean-ctx's own
> tool-output compression (which is always on).

Each level is not a single switch — it expands into **four coordinated
components** (shown by `lean-ctx compression`):

| Component | Values |
|-----------|--------|
| Agent prompt (`TerseAgent`) | off / lite / full / ultra |
| Output density (`OutputDensity`) | normal / terse / ultra |
| CRP mode | the agent response-compression profile |
| Token-model tuning | matched to the level |

Setting a level also **injects the matching compression prompt into your rules
files** (`rules_inject::inject`) so the agent itself responds tersely. Restart
the agent/IDE to apply.

### Overrides (most specific wins)

```bash
LEAN_CTX_COMPRESSION=standard      # per shell session (env)
```
```toml
# .lean-ctx.toml (per project)
compression_level = "standard"
```

So you can run `max` globally but pin one tricky repo to `lite` without touching
global config.

---

## 2. MCP tool profiles — what your agent can see

Fewer tools = fewer tokens spent on tool definitions and less agent confusion.
`lean-ctx tools` chooses which `ctx_*` tools are exposed.

```bash
lean-ctx tools                     # show active profile
lean-ctx tools minimal             # ~6 core read/search/session tools
lean-ctx tools standard            # the balanced everyday set (~22 tools)
lean-ctx tools power               # everything (graph, control, agent, …)
lean-ctx tools list                # list tools per profile
```

| Profile | Tools | Best for |
|---------|-------|----------|
| `minimal` | ~6 | small models / strict token budgets |
| `standard` | ~22 | most users — recommended everyday trim |
| `power` | all (71) | code-intelligence + multi-agent + context-engineering work |

> **Default:** with no explicit `tool_profile` in config, lean-ctx exposes the
> **`power`** set (every tool) — `tool_profile_effective()` falls back to `power`.
> Run `lean-ctx tools standard` to trim to the everyday set, or `minimal` for
> strict token budgets. See the [MCP tool map](appendix-mcp-tools.md) for exactly
> which tool sits in which profile.

**Golden output — `lean-ctx tools`** shows the active profile, the exact tool
count, and where the value came from (so the `power`/68 default is verifiable):

```text
Tool Profile: power
  Tools exposed: 68
  Description:   All tools exposed
  Source:         default (backward compatible)

  Switch with: lean-ctx tools <minimal|standard|power>
```

`Source: default (backward compatible)` is exactly the fallback described above —
no `tool_profile` was set, so `power` is in effect.

---

## 3. Context profiles — saved tuning presets

Where `tools` picks the *tool surface*, `profile` saves a *full tuning preset*
(compression + behavior) you can switch between.

```bash
lean-ctx profile list              # available context profiles
lean-ctx profile active            # which one is active
lean-ctx profile show <name>       # inspect a profile
lean-ctx profile diff <a> <b>      # compare two
lean-ctx profile create <name>     # snapshot current settings as a profile
lean-ctx profile set <name>        # activate
```

Use this to keep, say, a `review` profile (high fidelity) and a `bulk` profile
(max compression) and flip between them per task.

> "TOOL PROFILES" (`tools`) and "CONTEXT PROFILES" (`profile`) are different
> axes — §2 controls *which tools*, §3 controls *how they behave*.

---

## 4. The config file — every setting in one place

```bash
lean-ctx config                    # dump effective config
lean-ctx config show               # human-readable
lean-ctx config init               # write a starter config.toml
lean-ctx config schema             # full key reference
lean-ctx config validate           # check a config for errors
lean-ctx config set <key> <value>  # set one key
lean-ctx config apply              # apply changes to a running daemon
```

After editing config that the daemon reads, run `lean-ctx restart` (Journey 6)
so the daemon reloads. Full key list: [Paths, env vars & config](appendix-paths-and-config.md).

---

## 5. Themes — terminal output styling

```bash
lean-ctx theme list                # available themes
lean-ctx theme set <name>          # apply
lean-ctx theme export / import     # share a theme
```

Purely cosmetic (colors of CLI output); no effect on what's sent to the agent.

---

## 6. Custom compression filters

Beyond the built-in compressors, you can define project-specific filters that
strip or reshape command output.

```bash
lean-ctx filter list               # configured filters
lean-ctx filter init               # scaffold a filter config
lean-ctx filter validate           # check filter definitions
```

Use this when a tool your team runs produces noisy output that the generic
compressor doesn't handle well.

---

## 7. Governance — `rules` (ContextOps)

For teams, the agent rules files (AGENTS.md, `.cursor/rules`, etc.) are
configuration that should be version-controlled and kept in sync.

```bash
lean-ctx rules status              # are rules present & current?
lean-ctx rules init                # create governed rules
lean-ctx rules diff                # local vs. canonical
lean-ctx rules lint                # validate rules
lean-ctx rules sync                # bring rules up to date
```

This makes "every dev's agent follows the same rules" enforceable rather than
hoped-for.

### Promote learned knowledge into rules

```bash
lean-ctx export-rules              # high-confidence knowledge → rules files
```

This turns durable facts your sessions discovered (Journey 3) into persistent
agent rules — closing the loop from "learned once" to "always known".

---

## 8. Security hardening — `harden`

By default lean-ctx *encourages* agents to use `ctx_*` tools. `harden` makes it
*enforced* by denying native Read/Grep in the agent's MCP config.

```bash
lean-ctx harden                    # soft: set LEAN_CTX_HARDEN=1 in MCP configs
lean-ctx harden --hard             # also add Bash to Claude Code permissions.deny
lean-ctx harden --undo             # revert everything
```

After hardening, native Read/Grep are denied (except immediately after an Edit,
so edit-verify still works). This guarantees token discipline across a team
rather than relying on each agent's goodwill.

> Safety reference: `lean-ctx safety-levels` prints the compression
> safety-level table (what each level is allowed to drop), so you can audit
> exactly what hardening + a given compression level will and won't strip.

---

## 9. Decision guide

| You want… | Reach for |
|-----------|-----------|
| Save more / fewer tokens globally | `compression` (§1) |
| Limit which tools the agent sees | `tools` (§2) |
| Switch between tuning presets per task | `profile` (§3) |
| Change a single setting precisely | `config set` (§4) |
| Recolor CLI output | `theme` (§5) |
| Tame one noisy command's output | `filter` (§6) |
| Enforce shared agent rules across a team | `rules` + `export-rules` (§7) |
| Force token discipline (deny native reads) | `harden` (§8) |

---

## Storage & config (customization)

| Path / key | Controls |
|------------|----------|
| `config.toml` `compression_level` | global compression level |
| `.lean-ctx.toml` `compression_level` | per-project override |
| `LEAN_CTX_COMPRESSION` (env) | per-session override |
| `LEAN_CTX_HARDEN=1` (env, set by `harden`) | deny native reads |
| `config.toml` profile/tool-profile keys | active profiles |

---

## UX notes captured during this walkthrough

- `tools` vs `profile` is the single most confusing pair of names in the CLI;
  §2/§3 state the distinction up front and §9 disambiguates by intent.
- `compression` expanding into four hidden components is powerful but invisible;
  documented here so users understand why one flag changes agent behavior *and*
  output.
- `harden` is the strongest token-discipline lever and is under-advertised;
  surfaced as a first-class governance tool with its exact effects and undo.
