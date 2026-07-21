//! End-to-end regression for GitHub #596:
//! "installer fails on symlinked claude and codex folders".
//!
//! Dotfiles users symlink their agent config (`~/.claude.json`,
//! `~/.codex/config.toml`, …) into a managed repo. The `[Critical] symlink
//! hijack protection` added to `config_io::write_atomic` then hard-blocked
//! every write *through* such a symlink, so `lean-ctx init`/`setup` could no
//! longer register the MCP server or write agent config.
//!
//! lean-ctx now writes THROUGH a user-managed symlink to its real target
//! (as long as the target stays within `$HOME`), preserving the symlink. These
//! tests pin that behaviour with real symlinks against an isolated `$HOME`.
//!
//! Unix-only: `$HOME`-override + POSIX symlinks. On Windows `dirs::home_dir()`
//! ignores the env var and junction semantics differ.
#![cfg(unix)]

use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

fn write_exe(path: &Path, content: &str) {
    std::fs::write(path, content).expect("write fake binary");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

struct Sandbox {
    _tmp: tempfile::TempDir,
    home: std::path::PathBuf,
    data: std::path::PathBuf,
    dotfiles: std::path::PathBuf,
    bin_dir: std::path::PathBuf,
}

fn sandbox() -> Sandbox {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = tmp.path().join("data");
    let bin_dir = tmp.path().join("bin");
    // The user's managed dotfiles repo — kept *inside* $HOME so the home-only
    // symlink-follow guard permits writing through to it.
    let dotfiles = home.join("dotfiles");
    for d in [&home, &data, &bin_dir, &dotfiles] {
        std::fs::create_dir_all(d).unwrap();
    }
    Sandbox {
        _tmp: tmp,
        home,
        data,
        dotfiles,
        bin_dir,
    }
}

fn run_init(sb: &Sandbox, agent: &str) -> std::process::Output {
    let path = format!(
        "{}:{}",
        sb.bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .args(["init", "--agent", agent, "--global", "--mode", "mcp"])
        .env("HOME", &sb.home)
        .env("LEAN_CTX_DATA_DIR", &sb.data)
        .env("LEAN_CTX_ACTIVE", "1")
        .env("LEAN_CTX_DISABLED", "1")
        .env("SHELL", "/bin/bash")
        .env("PATH", &path)
        .output()
        .expect("spawn lean-ctx init")
}

/// `~/.claude.json` is a symlink into the user's dotfiles repo. The lean-ctx MCP
/// entry must be written THROUGH the symlink into the real file, and the symlink
/// must survive (not be replaced by a regular file).
#[test]
fn claude_json_symlink_is_written_through() {
    let sb = sandbox();

    let dotfile = sb.dotfiles.join("claude.json");
    std::fs::write(&dotfile, "{}\n").unwrap();
    let link = sb.home.join(".claude.json");
    symlink(&dotfile, &link).unwrap();

    // Fake `claude` that fails so lean-ctx falls back to the file-merge/write
    // path (where the symlink guard lives) instead of `claude mcp add-json`.
    write_exe(&sb.bin_dir.join("claude"), "#!/bin/sh\nexit 1\n");

    let out = run_init(&sb, "claude");
    assert!(
        out.status.success(),
        "init --agent claude exit; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let meta = std::fs::symlink_metadata(&link).unwrap();
    assert!(
        meta.file_type().is_symlink(),
        "~/.claude.json must remain a symlink (write-through, not replace)"
    );

    let written = std::fs::read_to_string(&dotfile).unwrap();
    assert!(
        written.contains("mcpServers") && written.contains("lean-ctx"),
        "lean-ctx MCP entry must be written THROUGH the symlink into the dotfile; got:\n{written}"
    );
}

/// `~/.codex/config.toml` is a symlink into the dotfiles repo (the classic
/// "symlinked codex folder" setup). lean-ctx must merge its MCP server into the
/// real target while preserving the user's existing content and the symlink.
#[test]
fn codex_config_symlink_is_written_through() {
    let sb = sandbox();

    // Real ~/.codex dir so Codex is detected; only the config file is symlinked.
    let codex_dir = sb.home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();

    let dotfile = sb.dotfiles.join("codex-config.toml");
    std::fs::write(&dotfile, "# user codex config\n").unwrap();
    let link = codex_dir.join("config.toml");
    symlink(&dotfile, &link).unwrap();

    // Fake `codex` binary so detection is robust regardless of host PATH.
    write_exe(&sb.bin_dir.join("codex"), "#!/bin/sh\nexit 0\n");

    let out = run_init(&sb, "codex");
    assert!(
        out.status.success(),
        "init --agent codex exit; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let meta = std::fs::symlink_metadata(&link).unwrap();
    assert!(
        meta.file_type().is_symlink(),
        "~/.codex/config.toml must remain a symlink (write-through, not replace)"
    );

    let written = std::fs::read_to_string(&dotfile).unwrap();
    assert!(
        written.contains("mcp_servers.lean-ctx") || written.contains("lean-ctx"),
        "lean-ctx MCP entry must be written THROUGH the symlink into the dotfile; got:\n{written}"
    );
}
