// SPDX-License-Identifier: Apache-2.0
//! Real tool hooks for Kiro and Cline (enforcement only: deny a secret-like
//! read or a shell allowlist violation, never approve or rewrite).
//!
//! - Kiro: one standalone hook file `~/.kiro/hooks/lean-ctx.json` in the
//!   V3 / IDE 1.0 format (`{"version": "v1", "hooks": [...]}`, trigger
//!   `PreToolUse`, action `command`), kiro.dev/docs/hooks and
//!   kiro.dev/docs/cli/v3/hooks-migration. The file is LeanCTX's own, so it is
//!   written and removed whole.
//! - Cline: the executable `~/.cline/hooks/PreToolUse` (one script per event,
//!   named after it), read by the Cline CLI and SDK. A `PreToolUse` the
//!   person wrote is never overwritten. macOS and Linux only (Cline does not
//!   run hooks on Windows).

use std::path::{Path, PathBuf};

/// Marks the Cline hook script as LeanCTX's own.
const CLINE_MARKER: &str = "# lean-ctx: cline-enforce";

pub(crate) fn kiro_hook_path(home: &Path) -> PathBuf {
    home.join(".kiro/hooks/lean-ctx.json")
}

pub(crate) fn cline_hook_path(home: &Path) -> PathBuf {
    home.join(".cline/hooks/PreToolUse")
}

/// The Kiro hook file for `binary`.
pub(crate) fn kiro_hook_body(binary: &str) -> String {
    let doc = serde_json::json!({
        "version": "v1",
        "hooks": [{
            "name": "lean-ctx-enforce",
            "trigger": "PreToolUse",
            "matcher": "*",
            "action": {
                "type": "command",
                "command": format!("{} hook kiro-enforce", quote(binary)),
            },
            "timeout": 30,
            "enabled": true
        }]
    });
    let mut text = serde_json::to_string_pretty(&doc).unwrap_or_default();
    text.push('\n');
    text
}

/// The Cline hook script for `binary`.
pub(crate) fn cline_hook_body(binary: &str) -> String {
    format!(
        "#!/bin/sh\n{CLINE_MARKER}\nexec {} hook cline-enforce\n",
        quote(binary)
    )
}

/// POSIX quoting: bare only when every byte is in the safe set (as
/// `shell::quote_posix`), else single-quoted with `'` escaped — so `;`, `&`,
/// `|`, `<`, `>`, `(`, `)`, globs and newlines never reach the shell bare.
pub(crate) fn quote(binary: &str) -> String {
    if !binary.is_empty()
        && binary
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+%^".contains(&b))
    {
        return binary.to_string();
    }
    format!("'{}'", binary.replace('\'', "'\\''"))
}

/// Write (or refresh) the Kiro hook file.
pub(crate) fn install_kiro_runtime_hook(home: &Path) {
    let binary = super::super::resolve_hook_command_binary();
    let path = kiro_hook_path(home);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    super::super::write_file(&path, &kiro_hook_body(&binary));
}

/// Write (or refresh) the Cline hook script unless the person has their own.
pub(crate) fn install_cline_runtime_hook(home: &Path) {
    if cfg!(windows) {
        return;
    }
    let path = cline_hook_path(home);
    if path.exists() && !is_own_cline_hook(&path) {
        tracing::warn!(
            "Cline hook {} is not LeanCTX's, left untouched",
            path.display()
        );
        return;
    }
    let binary = super::super::resolve_hook_command_binary();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    super::super::write_file(&path, &cline_hook_body(&binary));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
}

fn is_own_cline_hook(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| text.contains(CLINE_MARKER))
}

/// LeanCTX's Kiro hook file exists.
pub fn kiro_hook_installed(home: &Path) -> bool {
    std::fs::read_to_string(kiro_hook_path(home)).is_ok_and(|t| t.contains("kiro-enforce"))
}

/// LeanCTX's Cline hook script exists.
pub fn cline_hook_installed(home: &Path) -> bool {
    is_own_cline_hook(&cline_hook_path(home))
}

/// Remove LeanCTX's Kiro and Cline hooks; `true` when one was removed.
pub fn remove_kiro_cline_hooks(home: &Path) -> bool {
    let mut removed = false;
    if kiro_hook_installed(home) {
        removed |= std::fs::remove_file(kiro_hook_path(home)).is_ok();
    }
    if cline_hook_installed(home) {
        removed |= std::fs::remove_file(cline_hook_path(home)).is_ok();
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kiro_and_cline_hooks_install_detect_and_never_replace_a_foreign_script() {
        let env = super::super::governor_hosts::test_support::TestEnv::new();
        let home = env.path();

        install_kiro_runtime_hook(home);
        let kiro: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(kiro_hook_path(home)).unwrap()).unwrap();
        let hook = &kiro["hooks"][0];
        assert_eq!(kiro["version"], "v1");
        assert_eq!(hook["trigger"], "PreToolUse");
        assert_eq!(hook["action"]["type"], "command");
        assert_eq!(hook["action"]["command"], "lean-ctx hook kiro-enforce");
        assert!(kiro_hook_installed(home));

        if cfg!(unix) {
            std::fs::create_dir_all(home.join(".cline/hooks")).unwrap();
            std::fs::write(cline_hook_path(home), "#!/bin/sh\necho mine\n").unwrap();
            install_cline_runtime_hook(home);
            assert!(!cline_hook_installed(home), "a person's own hook stays");
            std::fs::remove_file(cline_hook_path(home)).unwrap();
            install_cline_runtime_hook(home);
            let script = std::fs::read_to_string(cline_hook_path(home)).unwrap();
            assert!(script.starts_with("#!/bin/sh\n"));
            assert!(script.contains("exec lean-ctx hook cline-enforce"));
            assert!(cline_hook_installed(home));
        }

        assert_eq!(quote("/opt/bin/lean-ctx"), "/opt/bin/lean-ctx");
        for hostile in [
            "/a b/lean-ctx",
            "/x;rm -rf ~",
            "/x&y",
            "/x|y",
            "/x>y",
            "/x(y)",
            "/x*",
            "/x\ny",
        ] {
            let q = quote(hostile);
            assert!(
                q.starts_with('\'') && q.ends_with('\''),
                "{hostile:?} → {q}"
            );
        }
        assert_eq!(quote("/it's"), "'/it'\\''s'");

        assert!(remove_kiro_cline_hooks(home));
        assert!(!kiro_hook_installed(home) && !cline_hook_installed(home));
    }
}
