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
    OpenCode,
    Crush,
}
