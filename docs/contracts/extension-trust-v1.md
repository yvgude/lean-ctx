# Extension Trust & Sandbox — `extension-trust-v1`

Status: stable · EPIC 12.3 · Code: [`rust/src/core/plugins/sandbox.rs`](../../rust/src/core/plugins/sandbox.rs)

Every plugin subprocess — lifecycle **hooks** ([`executor`](../../rust/src/core/plugins/executor.rs))
and manifest-declared **tools** ([`tools`](../../rust/src/core/plugins/tools.rs)) — runs
under a [`SandboxPolicy`] derived from the plugin's `[trust]` manifest section.
The model is **least privilege by default** and deliberately split into two
honest categories so lean-ctx never claims enforcement it does not perform.

## Declaring trust

```toml
[plugin]
name = "my-plugin"
version = "0.1.0"

[trust]
permissions = ["network"]   # default: [] (least privilege)
```

Recognized permissions (anything else is a **manifest validation error** —
fail-closed, no silent grant):

| Permission | Category | Effect |
|------------|----------|--------|
| `env_passthrough` | **Enforced** | Opt **out** of env scrubbing; the child receives the full host environment. Without it the child only sees the [allowlist](#enforced-controls). |
| `network` | Declared | Plugin intends outbound network access. Surfaced for user consent + in `/v1/capabilities`; not OS-blocked. |
| `fs_write` | Declared | Plugin intends to write outside its own dir. Surfaced; not OS-blocked. |

## Enforced controls (deterministic)

Applied to every child before spawn, regardless of declared permissions:

1. **Environment isolation** — without `env_passthrough` the child's env is
   cleared and rebuilt from a fixed allowlist (`PATH`, `HOME`, `LANG`,
   `LC_ALL`, `LC_CTYPE`, `TMPDIR`/`TEMP`/`TMP`, and the Windows boot vars).
   Host secrets living in the ambient environment never reach the plugin.
   lean-ctx's own trusted vars (`LEAN_CTX_PLUGIN_DIR`, `LEAN_CTX_HOOK`/`_TOOL`)
   are set **after** scrubbing so they always apply.
2. **Working-directory jail** — cwd is pinned to the plugin directory (when it
   exists), so relative paths resolve inside the plugin, not the host cwd.
3. **Timeout** — each call is bounded by the hook/tool `timeout_ms`
   (enforced by the executor's wait loop; default 5000 ms).

## Declared controls (consent surface)

`network` and `fs_write` cannot be blocked portably without OS namespaces /
seccomp / sandbox-exec, which lean-ctx does not assume. Instead they are
**declared** capabilities: recorded on the manifest, surfaced to the user, and
exposed in the capabilities document so a harness/operator can decide whether to
trust the plugin. This is honest by design — a green checkmark you can audit,
not a guarantee we cannot keep.

## Discovery

`GET /v1/capabilities` → `extensions.plugins[]` carries the declared set:

```json
{
  "extensions": {
    "plugins": [
      { "name": "my-plugin", "version": "0.1.0", "permissions": ["network"] }
    ]
  }
}
```

An empty `permissions` array means least privilege (scrubbed env, nothing
declared).

## Versioning

`extension-trust-v1` is additive. New permissions may be added in a minor
revision; removing or repurposing one is a breaking change requiring `-v2`. The
enforced/declared split is part of the contract.
