//! #281: `[setup] auto_update_mcp = false` must keep the lean-ctx MCP server out
//! of every agent/editor config, while hooks, rules and skills still install.
//!
//! These run the real binary as a subprocess so `Config::load()` reads the temp
//! config dir fresh (no in-process cache to fight). Every lean-ctx directory is
//! redirected into the tempdir so nothing touches the developer's real machine,
//! and `LEAN_CTX_HOOK_CHILD=1` suppresses daemon auto-start.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A fully isolated lean-ctx environment rooted in a tempdir.
struct Sandbox {
    _root: tempfile::TempDir,
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
    state: PathBuf,
    cache: PathBuf,
}

impl Sandbox {
    /// `config_body` is written verbatim to `config.toml`; pass `""` for defaults
    /// (which means `auto_update_mcp = true`).
    fn new(config_body: &str) -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let config = root.path().join("config");
        let data = root.path().join("data");
        let state = root.path().join("state");
        let cache = root.path().join("cache");
        for d in [&home, &config, &data, &state, &cache] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(config.join("config.toml"), config_body).unwrap();
        Self {
            _root: root,
            home,
            config,
            data,
            state,
            cache,
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
        cmd.env("HOME", &self.home)
            .env("LEAN_CTX_CONFIG_DIR", &self.config)
            .env("LEAN_CTX_DATA_DIR", &self.data)
            .env("LEAN_CTX_STATE_DIR", &self.state)
            .env("LEAN_CTX_CACHE_DIR", &self.cache)
            // Never talk to (or start) the developer's daemon.
            .env("LEAN_CTX_HOOK_CHILD", "1")
            .env("LEAN_CTX_QUIET", "1");
        cmd
    }

    fn jb_snippet(&self) -> PathBuf {
        self.home.join(".jb-mcp.json")
    }
}

fn assert_success(out: &Output, ctx: &str) {
    assert!(
        out.status.success(),
        "{ctx} failed: {}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `init --agent jetbrains` drives the hooks-layer MCP writer
/// (`install_jetbrains_hook`) — the path that bypassed the first #281 fix.
fn run_init_jetbrains(sandbox: &Sandbox) -> Output {
    sandbox
        .command()
        .args(["init", "--agent", "jetbrains", "--global"])
        .output()
        .expect("spawn lean-ctx init --agent jetbrains")
}

#[test]
fn init_agent_skips_mcp_write_when_opted_out() {
    let sandbox = Sandbox::new("[setup]\nauto_update_mcp = false\n");
    let out = run_init_jetbrains(&sandbox);
    assert_success(&out, "init --agent jetbrains (opted out)");

    assert!(
        !sandbox.jb_snippet().exists(),
        "auto_update_mcp=false must not write the JetBrains MCP snippet (~/.jb-mcp.json)"
    );
}

#[test]
fn init_agent_writes_mcp_by_default() {
    // The positive control: with the default flag the writer must still run, so
    // the opt-out test above is proving a real difference rather than a no-op.
    let sandbox = Sandbox::new("");
    let out = run_init_jetbrains(&sandbox);
    assert_success(&out, "init --agent jetbrains (default)");

    assert!(
        sandbox.jb_snippet().exists(),
        "default config must write the JetBrains MCP snippet"
    );
}

#[test]
fn doctor_fix_skips_mcp_step_when_opted_out() {
    let sandbox = Sandbox::new("[setup]\nauto_update_mcp = false\n");
    let out = sandbox
        .command()
        .args(["doctor", "--fix", "--json"])
        .output()
        .expect("spawn lean-ctx doctor --fix --json");

    // `doctor --fix` can exit non-zero in a bare sandbox (not every health check
    // passes there), but it still emits the JSON report — and the mcp_config step
    // must report the opt-out instead of registering anything.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("MCP registration skipped (auto_update_mcp=false)"),
        "doctor --fix must report the MCP opt-out in its mcp_config step; got:\n{stdout}"
    );
}

/// Sanity: opting out must not leave any `mcp.json`-style file under HOME.
#[test]
fn opt_out_leaves_no_mcp_files_under_home() {
    let sandbox = Sandbox::new("[setup]\nauto_update_mcp = false\n");
    assert_success(&run_init_jetbrains(&sandbox), "init --agent jetbrains");

    let leaked = find_mcp_files(&sandbox.home);
    assert!(
        leaked.is_empty(),
        "auto_update_mcp=false must not write any MCP config files; found: {leaked:?}"
    );
}

/// Collect any file whose name signals an MCP registration lean-ctx might write.
fn find_mcp_files(root: &Path) -> Vec<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut hits = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let is_mcp_file = name == "mcp.json" || name == ".jb-mcp.json";
            if is_mcp_file && std::fs::read_to_string(&path).is_ok_and(|c| c.contains("lean-ctx")) {
                hits.push(path);
            }
        }
    }
    hits
}
