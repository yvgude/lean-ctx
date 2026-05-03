# **FAQ — Installation & Setup**

> **Latest version: 3.4.5** — 49 MCP tools · 10 read modes · 90+ shell patterns
> Docs: <https://leanctx.com/docs/getting-started>

---

**Q: How do I install lean-ctx?**
```bash
curl -fsSL https://leanctx.com/install.sh | sh   # universal, no Rust needed
brew tap yvgude/lean-ctx && brew install lean-ctx  # macOS / Linux
npm install -g lean-ctx-bin                        # Node.js
cargo install lean-ctx                             # Rust
```
Then run `lean-ctx setup` and `lean-ctx doctor` to verify.

**Q: Do I need Rust installed?**
No. Since v3.2.3 the install script auto-detects if `cargo` is missing and downloads a pre-built binary. Rust is only needed if you want to build from source.

**Q: Which editors/AI tools are supported?**
lean-ctx auto-configures for: **Cursor, Claude Code, GitHub Copilot, Windsurf, VS Code, Zed, Codex CLI, Gemini CLI, OpenCode, Pi, Qwen Code, Trae, Amazon Q, JetBrains, Antigravity, Cline/Roo Code, Aider, Amp, Kiro, Continue, Crush** — run `lean-ctx setup` and it detects everything.

**Q: How do I update?**
```bash
lean-ctx update   # recommended — refreshes binary, hooks, and aliases
```
After updating, restart your shell (`source ~/.zshrc`) and your IDE.

**Q: How do I uninstall or temporarily disable?**
- Disable for current session: `lean-ctx-off`
- Re-enable: `lean-ctx-on`
- Full uninstall: `lean-ctx uninstall`
- Disable for a single command: `LEAN_CTX_DISABLED=1 your-command`
