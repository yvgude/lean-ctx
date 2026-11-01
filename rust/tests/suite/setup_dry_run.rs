// SPDX-License-Identifier: Apache-2.0

//! `--dry-run` writes nothing: `setup` and `onboard` plans run in their own
//! process against a sandbox HOME whose lean-ctx config/data/state/cache dirs
//! all live inside it, so the whole tree is compared byte for byte. A
//! separate process keeps other tests' background writers out of the
//! measurement (an in-process test shares the process-wide data-dir env).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.insert(path.clone(), None);
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                files.insert(path, Some(bytes));
            }
        }
    }
    files
}

fn run(bin: &str, home: &Path, args: &[&str]) -> Output {
    let bin_dir = Path::new(bin).parent().unwrap();
    Command::new(bin)
        .args(args)
        .current_dir(home)
        .env_clear()
        .env("HOME", home)
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", bin_dir.to_string_lossy()),
        )
        .env("LEAN_CTX_CONFIG_DIR", home.join("lc/config"))
        .env("LEAN_CTX_DATA_DIR", home.join("lc/data"))
        .env("LEAN_CTX_STATE_DIR", home.join("lc/state"))
        .env("LEAN_CTX_CACHE_DIR", home.join("lc/cache"))
        .env("SHELL", "/bin/bash")
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD", "0")
        .output()
        .expect("lean-ctx spawn")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
#[cfg_attr(
    windows,
    ignore = "HOME-override isolation is Unix-only (dirs::home_dir uses the Win32 API)"
)]
fn setup_and_onboard_dry_runs_write_nothing() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".cursor")).unwrap();
    std::fs::create_dir_all(home.join(".codeium/windsurf")).unwrap();
    std::fs::write(home.join(".bashrc"), "# mine\n").unwrap();

    // The first lean-ctx command of a fresh install records one-time
    // config-load markers in its own dirs; take them before the baseline.
    let warm = run(bin, &home, &["setup", "--dry-run", "--agents", "cursor"]);
    assert!(
        warm.status.success(),
        "{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    assert!(
        !home.join(".cursor/mcp.json").exists(),
        "a dry run must not register the MCP server"
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".bashrc")).unwrap(),
        "# mine\n",
        "a dry run must not touch shell rc files"
    );

    let before = snapshot(&home);

    let setup = run(bin, &home, &["setup", "--dry-run", "--agents", "cursor"]);
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let plan = stdout(&setup);
    assert!(plan.contains("Dry run"), "{plan}");
    assert!(
        plan.contains("would_create") && plan.contains("Cursor"),
        "{plan}"
    );
    assert!(
        plan.contains("deselected") && plan.contains("Windsurf"),
        "{plan}"
    );
    assert!(plan.contains("would_save"), "{plan}");

    let json = run(
        bin,
        &home,
        &["setup", "--dry-run", "--json", "--agents", "cursor"],
    );
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json report");
    assert!(report["steps"].is_array(), "{report}");

    let onboard = run(bin, &home, &["onboard", "--dry-run"]);
    assert!(
        onboard.status.success(),
        "{}",
        String::from_utf8_lossy(&onboard.stderr)
    );

    assert_eq!(
        snapshot(&home),
        before,
        "setup/onboard --dry-run changed files under the sandbox HOME"
    );
}

#[test]
#[cfg_attr(
    windows,
    ignore = "HOME-override isolation is Unix-only (dirs::home_dir uses the Win32 API)"
)]
fn init_agent_dry_run_writes_nothing() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();

    let warm = run(bin, &home, &["init", "--agent", "claude", "--dry-run"]);
    assert!(
        warm.status.success(),
        "{}",
        String::from_utf8_lossy(&warm.stderr)
    );
    let before = snapshot(&home);
    let plan = run(bin, &home, &["init", "--agent", "claude", "--dry-run"]);
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    assert!(stdout(&plan).contains("Would configure: claude"));
    assert!(!home.join(".claude/settings.json").exists());
    assert!(!home.join("AGENTS.md").exists());
    assert_eq!(before, snapshot(&home));
}
