You're putting LeanCTX in front of real code — possibly on a shared machine, in CI, or under a security policy. This journey covers every guardrail LeanCTX applies by default and every dial you can tighten: filesystem jail, shell allowlist, secret redaction, OS sandboxing, harden mode, and role policies.

---

## 0. The defense-in-depth model

Every file read and shell command flows through layered guardrails, **on by
default** — you don't opt in:

```mermaid
flowchart LR
  R[ctx_read / ctx_shell] --> PJ[PathJail<br/>stay in project root]
  PJ --> AL[Shell allowlist<br/>~200 known binaries]
  AL --> SB[OS sandbox<br/>Seatbelt / Landlock]
  SB --> SD[Secret redaction<br/>before output leaves]
  SD --> OUT[result to the agent]
```

You can tighten each layer (stricter shell parsing, harden mode, role policies)
but you cannot accidentally turn off the baseline.

---

## 1. PathJail — stay inside the project

**What it does:** confines file access to the resolved project root. Absolute
paths outside the jail are rejected, so a stray `read /etc/passwd` or a
path-traversal `../../` cannot escape the workspace.

- Re-rooting to a different project root is **off by default**
  (`allow_auto_reroot = false`) — LeanCTX will not silently follow an absolute
  path into another tree.
- Multi-root setups (`lean-ctx serve --root a:A --root b:B`) jail each root
  independently.

This is the foundation every other layer assumes.

---

## 2. Shell allowlist & strict mode

**What it does:** the shell hook only compresses/executes commands whose binary
is on the allowlist (~200 common dev tools: `git`, `cargo`, `npm`, `node`,
`python`, …). Anything else passes through untouched rather than being wrapped.

```toml
# config.toml
shell_allowlist = ["git", "cargo", "npm", "…"]   # override the default set
shell_strict_mode = false                         # set true to block $() and backticks
excluded_commands = []                            # never intercept these
```

- **`docker` / `podman` are deliberately *not* in the default allowlist** —
  mount flags can bypass PathJail. Add them explicitly only if you accept that.
- `shell_strict_mode = true` blocks command substitution (`$(…)`, backticks) for
  environments that must forbid dynamic command construction.
- Override the list without editing config via `LEAN_CTX_SHELL_ALLOWLIST`
  (comma-separated).

---

## 3. OS sandboxing for executed code

**What it does:** when LeanCTX executes code (`ctx_execute`), it runs under the
OS sandbox — **Seatbelt** on macOS and **Landlock** on Linux — so the executed
process gets a restricted filesystem/network view, not your full user
privileges. On platforms without a supported sandbox, execution is gated rather
than run unconfined.

This is separate from PathJail (which guards LeanCTX's *own* reads); the sandbox
guards *child processes* LeanCTX spawns on your behalf.

---

## 4. Secret redaction — nothing leaks to the model

**What it does:** before any shell output or file content is returned, LeanCTX
scans it for credentials (AWS keys, tokens, etc.) and replaces matches with
`[REDACTED:<kind>]`. It is **on by default**.

```toml
[secret_detection]
enabled = true
redact = true
custom_patterns = ["MYCORP_[A-Z0-9]{20}"]   # add org-specific secret shapes
```

Built-in patterns cover common cloud/credential formats; `custom_patterns` lets
you redact organization-specific secret shapes. Matches are reported with a safe
preview (e.g. `AKIA…`) so you know redaction fired without seeing the secret.

---

## 5. Harden mode — force the compressed path

**What it does:** `harden` makes the agent use LeanCTX's compressed `ctx_*`
tools by **denying native Read/Grep** (except immediately after an Edit). Two
levels:

```bash
lean-ctx harden            # soft: sets LEAN_CTX_HARDEN=1 in MCP configs
lean-ctx harden --hard     # also adds "Bash" to ~/.claude/settings.json deny
lean-ctx harden --undo     # remove both — native tools allowed again
```

Real output:

```text
lean-ctx harden (level: soft)

  [OK] /Users/you/.cursor/mcp.json
  [OK] /Users/you/.claude.json
  [OK] Set LEAN_CTX_HARDEN=1 in MCP configs

Harden active. Native Read/Grep will be denied (except after Edit).
Undo with: lean-ctx harden --undo
```

Soft harden is reversible and config-only; `--hard` additionally blocks the
Claude Bash tool. Both are fully undone by `--undo` (see also
[Journey 12 → §4](/docs/journeys/troubleshooting/)).

---

## 6. Role policies — least privilege per agent

**What it does:** role policies (set via
`ctx_session action=role` / `lean-ctx session role`) scope what a session may do
— e.g. a `reviewer` role that cannot write, or limiting privileged search
(`ctx_search ignore_gitignore=true` requires an admin-class role). Use this to
give a sub-agent or teammate a least-privilege surface.

---

## 7. Audit & alignment

LeanCTX maintains an **audit trail** of security-relevant actions and ships an
**OWASP alignment** map documenting which controls address which risks — useful
when answering a security review.

---

## Governance checklist

| Goal | Control |
|------|---------|
| Keep file access in-project | PathJail (on by default) |
| Restrict which commands run | `shell_allowlist` + `shell_strict_mode` |
| Never wrap docker mount-escapes | docker/podman off the default allowlist |
| Confine executed code | Seatbelt (macOS) / Landlock (Linux) |
| Stop secrets reaching the model | `[secret_detection]` (on by default) |
| Force compressed reads | `lean-ctx harden [--hard]` |
| Least-privilege agents | role policies |
| Answer a security review | audit trail + OWASP alignment |

> Tuning *how much* LeanCTX compresses or *which tools* it exposes lives in
> [Journey 10 — Customization & Governance](/docs/journeys/customization-and-governance/);
> this journey is specifically the **security** surface.
