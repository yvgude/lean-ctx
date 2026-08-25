<div align="center">

<pre>
██╗     ███████╗ █████╗ ███╗   ██╗     ██████╗████████╗██╗  ██╗
██║     ██╔════╝██╔══██╗████╗  ██║    ██╔════╝╚══██╔══╝╚██╗██╔╝
██║     █████╗  ███████║██╔██╗ ██║    ██║        ██║    ╚███╔╝
██║     ██╔══╝  ██╔══██║██║╚██╗██║    ██║        ██║    ██╔██╗
███████╗███████╗██║  ██║██║ ╚████║    ╚██████╗   ██║   ██╔╝ ██╗
╚══════╝╚══════╝╚═╝  ╚═╝╚═╝  ╚═══╝     ╚═════╝   ╚═╝   ╚═╝  ╚═╝
</pre>

### **Control what your AI can see.**

**LeanCTX — an open-source context engineering layer for coding agents**

LeanCTX runs locally alongside your coding agent. It selects relevant project
context, shapes noisy tool output, preserves recoverable source references, and
records factual local evidence without taking ownership of your agent loop.

Your agent. Your model. Your tools. Your context.

<p>
  <a href="https://github.com/yvgude/lean-ctx/stargazers"><img src="https://img.shields.io/github/stars/yvgude/lean-ctx?style=social" alt="GitHub Stars"></a>&nbsp;&nbsp;
  <a href="https://github.com/yvgude/lean-ctx/actions/workflows/ci.yml"><img src="https://github.com/yvgude/lean-ctx/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/yvgude/lean-ctx/actions/workflows/security-check.yml"><img src="https://github.com/yvgude/lean-ctx/actions/workflows/security-check.yml/badge.svg" alt="Security"></a>
  <a href="https://crates.io/crates/lean-ctx"><img src="https://img.shields.io/crates/v/lean-ctx?color=%23e6522c" alt="crates.io"></a>
  <a href="https://www.npmjs.com/package/lean-ctx-bin"><img src="https://img.shields.io/npm/v/lean-ctx-bin?label=npm&color=%23cb3837" alt="npm"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache%202.0-blue.svg" alt="License"></a>
  <img src="https://img.shields.io/badge/Telemetry-Opt--in%20Only-brightgreen?logo=shield&logoColor=white" alt="Opt-in Telemetry">
</p>

<p>
  <a href="https://leanctx.com">Website</a>&nbsp;&nbsp;·&nbsp;&nbsp;<a href="https://leanctx.com/docs/getting-started">Docs</a>&nbsp;&nbsp;·&nbsp;&nbsp;<a href="#install">Install</a>&nbsp;&nbsp;·&nbsp;&nbsp;<a href="#how-it-works">How it works</a>&nbsp;&nbsp;·&nbsp;&nbsp;<a href="#proof-not-promises">Proof</a>&nbsp;&nbsp;·&nbsp;&nbsp;<a href="cookbook/README.md">Cookbook</a>&nbsp;&nbsp;·&nbsp;&nbsp;<a href="SECURITY.md">Security</a>
</p>

</div>

---

## What LeanCTX is

LeanCTX is the open-source context engineering layer between an AI coding agent
and the local project or tools it needs to understand.

```text
agent → LeanCTX context tools / shell hook → project and local tools
agent → optional local proxy                 → model provider
```

It helps an agent:

- **Select** the files, symbols, search results, and source regions relevant to
  the current task.
- **Shape** large reads and noisy command output into bounded, useful context.
- **Reuse** cached context instead of rediscovering the same source every turn.
- **Recover** exact source and archived output through content-addressed
  references.
- **Prove** local observations with receipts and independently checkable
  evidence primitives.

LeanCTX does not run or orchestrate agents, choose their models, own their task
logic, or turn a local observation into a business outcome claim.

## What is available now

- Local Runtime, CLI, MCP server, shell integration, and optional local proxy.
- Context-aware file reads, search, structural views, and shell-output shaping.
- Recoverable references for compressed or archived source and command output.
- Local receipt generation and offline-verification primitives.
- Supported setup paths for Codex, Claude Code, and Cursor.
- Local-first operation with opt-in telemetry.

## See it in action

