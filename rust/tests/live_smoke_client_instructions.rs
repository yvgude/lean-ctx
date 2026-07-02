//! Live client-wiring smoke (#578, Säule 1c).
//!
//! Boots the REAL binary end-to-end per client, in an isolated HOME:
//!   1. `lean-ctx init --agent <client>` installs MCP registration + rules file,
//!   2. `lean-ctx mcp` is spoken to over actual stdio JSON-RPC,
//!   3. the `initialize` response proves the cross-channel dedup contract:
//!      covered clients (Cursor mdc / Codex instructions.md) get the one-line
//!      SKELETON_ANCHOR, uncovered clients (Claude Code) get the full skeleton,
//!   4. `tools/list` proves the advertised surface is exactly the lazy core.
//!
//! This is the CI stand-in for a manual "open Cursor/Claude/Codex and check"
//! session — it exercises the same code paths the editors hit (per-client
//! instructions in `server_handler::initialize`, lazy tool visibility).

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// The one-line anchor `instructions.rs` emits for covered clients. Kept as a
/// distinctive prefix (not the full constant) so cosmetic rewording doesn't
/// break the smoke while the contract (anchor vs full skeleton) still holds.
const ANCHOR_PREFIX: &str = "lean-ctx active — your auto-loaded lean-ctx rules apply";
/// A line only the FULL canonical skeleton carries.
const SKELETON_MARKER: &str = "MANDATORY MAPPING";

struct TestEnv {
    _tmp: tempfile::TempDir,
    home: std::path::PathBuf,
    data: std::path::PathBuf,
    project: std::path::PathBuf,
}

fn test_env() -> TestEnv {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = tmp.path().join("data");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(project.join(".git")).unwrap();
    TestEnv {
        _tmp: tmp,
        home,
        data,
        project,
    }
}

fn base_cmd(bin: &str, env: &TestEnv) -> Command {
    let mut cmd = Command::new(bin);
    cmd.current_dir(&env.project)
        .env("HOME", &env.home)
        .env("LEAN_CTX_DATA_DIR", &env.data)
        .env("CODEX_HOME", env.home.join(".codex"))
        // Skip background maintenance (rules re-inject, version check) so the
        // server can't mutate coverage state mid-handshake.
        .env("LEAN_CTX_HEADLESS", "1");
    cmd
}

fn run_init(bin: &str, env: &TestEnv, agent: &str) {
    let out = base_cmd(bin, env)
        .args(["init", "--agent", agent, "--global", "--mode", "mcp"])
        .env("LEAN_CTX_ACTIVE", "1")
        .env("LEAN_CTX_DISABLED", "1")
        .output()
        .expect("init spawn");
    assert!(
        out.status.success(),
        "init --agent {agent} failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Speak minimal MCP over the child's stdio: initialize (with `client_name`),
/// then tools/list. Returns (instructions, advertised tool names).
fn mcp_handshake(bin: &str, env: &TestEnv, client_name: &str) -> (String, Vec<String>) {
    let mut child: Child = base_cmd(bin, env)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("mcp server spawn");

    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");

    // Reader thread: forward every stdout line; a deadline on the receiving
    // side keeps a wedged server from hanging the suite.
    let (tx, rx) = mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let recv_response = |id: u64| -> serde_json::Value {
        let needle = format!("\"id\":{id}");
        let deadline = std::time::Instant::now() + Duration::from_mins(1);
        loop {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or_else(|| {
                    panic!("timeout waiting for JSON-RPC response id={id}");
                });
            let line = rx
                .recv_timeout(remaining)
                .unwrap_or_else(|e| panic!("no response id={id} within deadline: {e}"));
            if line.contains(&needle) {
                return serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("invalid JSON-RPC line: {e}\n{line}"));
            }
        }
    };

    let init = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": client_name, "version": "1.0.0" }
        }
    });
    writeln!(stdin, "{init}").expect("write initialize");
    let init_res = recv_response(1);
    let instructions = init_res["result"]["instructions"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !instructions.is_empty(),
        "initialize must return instructions; got: {init_res}"
    );

    writeln!(
        stdin,
        "{}",
        serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
    )
    .expect("write initialized");

    writeln!(
        stdin,
        "{}",
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })
    )
    .expect("write tools/list");
    let tools_res = recv_response(2);
    let tools: Vec<String> = tools_res["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list must return tools; got: {tools_res}"))
        .iter()
        .filter_map(|t| t["name"].as_str().map(str::to_string))
        .collect();

    drop(stdin); // EOF → clean server shutdown
    let _ = child.wait();
    let _ = reader.join();

    (instructions, tools)
}

fn assert_lazy_core_surface(tools: &[String], client: &str) {
    for expected in lean_ctx::tool_defs::CORE_TOOL_NAMES {
        assert!(
            tools.iter().any(|t| t == expected),
            "{client}: lazy core tool {expected} missing from tools/list: {tools:?}"
        );
    }
    assert!(
        !tools.iter().any(|t| t == "ctx_graph"),
        "{client}: ctx_graph left the lazy core in #578 (ctx_callgraph replaced it): {tools:?}"
    );
}

