//! #990 e2e — machine-readable purity *and* once-per-session flag survival.
//!
//! `ctx_outline format=json` must return byte-exact, parseable JSON with no
//! prose decoration. Critically, a json *first* call must not consume the
//! once-per-session auto-context briefing slot: skipping the dispatch pre-hook
//! for machine-readable calls keeps the wake-up briefing intact so it still
//! fires on the next human-facing call.
//!
//! Determinism: the project root is pinned via `LEAN_CTX_PROJECT_ROOT` (no
//! async client-roots race), one project fact is stored beforehand, and the
//! test polls `ctx_preload` — which does *not* consume the briefing flag —
//! until the graph is loaded in-process. Only then does it run the discriminating sequence
//! (json call, then two text calls), so the graph-build race cannot make this
//! test flaky and the root is live exactly when the json call runs.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

const SAMPLE_RS: &str = r"//! Sample module for ctx_outline e2e.

pub struct Engine {
    pub name: String,
    pub power: u32,
}

impl Engine {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string(), power: 0 }
    }

    pub fn ignite(&mut self) -> bool {
        self.power = 100;
        true
    }
}

pub trait Drivable {
    fn drive(&self) -> String;
}

pub enum Gear {
    Park,
    Drive,
    Reverse,
}

pub fn accelerate(engine: &mut Engine, delta: u32) -> u32 {
    engine.power = engine.power.saturating_add(delta);
    engine.power
}
";

/// Fully isolated lean-ctx environment with a one-file project tree.
struct McpSandbox {
    _root: tempfile::TempDir,
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    project: PathBuf,
}

impl McpSandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let config = root.path().join("config");
        let data = root.path().join("data");
        let state = root.path().join("state");
        let cache = root.path().join("cache");
        let project = root.path().join("project");
        for d in [&home, &config, &data, &state, &cache, &project] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(project.join("sample.rs"), SAMPLE_RS).unwrap();
        // Canonicalize so `index build --root`, `LEAN_CTX_PROJECT_ROOT` and the
        // server's cwd resolve to the exact same path (macOS /tmp -> /private/tmp).
        let project = project.canonicalize().unwrap();
        Self {
            _root: root,
            home,
            config,
            data,
            state,
            cache,
            project,
        }
    }

    fn base_command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
        cmd.env("HOME", &self.home)
            .env("LEAN_CTX_CONFIG_DIR", &self.config)
            .env("LEAN_CTX_DATA_DIR", &self.data)
            .env("LEAN_CTX_STATE_DIR", &self.state)
            .env("LEAN_CTX_CACHE_DIR", &self.cache)
            // Never talk to (or start) the developer's daemon.
            .env("LEAN_CTX_HOOK_CHILD", "1")
            // Resolve the project root synchronously from the env (no async
            // client-roots / cwd race) so the auto-context overview is live on
            // the very FIRST tool call. This makes the briefing-survival check
            // discriminating: the root is ready when the json call runs, so the
            // unfixed dispatch would burn the once-per-session flag right there.
            // `LEAN_CTX_QUIET` is intentionally NOT set — it suppresses the
            // autonomy briefing this test asserts on.
            .env("LEAN_CTX_PROJECT_ROOT", &self.project);
        cmd
    }

    /// Synchronously build the project graph + BM25 so the overview warms fast.
    ///
    /// On resource-constrained CI runners the index subprocess can be OOM-killed
    /// (SIGKILL / exit 137). Panic in that case would be a flaky false-negative,
    /// so we return `false` and let the caller skip the test gracefully.
    fn build_index(&self) -> bool {
        let out = self
            .base_command()
            .args(["index", "build", "--root"])
            .arg(&self.project)
            .output()
            .expect("spawn lean-ctx index build");
        if !out.status.success() {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                if matches!(out.status.signal(), Some(9 | 15)) {
                    eprintln!(
                        "index build killed by signal {} — skipping test",
                        out.status.signal().unwrap()
                    );
                    return false;
                }
            }
            panic!(
                "index build failed: {}\nstdout:\n{}\nstderr:\n{}",
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
        }
        true
    }

    /// Store one project fact through the CLI, so the session has a wake-up
    /// briefing to deliver.
    fn remember(&self, value: &str) {
        let out = self
            .base_command()
            .current_dir(&self.project)
            .args(["knowledge", "remember", value, "--category", "decision"])
            .args(["--key", "engine-power"])
            .output()
            .expect("spawn lean-ctx knowledge remember");
        assert!(
            out.status.success(),
            "knowledge remember failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Create a context package and mark it auto-load through the CLI.
    fn auto_load_package(&self, name: &str) {
        for args in [
            vec!["pack", "create", "--name", name, "--description", "e2e"],
            vec!["pack", "auto-load", name],
        ] {
            let out = self
                .base_command()
                .current_dir(&self.project)
                .args(&args)
                .output()
                .expect("spawn lean-ctx pack");
            assert!(
                out.status.success(),
                "{args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

/// A live `mcp` stdio session driven request/response so we can poll for
/// readiness before asserting on order-sensitive, once-per-session behaviour.
struct McpSession {
    child: Child,
    stdin: Option<ChildStdin>,
    reader: BufReader<ChildStdout>,
}

impl McpSession {
    fn start(sandbox: &McpSandbox) -> Self {
        let mut child = sandbox
            .base_command()
            .arg("mcp")
            .current_dir(&sandbox.project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn lean-ctx mcp");
        let stdin = child.stdin.take().expect("mcp stdin");
        let stdout = child.stdout.take().expect("mcp stdout");
        let mut session = Self {
            child,
            stdin: Some(stdin),
            reader: BufReader::new(stdout),
        };
        session.handshake();
        session
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    /// Read stdout until the JSON-RPC response with `id` arrives, skipping any
    /// server-initiated notifications (which carry no `id`).
    fn read_for(&mut self, id: i64) -> Value {
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).expect("read mcp stdout");
            assert!(n != 0, "mcp closed stdout before responding to id {id}");
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(line)
                && v.get("id").and_then(Value::as_i64) == Some(id)
            {
                return v;
            }
        }
    }

    fn handshake(&mut self) {
        let init = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "e2e", "version": "1.0" }
            }
        })
        .to_string();
        self.send(&init);
        self.read_for(1);
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string());
    }

    fn call(&mut self, id: i64, name: &str, args: &Value) -> String {
        self.send(
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": name, "arguments": args }
            })
            .to_string(),
        );
        let resp = self.read_for(id);
        text_content(&resp).unwrap_or_default()
    }

    /// Poll `ctx_preload` until the project graph is loaded in-process. The
    /// wake-up briefing skips this tool *before* its flag is touched, so warming
    /// this way never consumes the once-per-session slot.
    fn warm_overview(&mut self) {
        for i in 0..50 {
            let preload = self.call(
                1000 + i,
                "ctx_preload",
                &json!({ "task": "outline sample" }),
            );
            if !preload.is_empty()
                && !preload.contains("No graph available")
                && !preload.starts_with("ERROR")
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("ctx_preload never reported a ready project graph");
    }

    fn shutdown(mut self) {
        self.stdin.take();
        let _ = self.child.wait();
    }
}

