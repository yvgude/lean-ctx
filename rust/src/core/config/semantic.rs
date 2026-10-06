// SPDX-License-Identifier: Apache-2.0
//! Semantic code-intelligence mode (`semantic_mode` in `config.toml`).

use serde::{Deserialize, Serialize};

use super::Config;

/// How the code graph uses local semantic backends (language servers, a live
/// JetBrains IDE) on top of the always-on tree-sitter structure.
/// - `off`: structural only; no semantic backend is ever queried.
/// - `auto`: (Default) background enrichment uses only backends already
///   running — a warm language server or a live IDE — and never spawns one.
/// - `eager`: background enrichment may start the project's language servers.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SemanticMode {
    Off,
    #[default]
    Auto,
    Eager,
}

impl SemanticMode {
    pub fn from_env() -> Option<Self> {
        std::env::var("LEAN_CTX_SEMANTIC_MODE").ok().and_then(|v| {
            match v.trim().to_lowercase().as_str() {
                "off" => Some(Self::Off),
                "auto" => Some(Self::Auto),
                "eager" => Some(Self::Eager),
                _ => None,
            }
        })
    }

    pub fn effective(config: &Config) -> Self {
        Self::from_env().unwrap_or(config.semantic_mode)
    }

    /// Effective mode for `project_root` specifically (its own, trust-gated
    /// `.lean-ctx.toml`), independent of the process's working directory.
    ///
    /// `eager` lets background work start the project's language servers,
    /// which execute project code (build scripts, proc macros). Whatever
    /// requested it — global config, `LEAN_CTX_SEMANTIC_MODE`, or the
    /// project — it applies only to a trusted workspace (`lean-ctx trust`);
    /// an untrusted one runs as `auto`. Explicit tools such as `ctx_refactor`
    /// are unaffected.
    pub fn for_project(project_root: &str) -> Self {
        let mode = Self::from_env()
            .unwrap_or_else(|| Config::load_for_project_root(project_root).semantic_mode);
        if mode == Self::Eager
            && !crate::core::workspace_trust::is_trusted(std::path::Path::new(project_root))
        {
            return Self::Auto;
        }
        mode
    }
}
