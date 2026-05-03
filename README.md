```
  ██╗     ███████╗ █████╗ ███╗   ██╗     ██████╗████████╗██╗  ██╗
  ██║     ██╔════╝██╔══██╗████╗  ██║    ██╔════╝╚══██╔══╝╚██╗██╔╝
  ██║     █████╗  ███████║██╔██╗ ██║    ██║        ██║    ╚███╔╝ 
  ██║     ██╔══╝  ██╔══██║██║╚██╗██║    ██║        ██║    ██╔██╗ 
  ███████╗███████╗██║  ██║██║ ╚████║    ╚██████╗   ██║   ██╔╝ ██╗
  ╚══════╝╚══════╝╚═╝  ╚═╝╚═╝  ╚═══╝     ╚═════╝   ╚═╝   ╚═╝  ╚═╝
             Context Runtime for AI Agents
```

<h3 align="center">The context layer for AI coding agents</h3>

<p align="center">
  <strong>Reduce token waste in Cursor, Claude Code, Copilot, Windsurf, Codex, Gemini & more by 60–95% (up to 99% on cached reads)</strong><br/>
  Shell Hook + MCP Server · 49 tools · 10 read modes · 90+ patterns · Single Rust binary
</p>

<p align="center">
  <a href="https://github.com/yvgude/lean-ctx/actions/workflows/ci.yml"><img src="https://github.com/yvgude/lean-ctx/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/yvgude/lean-ctx/actions/workflows/security-check.yml"><img src="https://github.com/yvgude/lean-ctx/actions/workflows/security-check.yml/badge.svg" alt="Security"></a>
  <a href="https://crates.io/crates/lean-ctx"><img src="https://img.shields.io/crates/v/lean-ctx?color=%23e6522c" alt="crates.io"></a>
  <a href="https://crates.io/crates/lean-ctx"><img src="https://img.shields.io/crates/d/lean-ctx?color=%23e6522c" alt="Downloads"></a>
  <a href="https://www.npmjs.com/package/lean-ctx-bin"><img src="https://img.shields.io/npm/v/lean-ctx-bin?label=npm&color=%23cb3837" alt="npm"></a>
  <a href="https://aur.archlinux.org/packages/lean-ctx"><img src="https://img.shields.io/aur/version/lean-ctx?color=%231793d1" alt="AUR"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache%202.0-blue.svg" alt="License"></a>
  <a href="https://discord.gg/pTHkG9Hew9"><img src="https://img.shields.io/badge/Discord-Join-5865F2?logo=discord&logoColor=white" alt="Discord"></a>
  <a href="https://x.com/leanctx"><img src="https://img.shields.io/badge/𝕏-Follow-000000?logo=x&logoColor=white" alt="X/Twitter"></a>
  <img src="https://img.shields.io/badge/Telemetry-Opt--in%20Only-brightgreen?logo=shield&logoColor=white" alt="Opt-in Telemetry">
</p>

<p align="center">
  <a href="https://leanctx.com">Website</a> ·
  <a href="https://leanctx.com/docs/getting-started">Docs</a> ·
  <a href="#get-started-60-seconds">Install</a> ·
  <a href="#demo">Demo</a> ·
  <a href="#benchmarks">Benchmarks</a> ·
  <a href="cookbook/README.md">Cookbook</a> ·
  <a href="SECURITY.md">Security</a> ·
  <a href="CHANGELOG.md">Changelog</a> ·
  <a href="https://discord.gg/pTHkG9Hew9">Discord</a>
</p>

---

> **lean-ctx** is a local-first context runtime that compresses file reads + shell output before they reach the LLM. Cached re-reads drop to **~13 tokens**.

<p align="center"><strong>See it in action:</strong></p>

<table>
  <tr>
    <td align="center" width="33%">
      <img src="assets/leanctx-demo.gif" width="320" alt="Map-mode file read + compressed git output demo">
      <br/>
      <strong>Read + Shell</strong>
      <br/>
      Map-mode reads + compressed CLI output
    </td>
    <td align="center" width="33%">
      <img src="assets/leanctx-gain.gif" width="320" alt="lean-ctx gain live dashboard demo">
      <br/>
      <strong>Gain (live)</strong>
      <br/>
      Tokens + USD savings in real time
    </td>
    <td align="center" width="33%">
      <img src="assets/leanctx-benchmark.gif" width="320" alt="lean-ctx benchmark report demo">
      <br/>
      <strong>Benchmark proof</strong>
      <br/>
      Measure compression by language + mode
    </td>
  </tr>
</table>

<p align="center"><sub>All GIFs are generated from reproducible VHS tapes in <code>demo/</code>.</sub></p>

## What it does

- **File reads (MCP)**: cached + mode-aware reads (`full`, `map`, `signatures`, `diff`, …)
- **Shell output (hook)**: compresses noisy CLI output via 90+ patterns (git, npm, cargo, docker, …)
- **Session memory (CCP)**: persist task/facts/decisions across chats for faster cold starts
- **HTTP mode**: `lean-ctx serve` for Streamable HTTP MCP + `/v1/tools/call` (used by the Cookbook + SDK)

## How it works (30 seconds)