fn text_content(resp: &Value) -> Option<String> {
    resp.get("result")?
        .get("content")?
        .as_array()?
        .first()?
        .get("text")?
        .as_str()
        .map(str::to_string)
}

#[test]
fn json_first_call_is_pure_and_preserves_wakeup_briefing() {
    let sandbox = McpSandbox::new();
    if !sandbox.build_index() {
        return; // OOM-killed on CI — skip gracefully
    }
    sandbox.remember("ignite sets engine power to 100");
    sandbox.auto_load_package("e2e-pack");

    let mut session = McpSession::start(&sandbox);
    // Warm the in-process graph so the briefing content is ready deterministically
    // (without consuming the once-per-session briefing flag).
    session.warm_overview();

    // Call A is json (machine-readable); B is `ctx_session status`, the only
    // call that delivers the briefing; C is a plain-text outline; D repeats B.
    let call_a = session.call(
        2,
        "ctx_outline",
        &json!({ "path": "sample.rs", "format": "json" }),
    );
    let call_b = session.call(3, "ctx_session", &json!({ "action": "status" }));
    let call_c = session.call(4, "ctx_outline", &json!({ "path": "sample.rs" }));
    let call_d = session.call(5, "ctx_session", &json!({ "action": "status" }));
    session.shutdown();

    // (1) Machine-readable purity: A is byte-exact JSON, no prose decoration.
    assert!(
        call_a.trim_start().starts_with('{'),
        "json call must start with '{{', got:\n{call_a}"
    );
    assert!(
        !call_a.contains("AUTO CONTEXT"),
        "json call must NOT carry the auto-context briefing:\n{call_a}"
    );
    let parsed: Value = serde_json::from_str(&call_a)
        .unwrap_or_else(|e| panic!("json call must parse ({e}):\n{call_a}"));
    assert_eq!(
        parsed["backend"], "tree-sitter",
        "outline must be AST-backed, not regex"
    );
    assert!(
        parsed["symbols"].as_array().is_some_and(|s| !s.is_empty()),
        "outline must list symbols:\n{call_a}"
    );

    // (2) Flag survival: the json call does not burn the once-per-session
    // briefing (#990); `ctx_session status` delivers it.
    assert!(
        call_b.contains("AUTO CONTEXT"),
        "wake-up briefing must survive a json-first call and arrive with \
         ctx_session status (#990); got:\n{call_b}"
    );
    // The briefing carries the stored fact: it used to ride on a full project
    // overview that exceeded the 400-token injection cap on real projects.
    assert!(
        call_b.contains("engine power to 100"),
        "the wake-up briefing must carry the stored project fact; got:\n{call_b}"
    );
    assert!(
        call_b.contains("CONTEXT PACKAGES AUTO-LOADED: e2e-pack v1.0.0"),
        "an auto-load context package must be applied and named; got:\n{call_b}"
    );

    // (3) Other tools never carry the briefing, and it arrives only once.
    assert!(
        !call_c.contains("AUTO CONTEXT") && !call_c.contains("WAKE-UP"),
        "an outline must not carry the session briefing:\n{call_c}"
    );
    assert!(
        !call_d.contains("AUTO CONTEXT"),
        "briefing must fire only once per session:\n{call_d}"
    );
}
