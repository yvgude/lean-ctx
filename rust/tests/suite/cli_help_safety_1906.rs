// SPDX-License-Identifier: Apache-2.0
//! #1906: `lean-ctx <command> --help` (and a lone `-h`) must describe the
//! command and change nothing. Before the dispatcher guard, `secure --help`
//! rewrote the config, `proof --help` wrote proof artifacts, `skillify --help`
//! generated rules and `grep --help` searched for the string "--help".
//!
//! Every top-level dispatcher arm runs against the real binary in an isolated
//! HOME / data dir / cwd, so a regressed guard only writes into a tempdir,
//! where the test sees it. Generalises the `init`-only check of #1849.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Arms the guard deliberately leaves alone: passthroughs whose arguments
/// belong to the wrapped command, the help/version entry points, the
/// completion engine and removed stubs (mirrors `EXEMPT` in `cmd_help.rs`).
const NOT_SWEPT: &[&str] = &[
    "-c",
    "exec",
    "-t",
    "--track",
    "raw",
    "bypass",
    "help",
    "--help",
    "-h",
    "--version",
    "-V",
    "__complete",
    "wrapped",
    "plugin",
    "plugins",
    "buddy",
    "pet",
    "watch",
];

/// Every name of every `"name" | "alias" =>` arm of the top-level dispatcher.
fn dispatcher_names() -> Vec<String> {
    let src = include_str!("../../src/cli/dispatch/mod.rs");
    let start = src
        .find("        match args[1].as_str() {")
        .expect("dispatcher match");
    let mut names = Vec::new();
    for line in src[start..].lines().skip(1) {
        if line.starts_with("        }") {
            break;
        }
        let Some(head) = line.strip_prefix("            \"") else {
            continue;
        };
        let Some((arm, _)) = head.split_once("=>") else {
            continue;
        };
        names.extend(
            format!("\"{arm}")
                .split('|')
                .map(|n| n.trim().trim_matches('"').to_string()),
        );
    }
    names
}

struct Sandbox {
    root: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        for dir in ["home", "data", "work"] {
            std::fs::create_dir_all(root.path().join(dir)).expect("sandbox dir");
        }
        Self { root }
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn run(&self, cmd: &str, flag: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
            .arg(cmd)
            .arg(flag)
            .current_dir(self.dir("work"))
            .env("HOME", self.dir("home"))
            .env("LEAN_CTX_DATA_DIR", self.dir("data"))
            .env("LEAN_CTX_HOOK_CHILD", "1")
            .env("LEAN_CTX_NO_UPDATE_CHECK", "1")
            .env("SHELL", "/bin/zsh")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("ZDOTDIR")
            .output()
            .expect("spawn lean-ctx")
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

fn sweep(flag: &str) {
    let names = dispatcher_names();
    assert!(names.len() > 150, "parser found only {} names", names.len());
    let mut failures = Vec::new();
    for name in names.iter().filter(|n| !NOT_SWEPT.contains(&n.as_str())) {
        let sandbox = Sandbox::new();
        let out = sandbox.run(name, flag);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let written: Vec<PathBuf> = ["home", "data", "work"]
            .iter()
            .flat_map(|d| files_under(&sandbox.dir(d)))
            .collect();
        if !out.status.success() {
            failures.push(format!("`{name} {flag}` exited {}", out.status));
        } else if stdout.trim().is_empty() {
            failures.push(format!("`{name} {flag}` printed no usage"));
        } else if !written.is_empty() {
            failures.push(format!("`{name} {flag}` wrote files: {written:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_command_long_help_is_side_effect_free() {
    sweep("--help");
}

#[test]
fn every_command_short_help_is_side_effect_free() {
    sweep("-h");
}

#[test]
fn guarded_command_prints_reference_usage_instead_of_running() {
    let sandbox = Sandbox::new();
    let out = sandbox.run("secure", "--help");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", out.status);
    assert!(
        stdout.starts_with("Usage (lean-ctx secure):"),
        "stdout:\n{stdout}"
    );
    assert!(files_under(&sandbox.dir("home")).is_empty());
}

#[test]
fn grep_help_describes_grep_instead_of_searching_for_it() {
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.dir("work").join("a.txt"), "--help\n").expect("seed");
    let out = sandbox.run("grep", "--help");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Usage (lean-ctx grep):"),
        "stdout:\n{stdout}"
    );
    assert!(!stdout.contains("a.txt"), "grep searched: {stdout}");
}
