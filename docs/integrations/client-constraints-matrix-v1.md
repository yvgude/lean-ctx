# Client Constraints Matrix v1 (docs‑backed SSOT)

> **Status: local integration implementation reference.** This matrix describes
> client configuration constraints, not a product tier or availability promise.
> LeanCTX is **The Context SDK for AI Agents**; current scope and status are
> governed by `docs/internal/README.md` (internal, not in this repository).

This document is the SSOT for **client-specific MCP integration constraints** (config schema, hook semantics, approval model, limits).

It is used by:

- the **Instruction Compiler** (client profiles, size bounds, deterministic compilation)
- `lean-ctx setup` / `lean-ctx doctor` (autotuning, drift detection, repair guidance)

## Machine‑readable block

<!-- leanctx-client-constraints-v1-json -->
```json
{
  "schemaVersion": 1,
  "updatedAt": "2026-05-02",
  "clients": [
    {
      "id": "cursor",
      "displayName": "Cursor",
      "config": { "paths": ["~/.cursor/mcp.json", ".cursor/mcp.json"], "rootKey": "mcpServers" },
      "hooks": { "supported": true, "paths": ["~/.cursor/hooks.json", ".cursor/hooks.json"], "events": ["preToolUse"] },
      "toolApproval": { "model": "autoApprove", "key": "autoApprove" },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://cursor.com/docs/mcp.md", "https://cursor.com/docs/hooks"]
    },
    {
      "id": "claude-code",
      "displayName": "Claude Code",
      "config": { "paths": ["~/.claude.json"], "rootKey": "mcpServers" },
      "hooks": { "supported": true, "paths": ["~/.claude/settings.json", ".claude/settings.json"], "events": ["PreToolUse"] },
      "toolApproval": { "model": "prompted", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": 2048 },
      "sources": ["https://code.claude.com/docs/en/overview", "https://code.claude.com/docs/en/hooks"]
    },
    {
      "id": "vscode-copilot",
      "displayName": "VS Code / GitHub Copilot",
      "config": { "paths": [".vscode/mcp.json", "~/Library/Application Support/Code/User/mcp.json"], "rootKey": "servers" },
      "hooks": { "supported": true, "paths": [".github/hooks/hooks.json", "~/.github/hooks/hooks.json"], "events": ["preToolUse", "postToolUse"] },
      "toolApproval": { "model": "prompted_or_policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": [
        "https://code.visualstudio.com/docs/copilot/reference/mcp-configuration",
        "https://code.visualstudio.com/docs/copilot/customization/mcp-servers",
        "https://docs.github.com/en/copilot/concepts/context/mcp"
      ]
    },
    {
      "id": "windsurf",
      "displayName": "Windsurf (Cascade)",
      "config": { "paths": ["~/.codeium/windsurf/mcp_config.json"], "rootKey": "mcpServers" },
      "hooks": { "supported": true, "paths": ["~/.codeium/windsurf/hooks.json", ".windsurf/hooks.json"], "events": ["pre_mcp_tool_use", "post_mcp_tool_use"] },
      "toolApproval": { "model": "prompted_or_policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://docs.windsurf.com/windsurf/cascade/mcp", "https://docs.windsurf.com/windsurf/cascade/hooks"]
    },
    {
      "id": "roo",
      "displayName": "Roo Code",
      "config": { "paths": [".roo/mcp.json"], "rootKey": "mcpServers" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "prompted_or_policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://docs.roocode.com/features/mcp/recommended-mcp-servers"]
    },
    {
      "id": "cline",
      "displayName": "Cline",
      "config": { "paths": ["VS Code extension settings (platform-specific)"], "rootKey": null },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "prompted_or_policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://docs.cline.bot/customization/cline-rules"]
    },
    {
      "id": "zed",
      "displayName": "Zed",
      "config": { "paths": ["~/Library/Application Support/Zed/settings.json", "~/.config/zed/settings.json"], "rootKey": "context_servers" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "prompted_or_policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://zed.dev/docs/assistant/model-context-protocol"]
    },
    {
      "id": "jetbrains",
      "displayName": "JetBrains IDEs",
      "config": { "paths": ["~/.jb-mcp.json"], "rootKey": "servers" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "prompted_or_policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://www.jetbrains.com/help/ai-assistant/mcp.html"]
    },
    {
      "id": "opencode",
      "displayName": "OpenCode",
      "config": { "paths": ["~/.config/opencode/opencode.json", "opencode.json"], "rootKey": "mcp" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "prompted_or_policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://opencode.ai/docs/mcp-servers/", "https://opencode.ai/docs/config/"]
    },
    {
      "id": "crush",
      "displayName": "Crush",
      "config": { "paths": ["~/.config/crush/crush.json"], "rootKey": "mcp" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "prompted_or_policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://mintlify.com/charmbracelet/crush/configuration/mcp"]
    },
    {
      "id": "amp",
      "displayName": "Amp",
      "config": { "paths": ["~/.config/amp/settings.json", ".amp/settings.json"], "rootKey": "amp.mcpServers" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "prompted_or_policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://ampcode.com/manual", "https://ampcode.com/news/cli-workspace-settings"]
    },
    {
      "id": "hermes",
      "displayName": "Hermes Agent",
      "config": { "paths": ["~/.hermes/config.yaml"], "rootKey": "mcp_servers" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://hermes-agent.nousresearch.com/docs/reference/mcp-config-reference"]
    },
    {
      "id": "kiro",
      "displayName": "AWS Kiro",
      "config": { "paths": ["~/.kiro/settings/mcp.json", ".kiro/settings/mcp.json"], "rootKey": "mcpServers" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "autoApprove", "key": "autoApprove" },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://kiro.dev/docs/mcp/configuration/"]
    },
    {
      "id": "amazonq",
      "displayName": "Amazon Q Developer",
      "config": { "paths": ["~/.aws/amazonq/default.json", ".amazonq/default.json"], "rootKey": "mcpServers" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "per_tool_permissions", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://docs.aws.amazon.com/amazonq/latest/qdeveloper-ug/mcp-ide.html"]
    },
    {
      "id": "gemini-cli",
      "displayName": "Gemini CLI",
      "config": { "paths": ["~/.gemini/settings.json", ".gemini/settings.json"], "rootKey": "mcpServers" },
      "hooks": { "supported": true, "paths": ["~/.gemini/settings.json", ".gemini/settings.json"], "events": ["BeforeTool", "AfterTool"] },
      "toolApproval": { "model": "trust_flag", "key": "trust" },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://geminicli.com/docs/tools/mcp-server/", "https://geminicli.com/docs/hooks/"]
    },
    {
      "id": "antigravity",
      "displayName": "Antigravity",
      "config": { "paths": ["~/.gemini/antigravity/mcp_config.json"], "rootKey": "mcpServers" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "prompted_or_policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://antigravity.google/docs/mcp"]
    },
    {
      "id": "codex",
      "displayName": "Codex CLI",
      "config": { "paths": ["~/.codex/config.toml", ".codex/config.toml"], "rootKey": "mcp_servers" },
      "hooks": { "supported": true, "paths": ["~/.codex/hooks.json"], "events": ["PreToolUse", "SessionStart"] },
      "toolApproval": { "model": "policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://developers.openai.com/codex/mcp", "https://developers.openai.com/codex/hooks"]
    },
    {
      "id": "grok",
      "displayName": "Grok",
      "config": { "paths": ["~/.grok/config.toml", ".grok/config.toml"], "rootKey": "mcp_servers" },
      "hooks": { "supported": true, "paths": ["~/.grok/hooks/*.json"], "events": ["PreToolUse", "SessionStart", "PreCompact"] },
      "toolApproval": { "model": "policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://x.ai/cli"]
    },
    {
      "id": "trae",
      "displayName": "Trae",
      "config": { "paths": ["~/.trae/mcp.json", ".trae/mcp.json"], "rootKey": "mcpServers" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "prompted_or_policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://docs.trae.ai/ide/add-mcp-servers", "https://docs.trae.ai/ide/model-context-protocol"]
    },
    {
      "id": "qwen-code",
      "displayName": "Qwen Code",
      "config": { "paths": ["~/.qwen/settings.json", ".qwen/settings.json"], "rootKey": "mcpServers" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "prompted_or_policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://qwenlm.github.io/qwen-code-docs/en/users/configuration/settings/"]
    },
    {
      "id": "verdent",
      "displayName": "Verdent",
      "config": { "paths": ["~/.verdent/mcp.json"], "rootKey": "mcpServers" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "prompted_or_policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://docs.verdent.ai/verdent-for-vscode/advanced-features/mcp"]
    },
    {
      "id": "pi",
      "displayName": "Pi Coding Agent",
      "config": { "paths": ["~/.pi/agent/mcp.json", ".pi/mcp.json"], "rootKey": "mcpServers" },
      "hooks": { "supported": false, "paths": [], "events": [] },
      "toolApproval": { "model": "policy", "key": null },
      "instructionLimits": { "mcpServerInstructionsMaxChars": null },
      "sources": ["https://github.com/nicobailon/pi-mcp-adapter", "https://pi.dev/packages"]
    }
  ]
}
```
<!-- /leanctx-client-constraints-v1-json -->

## MCP Capability Matrix

| Client | Resources | Prompts | Elicitation | Sampling | Dynamic Tools | Max Tools | Tier |
|--------|-----------|---------|-------------|----------|---------------|-----------|------|
| Cursor | yes | yes | yes | no | yes | - | 1 |
| Claude Code | yes | yes | yes | yes | yes | - | 1 |
| Kiro | yes | yes | yes | no | yes | - | 1 |
| VS Code Copilot | yes | yes | no | no | yes | - | 2 |
| Zed | no | yes | no | no | yes | - | 2 |
| Codex | yes | no | no | no | yes | - | 2 |
| Windsurf | no | no | no | no | yes | 100 | 3 |
| Antigravity | no | no | no | no | no | - | 4 |
| Gemini CLI | no | no | no | no | no | - | 4 |

## Human‑readable notes

- **Do not guess formats**: every entry must have at least one vendor doc source.
- **No destructive writes**: installers must be merge‑based and keep other plugins/config intact.
- **Tokens/headers**: never hardcode or print secrets; prefer env indirection or client-native secret inputs.
