// Auto-split from the former monolithic writers.rs. Grouped by operation
// (install/uninstall) + shared helpers; behavior is unchanged.

use super::types::{ConfigType, EditorTarget};

mod install;
mod shared;
mod uninstall;

pub use shared::auto_approve_tools;
pub use uninstall::remove_lean_ctx_mcp_server;
// Routers below dispatch to every install/uninstall writer; a glob keeps the
// dispatch table readable and lets the test module reach them via `super::*`.
#[allow(clippy::wildcard_imports)]
use install::*;
#[allow(clippy::wildcard_imports)]
use uninstall::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAction {
    Created,
    Updated,
    Already,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WriteOptions {
    pub overwrite_invalid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteResult {
    pub action: WriteAction,
    pub note: Option<String>,
}

pub fn write_config(target: &EditorTarget, binary: &str) -> Result<WriteResult, String> {
    write_config_with_options(target, binary, WriteOptions::default())
}

pub fn write_config_with_options(
    target: &EditorTarget,
    binary: &str,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    if let Some(parent) = target.config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    match target.config_type {
        ConfigType::McpJson => write_mcp_json(target, binary, opts),
        ConfigType::Zed => write_zed_config(target, binary, opts),
        ConfigType::Codex => write_codex_config(target, binary),
        ConfigType::VsCodeMcp => write_vscode_mcp(target, binary, opts),
        ConfigType::CopilotCli => write_copilot_cli(target, binary, opts),
        ConfigType::OpenCode => write_opencode_config(target, binary, opts),
        ConfigType::Crush => write_crush_config(target, binary, opts),
        ConfigType::JetBrains => write_jetbrains_config(target, binary, opts),
        ConfigType::Amp => write_amp_config(target, binary, opts),
        ConfigType::HermesYaml => write_hermes_yaml(target, binary, opts),
        ConfigType::GeminiSettings => write_gemini_settings(target, binary, opts),
        ConfigType::QoderSettings => write_qoder_settings(target, binary, opts),
        ConfigType::AugmentVsCode => write_augment_vscode(target, binary, opts),
        ConfigType::OpenClaw => write_openclaw_config(target, binary, opts),
        ConfigType::VibeToml => write_vibe_toml(target, binary, opts),
    }
}

pub fn remove_lean_ctx_server(
    target: &EditorTarget,
    opts: WriteOptions,
) -> Result<WriteResult, String> {
    match target.config_type {
        ConfigType::McpJson
        | ConfigType::JetBrains
        | ConfigType::GeminiSettings
        | ConfigType::QoderSettings => remove_lean_ctx_mcp_server(&target.config_path, opts),
        ConfigType::VsCodeMcp | ConfigType::CopilotCli => {
            remove_lean_ctx_vscode_server(&target.config_path, opts)
        }
        ConfigType::Codex => remove_lean_ctx_codex_server(&target.config_path),
        ConfigType::OpenCode | ConfigType::Crush => {
            remove_lean_ctx_named_json_server(&target.config_path, "mcp", opts)
        }
        ConfigType::Zed => {
            remove_lean_ctx_named_json_server(&target.config_path, "context_servers", opts)
        }
        ConfigType::Amp => remove_lean_ctx_amp_server(&target.config_path, opts),
        ConfigType::HermesYaml => remove_lean_ctx_hermes_yaml_server(&target.config_path),
        ConfigType::AugmentVsCode => {
            remove_lean_ctx_augment_vscode_server(&target.config_path, opts)
        }
        ConfigType::OpenClaw => remove_lean_ctx_openclaw_server(&target.config_path, opts),
        ConfigType::VibeToml => remove_lean_ctx_vibe_toml_server(&target.config_path, opts),
    }
}

#[cfg(test)]
mod tests;
