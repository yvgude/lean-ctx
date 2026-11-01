// SPDX-License-Identifier: Apache-2.0

use super::{detect_project_root, detect_project_root_in, normalize_explicit_root};

/// AP04-G10: a pin from a user-global agent config does not take a
/// command run inside another real project away from that project.
#[test]
fn a_pin_does_not_override_the_project_the_command_runs_in() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (pinned, current) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for project in [&pinned, &current] {
        std::fs::create_dir(project.path().join(".git")).unwrap();
    }
    let _pin = crate::test_env::VarGuard::set("LEAN_CTX_PROJECT_ROOT", pinned.path());

    let root = detect_project_root_in(&[], Some(current.path()));

    assert_eq!(root, current.path().to_string_lossy());
}

/// AP04-G10 / S5: a command that writes project-scoped state refuses to
/// act for `~`, even when `~` is a dotfiles repository.
#[test]
fn a_project_scoped_command_refuses_the_home_directory() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let home = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    std::fs::create_dir_all(home.join(".git")).unwrap();
    std::fs::create_dir_all(home.join("notes")).unwrap();
    let _home = crate::test_env::VarGuard::set("HOME", &home);
    let _pin = crate::test_env::VarGuard::set("LEAN_CTX_PROJECT_ROOT", "");

    assert!(super::require_project_root_in(Some(&home)).is_err());
    assert_ne!(
        detect_project_root_in(&[], Some(&home.join("notes"))),
        home.to_string_lossy()
    );
}

#[test]
fn explicit_root_is_not_promoted_to_parent_git_root() {
    let args = vec![
        "build".to_string(),
        "--root".to_string(),
        "/home/example/travail".to_string(),
    ];

    assert_eq!(detect_project_root(&args), "/home/example/travail");
}

#[test]
fn explicit_root_expands_home_prefix() {
    let Some(home) = dirs::home_dir() else {
        return;
    };

    assert_eq!(
        normalize_explicit_root("~/travail"),
        home.join("travail").to_string_lossy().to_string()
    );
}
/// #1903: the CLI read boundary must match MCP `ctx_read` (same resolver,
/// same verdict, same error text).
#[cfg(test)]
mod jail_parity_tests {
    use super::super::jail_cli_read_path;
    use crate::core::path_resolve::resolve_tool_path;

    fn project() -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("inside.txt"), "in").unwrap();
        std::fs::write(tmp.path().join("outside.txt"), "out").unwrap();
        (tmp, root.to_string_lossy().into_owned())
    }

    // Parity holds in every build: with the `no-jail` feature both paths admit
    // the read, otherwise both refuse it with the same text.
    #[test]
    fn out_of_root_read_is_denied_like_mcp() {
        // Both paths render the effective config location into the denial, so
        // a concurrent test must not move LEAN_CTX_CONFIG_DIR between them.
        let _env = crate::core::data_dir::test_env_lock();
        let (_tmp, root) = project();
        for raw in [
            format!("{root}/../outside.txt"),
            format!("{root}/../../../../../../etc/hosts"),
        ] {
            let cli = jail_cli_read_path(&raw, &root);
            let mcp = resolve_tool_path(Some(&root), None, &raw);
            assert_eq!(cli, mcp, "CLI and MCP must decide {raw} alike");
            if !cfg!(feature = "no-jail") {
                let err = cli.expect_err("CLI must deny out-of-root");
                assert!(err.contains("path escapes project root"), "{err}");
            }
        }
    }

    #[test]
    fn in_root_read_resolves_like_mcp() {
        let (_tmp, root) = project();
        let raw = format!("{root}/inside.txt");
        let cli = jail_cli_read_path(&raw, &root).unwrap();
        assert_eq!(cli, resolve_tool_path(Some(&root), None, &raw).unwrap());
        assert_eq!(std::fs::read_to_string(&cli).unwrap(), "in");
    }

    #[test]
    fn broad_root_is_refused() {
        let Some(home) = dirs::home_dir() else { return };
        let home = home.to_string_lossy().into_owned();
        let err = jail_cli_read_path(&format!("{home}/.profile"), &home).unwrap_err();
        assert!(err.contains("no project detected"), "{err}");
    }

    #[test]
    fn secret_path_enforcement_matches_mcp() {
        let _env = crate::core::data_dir::test_env_lock();
        let (_tmp, root) = project();
        let secret = format!("{root}/.env");
        std::fs::write(&secret, "TOKEN=x").unwrap();
        let prev = std::env::var("LEAN_CTX_IO_BOUNDARY_MODE").ok();
        crate::test_env::set_var("LEAN_CTX_IO_BOUNDARY_MODE", "enforce");
        let cli = jail_cli_read_path(&secret, &root);
        let mcp = resolve_tool_path(Some(&root), None, &secret);
        match prev {
            Some(v) => crate::test_env::set_var("LEAN_CTX_IO_BOUNDARY_MODE", v),
            None => crate::test_env::remove_var("LEAN_CTX_IO_BOUNDARY_MODE"),
        }
        let cli = cli.expect_err("enforce mode must refuse a secret path");
        assert_eq!(cli, mcp.unwrap_err());
        assert!(cli.contains("Secret-like path"), "{cli}");
    }
}