```
AI tool  →  (MCP tools + shell commands)  →  lean-ctx  →  your repo + CLI
```

- **MCP server**: exposes `ctx_*` tools (read modes, caching, deltas, search, memory, multi-agent)
- **Shell hook**: transparently compresses common commands so the LLM sees less noise
- **CCP**: persists session state so long-running work doesn’t “cold start” every chat

## Get started (60 seconds)

```bash
# 1) Install (pick one)
curl -fsSL https://leanctx.com/install.sh | sh      # universal (no Rust needed)
brew tap yvgude/lean-ctx && brew install lean-ctx    # macOS / Linux
npm install -g lean-ctx-bin                          # Node.js
cargo install lean-ctx                               # Rust

# 2) Setup (shell + auto-detected AI tools)
lean-ctx setup

# 3) Verify
lean-ctx doctor

# 4) See the payoff
lean-ctx gain --live
lean-ctx wrapped --week
```

After `setup`, restart your shell and your editor/AI tool once so the MCP + hooks are active.

<details>
<summary><strong>Troubleshooting / Safety</strong></summary>

- Disable immediately (current shell): `lean-ctx-off`
- Run a single command uncompressed: `lean-ctx -c --raw "git status"`
- Update: `lean-ctx update`
- Diagnose (shareable): `lean-ctx doctor --json`

</details>

## Supported IDEs & AI tools

lean-ctx is a standard **MCP server**, so it works with any MCP-compatible client.

For first-class integration, run:

```bash
lean-ctx init --agent <tool>
```

Supported `<tool>` values (24):

<details>
<summary><strong>Show full list</strong></summary>

- Cursor (`cursor`)
- Claude Code (`claude`)
- GitHub Copilot (`copilot`)
- Windsurf (`windsurf`)
- Codex CLI (`codex`)
- Gemini CLI (`gemini`)
- Cline (`cline`)
- Roo Code (`roo`)
- OpenCode (`opencode`)
- Crush (`crush`)
- Amazon Q Developer (`amazonq`)
- AWS Kiro (`kiro`)
- Antigravity (`antigravity`)
- Hermes (`hermes`)
- Qwen (`qwen`)
- Trae (`trae`)
- Verdent (`verdent`)
- Pi (`pi`)
- Aider (`aider`)
- Amp (`amp`)
- JetBrains IDEs (`jetbrains`)
- Emacs (`emacs`)
- Neovim (`neovim`)
- Sublime Text (`sublime`)

</details>

Also supported via MCP config (auto-detected in `setup`): **VS Code** and **Zed**.

### When to use (and when not to)

**Great fit if you…**
- use AI coding tools daily and your sessions are shell-heavy (git/tests/builds)
- work in medium/large repos (50+ files / monorepos)
- want a local-first layer with **no telemetry by default**

**Skip it if you…**
- mostly work in tiny repos and rarely call the shell from your AI tool
- always need raw/unfiltered logs (you can still use `--raw`, but ROI is lower)

<a id="demo"></a>

## Demo

Try these in any repo:

```bash
lean-ctx read rust/src/server/mod.rs -m map
lean-ctx -c "git log -n 5 --oneline"
lean-ctx gain --live
lean-ctx benchmark report .
```

- The repo ships the exact tapes used to render the GIFs in `demo/`
- Regenerate locally:

```bash
vhs demo/leanctx.tape
vhs demo/gain.tape
vhs demo/benchmark.tape
```

<a id="benchmarks"></a>

## Benchmarks

- **Latest snapshot**: [BENCHMARKS.md](BENCHMARKS.md)
- **Reproduce**:

```bash
lean-ctx benchmark report .
```

## Docs

- Getting started: https://leanctx.com/docs/getting-started
- Tools reference: https://leanctx.com/docs/tools/
- CLI reference: https://leanctx.com/docs/cli-reference/
- FAQ: [discord-faq.md](discord-faq.md)
- Feature catalog (SSOT snapshot): [LEANCTX_FEATURE_CATALOG.md](LEANCTX_FEATURE_CATALOG.md)
- Architecture: [ARCHITECTURE.md](ARCHITECTURE.md)
- Vision: [VISION.md](VISION.md)

## Privacy & security

- **No telemetry by default**
- **Optional anonymous stats sharing** (opt-in during setup)
- **Disableable update check** (config `update_check_disabled = true` or `LEAN_CTX_NO_UPDATE_CHECK=1`)
- Runs locally; your code never leaves your machine unless you explicitly enable cloud sync

See [SECURITY.md](SECURITY.md).

## Uninstall

```bash
lean-ctx-off       # disable immediately (current shell session)
lean-ctx uninstall # remove hooks + editor configs + data dir

# Remove the binary (pick your install method)
brew uninstall lean-ctx
npm uninstall -g lean-ctx-bin
cargo uninstall lean-ctx
```

## Contributing

Start with [CONTRIBUTING.md](CONTRIBUTING.md). Easy first PR: propose a new CLI compression pattern via the [issue template](.github/ISSUE_TEMPLATE/compression_pattern.md).

## License

Apache License 2.0 — see [LICENSE](LICENSE).

Portions of this software were originally released under the MIT License. See [LICENSE-MIT](LICENSE-MIT) and [NOTICE](NOTICE).
