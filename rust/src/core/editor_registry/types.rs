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
    /// Goose (`~/.config/goose/config.yaml`): a stdio entry under the
    /// top-level `extensions` map (`cmd`/`args`/`type: stdio`/`enabled`).
    GooseYaml,
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
    /// Oh My Pi (`~/.omp/agent/mcp.json`): native `mcpServers` root whose
    /// entries are plain stdio servers (`type`/`command`/`args`). OMP's schema
    /// has no `lifecycle` key — its client defers connecting on its own — so
    /// this writer must never emit one.
    OmpMcp,
    /// CodeWhale (`~/.codewhale/mcp.json`, legacy `~/.deepseek/mcp.json`):
    /// accepts either `servers` (upstream's preferred root) or `mcpServers`
    /// (cross-client compatibility root). The writer keeps whichever root the
    /// user's file already uses instead of adding a competing second one
    /// (GH #1402).
    CodeWhale,
    /// Meta Muse Code user settings (`$XDG_CONFIG_HOME/muse/settings.json`,
    /// default `~/.config/muse/settings.json`). The file requires
    /// `"schema_version": 1`; MCP servers live under canonical `mcpServers`
    /// (`type`/`command`/`args`) or the legacy `mcp_servers`
    /// (`transport`/`command`/`args`) spelling. Mixing the two disables MCP for
    /// the session, so the writer keeps whichever root the file already uses
    /// and never adds the other one (GH #2198).
    MuseSettings,
}
