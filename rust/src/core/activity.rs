// SPDX-License-Identifier: Apache-2.0
//! Facade of `lean-ctx-activity`: the per-session activity stream under
//! `<data dir>/activity`, with this Instance's secret redaction. Producers
//! (hooks, MCP calls, ACP client callbacks) call [`record`]; it never fails
//! the caller.

pub use lean_ctx_activity::*;

/// The store of this Instance.
pub fn store() -> Option<Store> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| Store::new(dir.join("activity")))
}

/// The project root a stream is kept under: normalized and promoted to its
/// git root, so a hook's `cwd` in a subdirectory, the MCP session's root and
/// a reader's `--project-root` name the same project.
pub fn project_root(path: &str) -> String {
    crate::core::command_root::promote_to_git_root(
        &crate::core::index_paths::normalize_project_root(path),
    )
}

/// Record `draft` now: secrets masked (`core::redaction`, the shell's
/// redaction), text bounded. Fail-open: an event that cannot be stored is
/// dropped with a debug log.
pub fn record(mut draft: Draft) {
    let Some(store) = store() else {
        return;
    };
    draft.project_root = project_root(&draft.project_root);
    if let Err(error) = store.append(
        draft,
        chrono::Utc::now(),
        &crate::core::redaction::redact_text,
    ) {
        tracing::debug!(%error, "lean-ctx: activity event dropped");
    }
}
