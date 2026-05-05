mod amp;
mod claude;
mod cline;
mod codex;
mod copilot;
mod crush;
mod cursor;
mod gemini;
mod hermes;
mod jetbrains;
mod kiro;
mod opencode;
mod pi;
mod qoder;
mod shared;
mod windsurf;

pub(super) use amp::install_amp_hook;
pub(super) use claude::{
    install_claude_hook, install_claude_hook_config, install_claude_hook_scripts,
    install_claude_project_hooks,
};
pub(super) use cline::install_cline_rules;
pub use codex::install_codex_hook;
pub(super) use copilot::install_copilot_hook;
pub(super) use crush::install_crush_hook;
pub use cursor::install_cursor_hook;
pub(super) use cursor::{install_cursor_hook_config, install_cursor_hook_scripts};
pub(super) use gemini::{
    install_gemini_hook, install_gemini_hook_config, install_gemini_hook_scripts,
};
pub(super) use hermes::install_hermes_hook;
pub(super) use jetbrains::install_jetbrains_hook;
pub(super) use kiro::install_kiro_hook;
pub(super) use opencode::install_opencode_hook;
pub(super) use pi::install_pi_hook;
pub use qoder::install_qoder_hook;
pub(super) use windsurf::install_windsurf_rules;