#[test]
#[cfg_attr(
    windows,
    ignore = "HOME-override isolation is Unix-only (dirs::home_dir uses the Win32 API)"
)]
fn cursor_covered_client_gets_anchor_and_lazy_core() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");
    let env = test_env();

    run_init(bin, &env, "cursor");

    // Wiring on disk: MCP registration + canonical rules file.
    let mcp_json = std::fs::read_to_string(env.home.join(".cursor/mcp.json")).expect("mcp.json");
    assert!(mcp_json.contains("lean-ctx"), "mcp.json must register us");
    let mdc = std::fs::read_to_string(env.home.join(".cursor/rules/lean-ctx.mdc")).expect("mdc");
    assert!(
        mdc.contains("<!-- lean-ctx-rules -->"),
        "mdc must carry the canonical rules block"
    );

    let (instructions, tools) = mcp_handshake(bin, &env, "cursor");

    // Covered client → one-line anchor, NOT the full skeleton (#578).
    assert!(
        instructions.contains(ANCHOR_PREFIX),
        "cursor is covered by its mdc → instructions must carry the anchor.\n{instructions}"
    );
    assert!(
        !instructions.contains(SKELETON_MARKER),
        "cursor instructions must NOT repeat the full skeleton.\n{instructions}"
    );
    assert_lazy_core_surface(&tools, "cursor");
}

#[test]
#[cfg_attr(
    windows,
    ignore = "HOME-override isolation is Unix-only (dirs::home_dir uses the Win32 API)"
)]
fn claude_uncovered_client_gets_full_skeleton() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");
    let env = test_env();

    run_init(bin, &env, "claude");

    // CLAUDE.md carries the compact block — deliberately NOT the canonical
    // rules (rules_channel: Claude never counts as covered).
    let claude_md = std::fs::read_to_string(env.home.join(".claude/CLAUDE.md")).expect("CLAUDE.md");
    assert!(claude_md.contains("<!-- lean-ctx -->"));

    let (instructions, tools) = mcp_handshake(bin, &env, "claude-code");

    assert!(
        instructions.contains(SKELETON_MARKER),
        "claude is NOT rules-file covered → instructions must carry the full skeleton.\n{instructions}"
    );
    assert!(
        !instructions.contains(ANCHOR_PREFIX),
        "claude must not get the covered-client anchor.\n{instructions}"
    );
    assert_lazy_core_surface(&tools, "claude");
}

#[test]
#[cfg_attr(
    windows,
    ignore = "HOME-override isolation is Unix-only (dirs::home_dir uses the Win32 API)"
)]
fn codex_covered_client_gets_anchor() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");
    let env = test_env();

    run_init(bin, &env, "codex");

    // Wiring on disk: config.toml MCP entry + canonical instructions.md.
    let config = std::fs::read_to_string(env.home.join(".codex/config.toml")).expect("config");
    assert!(config.contains("lean-ctx"), "config.toml must register us");
    let instr_md =
        std::fs::read_to_string(env.home.join(".codex/instructions.md")).expect("instructions.md");
    assert!(
        instr_md.contains("<!-- lean-ctx-rules -->"),
        "instructions.md must carry the canonical rules block"
    );

    let (instructions, tools) = mcp_handshake(bin, &env, "codex");

    assert!(
        instructions.contains(ANCHOR_PREFIX),
        "codex is covered by instructions.md → anchor expected.\n{instructions}"
    );
    assert!(
        !instructions.contains(SKELETON_MARKER),
        "codex instructions must NOT repeat the full skeleton.\n{instructions}"
    );
    assert_lazy_core_surface(&tools, "codex");
}

/// Zero-config golden path: the FIRST MCP session on a fresh machine must
/// leave rules AND the on-demand SKILL.md behind (session-start heal), so
/// `doctor` is green without the user ever running `lean-ctx setup`.
///
/// Unlike the other smokes this one does NOT set LEAN_CTX_HEADLESS — the
/// startup-maintenance block (rules inject + skills install) is the unit
/// under test. The maintenance runs async after initialize, so the test
/// polls for the artifacts with a deadline while the session stays open.
#[test]
#[cfg_attr(
    windows,
    ignore = "HOME-override isolation is Unix-only (dirs::home_dir uses the Win32 API)"
)]
fn first_session_heals_rules_and_skill() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");
    let env = test_env();
    // A Cursor install is just its config dir — no prior lean-ctx wiring.
    std::fs::create_dir_all(env.home.join(".cursor")).unwrap();

    let mut child: Child = Command::new(bin)
        .arg("mcp")
        .current_dir(&env.project)
        .env("HOME", &env.home)
        .env("LEAN_CTX_DATA_DIR", &env.data)
        .env("CODEX_HOME", env.home.join(".codex"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("mcp server spawn");
    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let reader = std::thread::spawn(move || {
        // Drain stdout so the server never blocks on a full pipe.
        for _ in BufReader::new(stdout).lines() {}
    });

    let init = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "cursor", "version": "1.0.0" }
        }
    });
    writeln!(stdin, "{init}").expect("write initialize");

    let rules = env.home.join(".cursor/rules/lean-ctx.mdc");
    let skill = env.home.join(".cursor/skills/lean-ctx/SKILL.md");
    let deadline = std::time::Instant::now() + Duration::from_mins(1);
    while (!rules.exists() || !skill.exists()) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }

    drop(stdin);
    let _ = child.wait();
    let _ = reader.join();

    assert!(
        rules.exists(),
        "session-start heal must write the Cursor rules file without any setup command"
    );
    assert!(
        skill.exists(),
        "session-start heal must install SKILL.md alongside the rules (zero-config: doctor green after first session)"
    );
}

/// Guard the exact anchor prefix this smoke keys on: if `SKELETON_ANCHOR` in
/// `instructions.rs` is reworded, this test points straight at the constant
/// instead of three opaque handshake failures.
#[test]
fn anchor_prefix_matches_lib_constant() {
    let src =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/instructions.rs"))
            .expect("read instructions.rs");
    assert!(
        src.contains(ANCHOR_PREFIX),
        "SKELETON_ANCHOR in instructions.rs no longer starts with the prefix this smoke asserts on — update both together"
    );
}
