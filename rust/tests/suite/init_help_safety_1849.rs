//! #1849: `lean-ctx init --help` (and `-h`) must print usage and write
//! NOTHING. The `init` arm passed every argument straight to `cmd_init`,
//! which never looked for `--help`, so `init --agent claude --help` ran a real
//! init and wrote `~/.claude/CLAUDE.md` — the #476 defect, in another command.
//!
//! The real binary runs in an isolated HOME, so even a regressed guard only
//! writes into a tempdir, where the test sees it.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    _root: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let data = root.path().join("data");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&data).expect("data");
        Self {
            _root: root,
            home,
            data,
        }
    }

    fn run_init(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
            .arg("init")
            .args(args)
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("LEAN_CTX_DATA_DIR", &self.data)
            .env("LEAN_CTX_HOOK_CHILD", "1")
            .env("SHELL", "/bin/zsh")
            // Agent config locations that would otherwise point outside the
            // sandbox, at the developer's real setup.
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("ZDOTDIR")
            .output()
            .expect("spawn lean-ctx init")
    }
}

/// Every file under `dir`, relative to it.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(dir) {
                found.push(rel.to_path_buf());
            }
        }
    }
    found
}

fn assert_is_help_not_init(sandbox: &Sandbox, args: &[&str]) {
    let out = sandbox.run_init(args);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = args.join(" ");
    assert!(
        out.status.success(),
        "`init {line}` should exit 0; got {}\nstdout:\n{stdout}",
        out.status
    );
    assert!(
        stdout.contains("Usage: lean-ctx init") && stdout.contains("--agent"),
        "`init {line}` must print usage; stdout was:\n{stdout}"
    );
    let written = files_under(&sandbox.home);
    assert!(
        written.is_empty(),
        "`init {line}` wrote into HOME — it ran a real init: {written:?}"
    );
}

#[test]
fn init_agent_long_help_shows_usage_and_writes_nothing() {
    assert_is_help_not_init(&Sandbox::new(), &["--agent", "claude", "--help"]);
}

#[test]
fn init_global_short_help_shows_usage_and_writes_nothing() {
    assert_is_help_not_init(&Sandbox::new(), &["-h", "--global"]);
}
