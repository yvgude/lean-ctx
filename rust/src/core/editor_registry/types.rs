use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct EditorTarget {
    pub name: &'static str,
    pub agent_key: String,
    pub config_path: PathBuf,
    pub detect_path: PathBuf,
    pub config_type: ConfigType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigType {
    McpJson,
    Zed,
    Codex,
    VsCodeMcp,
    CopilotCli,
    OpenCode,
    Crush,
    JetBrains,
    Amp,
    HermesYaml,
    GeminiSettings,
    QoderSettings,
    /// Augment VS Code extension: top-level JSON array of server entries
    /// stored under augment.vscode-augment globalStorage. Each entry carries
    /// `type`, `id`, `name`, `disabled`, `command`, `args`, `env`.
    AugmentVsCode,
    /// OpenClaw (`~/.openclaw/openclaw.json`): nested `mcp.servers` since
    /// 2026.6.1 (strict schema validation rejects top-level `mcpServers`).
    /// Older versions use the legacy camelCase key — the writer detects the
    /// version via `meta.lastTouchedVersion` and migrates (GitHub #390).
    OpenClaw,
    /// Mistral Vibe (`~/.vibe/config.toml`): TOML-based config with `[[mcp_servers]]` array
    VibeToml,
    /// Command Code (`~/.commandcode/mcp.json`): `mcpServers` root with a
    /// custom entry schema (`transport`/`enabled`/`command`/`instructions`).
    CommandCode,
    /// Cline CLI (`~/.cline/data/settings/cline_mcp_settings.json`, unified
    /// across the Cline IDE extension/CLI/SDK since 2026.7): `mcpServers`
    /// root where each entry nests `command`/`args`/`env` under a
    /// `transport` object (`{"type": "stdio", ...}`), unlike the flat
    /// `command`/`args`/`env` shape the VS Code extension's own
    /// `cline_mcp_settings.json` (under globalStorage, see `ConfigType::McpJson`
    /// via `cline_mcp_path`) and most other MCP-JSON agents use.
    ClineCli,
}
