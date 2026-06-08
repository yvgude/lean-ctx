You need a domain tool (an internal lookup) and a custom compressor for your data shape. Forking the engine and maintaining a patched build is a non-starter. LeanCTX gives you three escalating ways to extend it — none of which touch its source.

---

## 1. A tool the agent can call — a plugin manifest

LeanCTX registers it as a native MCP tool at startup and advertises it in
`/v1/capabilities`:

```toml
# plugin.toml
[plugin]
name = "crm"
version = "0.1.0"

[[tools]]
name = "crm_lookup"
description = "Look up an account in the CRM"
command = "crm-bin"                 # gets JSON args on stdin, returns text on stdout
timeout_ms = 8000
input_schema = { type = "object", properties = { account = { type = "string" } }, required = ["account"] }

[trust]
permissions = ["network"]          # declared + surfaced for consent
```

## 2. React to lifecycle events — hooks

Declare a command per event — `pre_read`, `post_compress`, `on_knowledge_update`,
`on_session_start` / `on_session_end`. Hooks are zero-cost when nothing listens.

## 3. A custom compressor / read-mode / chunker — WASM

Compile it to a sandboxed WASM module (any language) and drop it in a directory;
LeanCTX discovers it by file stem and registers it as a first-class extension:

```bash
export LEAN_CTX_WASM_DIR=~/.lean-ctx/wasm    # *.wasm → registered compressors
lean-ctx conformance                          # your extension is checked like a built-in
```

```text
/v1/capabilities → extensions.compressors: ["identity","markdown","prose","whitespace","my_ext"]
conformance scorecard → [ok] extensions/compressor:my_ext
```

## 4. Under the hood — `core/plugins/` + `core/wasm_ext.rs`

Plugins fire hooks and register manifest tools; the WASM host implements
`wasm-abi-v1` (`alloc` + `lctx_compress` / `lctx_provider_fetch`) with a
host-enforced byte budget, so a faulty guest can never overrun. Everything runs
under the extension trust model — scrubbed env, cwd jail, timeout, declared
permissions surfaced for consent — and is proven by the conformance suite.

## Payoff

The engine is extensible in any language, sandboxed, discoverable and
conformance-checked — your tools and transforms are first-class without ever
touching LeanCTX's source.