<table>
  <tr>
    <td align="center" width="33%">
      <img src="assets/leanctx-demo.gif" width="320" alt="Map-mode file read and compressed git output">
      <br/>
      <strong>Read + Shell</strong>
      <br/>
      Focused reads and bounded command output
    </td>
    <td align="center" width="33%">
      <img src="assets/leanctx-gain.gif" width="320" alt="LeanCTX local gain dashboard">
      <br/>
      <strong>Local observability</strong>
      <br/>
      Inspect measured context movement
    </td>
    <td align="center" width="33%">
      <img src="assets/leanctx-benchmark.gif" width="320" alt="LeanCTX benchmark report">
      <br/>
      <strong>Reproducible comparison</strong>
      <br/>
      Evaluate a declared local workload
    </td>
  </tr>
</table>

<p align="center"><sub>Demo assets are generated from reproducible VHS tapes in <code>demo/</code>.</sub></p>

## Why developers use LeanCTX

- **More useful context** — show the agent the representation required by the
  task instead of forwarding every available byte.
- **Less repeated discovery** — reuse cached reads and durable task findings.
- **Exact recovery** — compressed output remains connected to retrievable
  source.
- **Existing workflow preserved** — attach to supported coding agents without
  replacing their loop.
- **Local visibility** — inspect context usage, provenance, and factual
  measurements on your machine.
- **Model independence** — keep the context layer on your side of the provider
  boundary.

## Core capabilities

### Context reads

Choose the smallest useful representation: full source, exact line ranges,
public signatures, structural maps, diffs, or task-focused excerpts.

### Search and navigation

Use lexical, semantic, and symbol-aware paths to find relevant code while
keeping returned context bounded.

### Shell-output shaping

Common development commands are recognized and condensed through deterministic
patterns. Raw output remains available when exact bytes matter.

### Recovery

Archived output receives stable references so exact source remains retrievable
when a compact representation is not enough.

### Local evidence

Receipts and verification tools record what the Runtime observed. Quality and
accepted outcomes remain explicit inputs rather than inferred savings claims.

## Install

Pick one installation method:

```bash
curl -fsSL https://leanctx.com/install.sh | sh
brew tap yvgude/lean-ctx && brew install lean-ctx
npm install -g lean-ctx-bin
cargo install lean-ctx
```

Connect one supported coding agent and verify the installation:

```bash
lean-ctx wrap codex       # or: claude / cursor
lean-ctx doctor
```

Use `lean-ctx unwrap codex` to remove that integration.

## How it works

LeanCTX has two optional local paths:

```text
read path: AI tool → MCP tools / shell hook → LeanCTX → project + local tools
wire path: AI tool → optional local proxy   → model provider
```

- The **MCP server** exposes focused context, search, recovery, knowledge, and
  verification tools.
- The **shell hook** shapes eligible command output before it enters the agent
  context.
- The **local proxy** can observe and transform only the traffic routed through
  it.
- The **local stores** retain recoverable artifacts and factual evidence under
  explicit path and policy boundaries.

The proxy cannot observe hidden provider prompts, retries, bills, or task
quality. LeanCTX therefore does not infer them.

## Supported setup paths

Codex, Claude Code, and Cursor are the current first-class local setup paths.
Other clients may work through compatible interfaces, but compatibility alone
is not a support or evidence guarantee.

## Proof, not promises

Compression is useful only when the resulting work remains correct and
recoverable. A public performance or savings claim requires:

- a named and reproducible workload;
- comparable baseline and treatment;
- a declared quality threshold;
- transparent measurement and methodology;
- inspectable evidence and an independent verification path.

Without those conditions, token and cost changes are diagnostic signals, not
accepted outcomes.

## Privacy and safety

- Local-first by default.
- Telemetry is opt-in.
- Secret scrubbing and path boundaries protect local tool flows.
- Security-sensitive operations use explicit allowlists and bounded inputs.
- Recovery references are scoped; they are not an authorization bypass.

Review [SECURITY.md](SECURITY.md) and run `lean-ctx doctor` before enabling a
new integration.

## Documentation

- [Developer documentation](docs/README.md)
- [Getting started](https://leanctx.com/docs/getting-started)
- [Cookbook](cookbook/README.md)
- [Security](SECURITY.md)
- [Changelog](CHANGELOG.md)

## Uninstall

```bash
lean-ctx uninstall --dry-run
lean-ctx uninstall
```

If LeanCTX was installed through a package manager, use that package manager to
remove the binary after LeanCTX has removed its own local integration files.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
