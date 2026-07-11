mod amp;
mod antigravity;
mod claude;
mod cline;
mod codebuddy;
mod codex;
mod copilot;
mod crush;
mod cursor;
mod gemini;
mod grok;
mod hermes;
mod jetbrains;
mod kiro;
mod openclaw;
mod opencode;
mod pi;
mod qoder;
mod shared;
mod vibe;
mod windsurf;

pub(super) use amp::install_amp_hook;
pub(crate) use antigravity::{
    antigravity_cli_config_dir, antigravity_cli_plugin_dir, uninstall_antigravity_cli_plugin,
};
pub(super) use antigravity::{install_antigravity_cli_hook, install_antigravity_hook};
pub(crate) use claude::CLAUDE_MD_BLOCK_START;
pub(super) use claude::{
    install_claude_hook_config, install_claude_hook_scripts, install_claude_hook_with_mode,
    install_claude_permissions_deny_replace, install_claude_project_hooks,
};
pub(super) use cline::install_cline_rules;
pub(crate) use codebuddy::CODEBUDDY_MD_BLOCK_START;
pub(super) use codebuddy::{
    install_codebuddy_hook_config, install_codebuddy_hook_scripts,
    install_codebuddy_hook_with_mode, install_codebuddy_permissions_deny_replace,
    install_codebuddy_project_hooks,
};
pub use codex::install_codex_hook;
pub(super) use copilot::install_copilot_hook;
pub(super) use crush::install_crush_hook_with_mode;
pub use cursor::install_cursor_hook;
pub(super) use cursor::{
    install_cursor_deny_hook, install_cursor_hook_config, install_cursor_hook_scripts,
    install_cursor_hook_with_mode,
};
pub(crate) use gemini::unregister_gemini_context_filename;
pub(super) use gemini::{
    install_gemini_deny_hook, install_gemini_hook, install_gemini_hook_config,
    install_gemini_hook_scripts,
};
pub(super) use grok::install_grok_mcp;
pub(super) use hermes::install_hermes_hook_with_mode;
pub(super) use jetbrains::install_jetbrains_hook;
pub(super) use kiro::install_kiro_hook;
pub(super) use openclaw::install_openclaw_hook;
pub(super) use opencode::install_opencode_hook_with_mode;
pub(crate) use opencode::unregister_opencode_instructions;
pub(super) use pi::install_pi_hook_with_mode;
pub(super) use qoder::{install_qoder_hook, install_qoder_hook_with_mode};
pub(super) use vibe::install_vibe_hook;
pub(super) use windsurf::{
    install_windsurf_hooks, install_windsurf_hooks_replace, install_windsurf_rules,
};
