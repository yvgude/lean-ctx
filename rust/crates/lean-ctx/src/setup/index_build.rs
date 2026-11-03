// SPDX-License-Identifier: Apache-2.0
use lean_ctx::core::portable_binary::resolve_portable_binary;

/// #356: decide whether a setup refresh may auto-index `cwd`. Returns `false`
/// for any cwd inside a macOS TCC-protected home dir (`~/Documents`, `~/Desktop`,
/// `~/Downloads`) so a `lean-ctx update` run from a project there never stats
/// marker files in it. That stat pops the macOS privacy prompt when lean-ctx is
/// its own TCC responsible process, and a maintenance refresh has no need to
/// trigger it — the graph builds on the next real tool use anyway. On non-macOS
/// hosts `is_under_tcc_protected_dir` is always `false`, so behaviour is
/// unchanged.
pub(crate) fn may_autoindex_cwd(cwd: &std::path::Path) -> bool {
    !lean_ctx::core::pathutil::is_under_tcc_protected_dir(cwd)
}

pub(crate) fn spawn_index_build_background(root: &std::path::Path) {
    if std::env::var("LEAN_CTX_DISABLED").is_ok()
        || matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1")
    {
        return;
    }
    let root_str = lean_ctx::core::graph_index::normalize_project_root(&root.to_string_lossy());
    if !lean_ctx::core::graph_index::is_safe_scan_root_public(&root_str) {
        tracing::info!("[setup: skipping background graph build for unsafe root {root_str}]");
        return;
    }

    let binary = resolve_portable_binary();

    #[cfg(unix)]
    {
        let mut cmd = std::process::Command::new("nice");
        cmd.args(["-n", "19"]);
        if which_ionice_available() {
            cmd.arg("ionice").args(["-c", "3"]);
        }
        cmd.arg(&binary)
            .args(["index", "build", "--root"])
            .arg(root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null());
        let _ = cmd.spawn();
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new(&binary)
            .args(["index", "build", "--root"])
            .arg(root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
            .spawn();
    }
}

#[cfg(unix)]
fn which_ionice_available() -> bool {
    std::process::Command::new("ionice")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use lean_ctx::core::editor_registry::ConfigType;

    use super::super::mcp::agent_mcp_targets;
    use super::may_autoindex_cwd;

    // #356: a setup refresh (e.g. via `lean-ctx update`) must not auto-index a
    // cwd inside a TCC-protected home dir, or it stats marker files there and
    // pops the macOS privacy prompt. Projects elsewhere index normally.
    #[test]
    fn may_autoindex_cwd_skips_tcc_protected_dirs() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        assert!(!may_autoindex_cwd(&home.join("Documents/proj")));
        assert!(!may_autoindex_cwd(&home.join("Desktop/proj")));
        assert!(!may_autoindex_cwd(&home.join("Downloads/proj")));
        assert!(may_autoindex_cwd(&home.join("code/proj")));
        assert!(may_autoindex_cwd(std::path::Path::new("/tmp/proj")));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn qoder_agent_targets_include_all_macos_mcp_locations() {
        let home = std::path::Path::new("/Users/tester");
        let targets = agent_mcp_targets("qoder", home).unwrap();
        let paths: Vec<_> = targets.iter().map(|t| t.config_path.as_path()).collect();

        assert_eq!(
            paths,
            vec![
                home.join(".qoder/mcp.json").as_path(),
                home.join("Library/Application Support/Qoder/User/mcp.json")
                    .as_path(),
                home.join("Library/Application Support/Qoder/SharedClientCache/mcp.json")
                    .as_path(),
            ]
        );
        assert!(
            targets
                .iter()
                .all(|t| t.config_type == ConfigType::QoderSettings)
        );
    }
}
