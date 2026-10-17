// SPDX-License-Identifier: Apache-2.0

//! What a `ctx_shell` command read or rewrote, fed to the bounce tracker.

#[allow(clippy::wildcard_imports)]
use super::*;

/// The directory a `ctx_shell` command starts in: its explicit `cwd`, else the
/// session's shell cwd, else the project root.
pub(super) async fn shell_command_base_dir(
    server: &LeanCtxServer,
    args: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<String> {
    if let Some(cwd) = helpers::get_str(args, "cwd").filter(|c| !c.trim().is_empty()) {
        return Some(cwd);
    }
    let session = server.session.read().await;
    session
        .shell_cwd
        .clone()
        .or_else(|| session.project_root.clone())
        .filter(|c| !c.trim().is_empty())
}

/// Feed a shell command's file reads and edits to the bounce tracker: a full
/// read (`cat`) can complete a correction of a compressed view, a partial one
/// (`head`, `sed -n`) never does, and an edit (`sed -i`, `>`, `tee`, `cp`/`mv`,
/// `apply_patch`) cancels the provisional correction of its file as
/// read-before-edit.
pub(super) fn record_shell_file_effects(command: &str, base: Option<&str>) {
    use crate::core::bounce_tracker::ReadView;
    use crate::core::shell_file_effects::ShellReadKind;
    let effects = crate::core::shell_file_effects::analyze(command);
    if effects.reads.is_empty() && effects.edits.is_empty() {
        return;
    }
    let resolve = |path: &str| -> String {
        match base {
            Some(dir) if !std::path::Path::new(path).is_absolute() && !path.starts_with('~') => {
                std::path::Path::new(dir)
                    .join(path)
                    .to_string_lossy()
                    .into_owned()
            }
            _ => path.to_string(),
        }
    };
    let Ok(mut bt) = crate::core::bounce_tracker::global().lock() else {
        return;
    };
    bt.next_seq();
    for (path, kind) in &effects.reads {
        let view = match kind {
            ShellReadKind::Full => ReadView::Full,
            ShellReadKind::Partial => ReadView::Partial,
        };
        bt.record_shell_read(&resolve(path), view);
    }
    for path in &effects.edits {
        bt.record_edit(&resolve(path));
    }
}
