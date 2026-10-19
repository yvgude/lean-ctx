# LeanCTX Engine

**LeanCTX is the Context Gateway for AI Systems.**
**Control what your AI can see.**

The open-source Engine brings context selection, supported access and content
controls, local knowledge and observable evidence to existing AI tools through
CLI, MCP, hooks and supported proxy paths. Your host owns the task, agent loop
and model.

## Install and connect an agent

```bash
cargo install lean-ctx --locked
lean-ctx wrap codex     # or: wrap claude / wrap cursor
lean-ctx doctor
```

## Select · Control · Learn · Prove

- **Select:** search and read the detail a task needs, with compact views and
  source references for recovering more.
- **Control:** apply configured path permissions, content filters and budgets
  on supported integration paths.
- **Learn:** retain local task, finding and decision records and connect code
  and knowledge across sessions. This does not train the model.
- **Prove:** inspect the source, operation, policy and usage evidence the
  integration can observe with `lean-ctx inspect`.

A context-only integration records its prepared result; the host owns the
subsequent model call. Filters have configured coverage limits. Recovery
depends on current permissions, retention and source or archive availability.
Usage estimates and calculated costs are separate from answer quality.

## Build from source

From the repository's `rust/` directory:

```bash
cargo check --locked
cargo fast
```

Development builds use the configured jobs and shared target directory. Use
`cargo build --release --locked` for explicit release validation. Keep the
installed runtime running while building or testing; `lean-ctx dev-install`
performs the supported atomic replacement after a successful build.

## Validate a change

```bash
cargo test --lib
cargo clippy --all-features -- -D warnings
cargo fmt --check
```

## Integration boundaries

- **Attach:** use the installed CLI, MCP server, hooks or supported local proxy.
- **Embed:** use the standalone LeanCTX SDK's released lifecycle and Agent Tools
  contracts. Preview operations and older in-tree experiments keep their own limits.
- **Commercial deployments:** Enterprise and SDK/OEM have separate scope and
  licensing; consult the applicable release and agreement.

See the [repository overview](https://github.com/yvgude/lean-ctx),
[installation guide](https://leanctx.com/docs/getting-started),
[security policy](https://github.com/yvgude/lean-ctx/blob/main/SECURITY.md),
[canonical positioning](https://github.com/yvgude/lean-ctx/blob/main/docs/POSITIONING_CANONICAL.md)
and [SDK boundaries](https://github.com/yvgude/lean-ctx/blob/main/docs/reference/sdk-surface.md).
