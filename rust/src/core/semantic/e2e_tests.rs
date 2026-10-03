//! End-to-end evaluation against real language servers, plus the JetBrains
//! bridge path against a local fake bridge.
//!
//! Language-server tests are ignored by default (CI guarantees no server);
//! run them with
//! `cargo test --lib semantic::e2e_tests -- --ignored --nocapture --test-threads=1`
//! on a machine with rust-analyzer, TypeScript (≥ 7, or ≤ 6 with
//! typescript-language-server), pylsp and gopls. Each prints one
//! `SEMANTIC_EVAL|…` metrics line (see ADR-015). The TypeScript ≤ 6 path in a
//! project with its own TypeScript runs when `LEAN_CTX_EVAL_TS_NODE_MODULES`
//! names a `node_modules` directory holding such a `typescript`.
//!
//! Every fixture poses the same three questions:
//! 1. an *ambiguous* method call (`save`, defined in two project files, no
//!    import in scope) — must be verified to the right file;
//! 2. a *decoy*: a name defined once in the project (so structure guesses
//!    it) while the call actually targets the standard library — the
//!    backend must veto the guess;
//! 3. *implementations* of a trait/interface, where the server supports it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::core::call_graph::{
    CallEdge, CallGraph, CallGraphInputs, StructuralTarget, SymbolSpan, resolve_edge_callee_targets,
};
use crate::core::config::SemanticMode;
use crate::core::property_graph::CodeGraph;

use super::implementations::resolve_implements_edges;
use super::{EscalationBudget, SemanticVerdict, escalate_calls};

struct Fixture {
    lang: &'static str,
    files: &'static [(&'static str, &'static str)],
    symbols: &'static [(&'static str, &'static str, usize, usize, &'static str)],
    ambiguous: &'static str,
    expect_target: &'static str,
    decoy: &'static str,
    /// `(declaring file, sorted implementor files)`, when supported.
    implements: Option<(&'static str, &'static [&'static str])>,
}

const RUST: Fixture = Fixture {
    lang: "rust",
    files: &[
        (
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
        ),
        (
            "src/lib.rs",
            "pub mod a;\npub mod app;\npub mod b;\npub mod mem;\npub mod pg;\npub mod store;\npub mod util;\n",
        ),
        (
            "src/a.rs",
            "pub struct UserRepo;\nimpl UserRepo {\n    pub fn save(&self) {}\n}\n",
        ),
        (
            "src/b.rs",
            "pub struct PaymentRepo;\nimpl PaymentRepo {\n    pub fn save(&self) {}\n}\n",
        ),
        (
            "src/app.rs",
            "pub fn checkout(repo: &crate::b::PaymentRepo) -> i32 {\n    repo.save();\n    let n: i32 = \"1\".parse().unwrap_or(0);\n    n\n}\n",
        ),
        ("src/util.rs", "pub fn parse(_s: &str) -> i32 {\n    0\n}\n"),
        ("src/store.rs", "pub trait Store {\n    fn put(&self);\n}\n"),
        (
            "src/pg.rs",
            "pub struct Pg;\nimpl crate::store::Store for Pg {\n    fn put(&self) {}\n}\n",
        ),
        (
            "src/mem.rs",
            "pub struct Mem;\nimpl crate::store::Store for Mem {\n    fn put(&self) {}\n}\n",
        ),
    ],
    symbols: &[
        ("src/a.rs", "save", 3, 3, "method"),
        ("src/b.rs", "save", 3, 3, "method"),
        ("src/app.rs", "checkout", 1, 5, "fn"),
        ("src/util.rs", "parse", 1, 3, "fn"),
        ("src/store.rs", "Store", 1, 3, "trait"),
    ],
    ambiguous: "save",
    expect_target: "src/b.rs",
    decoy: "parse",
    implements: Some(("src/store.rs", &["src/mem.rs", "src/pg.rs"])),
};

const TYPESCRIPT: Fixture = Fixture {
    lang: "typescript",
    files: &[
        (
            "tsconfig.json",
            "{ \"compilerOptions\": { \"strict\": true, \"target\": \"es2020\", \"module\": \"commonjs\" }, \"include\": [\"src\"] }\n",
        ),
        (
            "src/a.ts",
            "export class UserRepo {\n  save(): void {}\n}\n",
        ),
        (
            "src/b.ts",
            "export class PaymentRepo {\n  save(): void {}\n}\n",
        ),
        (
            "src/app.ts",
            "import { PaymentRepo } from \"./b\";\nexport function checkout(repo: PaymentRepo): number {\n  repo.save();\n  return Number.parseInt(\"1\");\n}\n",
        ),
        (
            "src/util.ts",
            "export function parseInt(_s: string): number {\n  return 0;\n}\n",
        ),
        (
            "src/store.ts",
            "export interface Store {\n  put(): void;\n}\n",
        ),
        (
            "src/pg.ts",
            "import { Store } from \"./store\";\nexport class Pg implements Store {\n  put(): void {}\n}\n",
        ),
        (
            "src/mem.ts",
            "import { Store } from \"./store\";\nexport class Mem implements Store {\n  put(): void {}\n}\n",
        ),
    ],
    symbols: &[
        ("src/a.ts", "save", 2, 2, "method"),
        ("src/b.ts", "save", 2, 2, "method"),
        ("src/app.ts", "checkout", 2, 5, "fn"),
        ("src/util.ts", "parseInt", 1, 3, "fn"),
        ("src/store.ts", "Store", 1, 3, "interface"),
    ],
    ambiguous: "save",
    expect_target: "src/b.ts",
    decoy: "parseInt",
    implements: Some(("src/store.ts", &["src/mem.ts", "src/pg.ts"])),
};

const PYTHON: Fixture = Fixture {
    lang: "python",
    files: &[
        (
            "a.py",
            "class UserRepo:\n    def save(self):\n        pass\n",
        ),
        (
            "b.py",
            "class PaymentRepo:\n    def save(self):\n        pass\n",
        ),
        (
            "app.py",
            "import json\n\nfrom b import PaymentRepo\n\n\ndef checkout(repo: PaymentRepo):\n    repo.save()\n    return json.loads(\"1\")\n",
        ),
        ("util.py", "def loads(s):\n    return 0\n"),
    ],
    symbols: &[
        ("a.py", "save", 2, 3, "method"),
        ("b.py", "save", 2, 3, "method"),
        ("app.py", "checkout", 6, 8, "fn"),
        ("util.py", "loads", 1, 2, "fn"),
    ],
    ambiguous: "save",
    expect_target: "b.py",
    decoy: "loads",
    // pylsp offers no textDocument/implementation.
    implements: None,
};

const GO: Fixture = Fixture {
    lang: "go",
    files: &[
        ("go.mod", "module fixture\n\ngo 1.21\n"),
        (
            "a.go",
            "package fixture\n\ntype UserRepo struct{}\n\nfunc (UserRepo) Save() {}\n",
        ),
        (
            "b.go",
            "package fixture\n\ntype PaymentRepo struct{}\n\nfunc (PaymentRepo) Save() {}\n",
        ),
        (
            "app.go",
            "package fixture\n\nimport \"strconv\"\n\nfunc Checkout(r PaymentRepo) string {\n\tr.Save()\n\treturn strconv.Itoa(1)\n}\n",
        ),
        (
            "util.go",
            "package fixture\n\nfunc Itoa(i int) string {\n\treturn \"\"\n}\n",
        ),
        (
            "store.go",
            "package fixture\n\ntype Store interface {\n\tPut()\n}\n",
        ),
        (
            "pg.go",
            "package fixture\n\ntype Pg struct{}\n\nfunc (Pg) Put() {}\n",
        ),
        (
            "mem.go",
            "package fixture\n\ntype Mem struct{}\n\nfunc (Mem) Put() {}\n",
        ),
    ],
    symbols: &[
        ("a.go", "Save", 5, 5, "method"),
        ("b.go", "Save", 5, 5, "method"),
        ("app.go", "Checkout", 5, 8, "fn"),
        ("util.go", "Itoa", 3, 5, "fn"),
        ("store.go", "Store", 3, 5, "interface"),
    ],
    ambiguous: "Save",
    expect_target: "b.go",
    decoy: "Itoa",
    implements: Some(("store.go", &["mem.go", "pg.go"])),
};

/// Writes the fixture into a fresh project and returns `(dir guard, inputs)`.
fn materialize(f: &Fixture) -> (tempfile::TempDir, String, CallGraphInputs) {
    let dir = tempfile::tempdir().unwrap();
    // Like real project roots: no Windows verbatim (`\\?\`) prefix.
    let root = crate::core::pathutil::safe_canonicalize(dir.path()).unwrap();
    for (path, body) in f.files {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let root_str = root.to_string_lossy().to_string();
    let source_ext = f.symbols[0].0.rsplit('.').next().unwrap();
    let inputs = CallGraphInputs {
        project_root: root_str.clone(),
        file_paths: f
            .files
            .iter()
            .map(|(p, _)| (*p).to_string())
            .filter(|p| p.ends_with(&format!(".{source_ext}")))
            .collect(),
        symbols: f
            .symbols
            .iter()
            .map(|(file, name, start, end, kind)| SymbolSpan {
                file: (*file).into(),
                name: (*name).into(),
                start_line: *start,
                end_line: *end,
                kind: (*kind).into(),
            })
            .collect(),
        import_edges: Vec::new(),
    };
    (dir, root_str, inputs)
}

/// Resident memory of all running processes of `binary`, in MiB.
/// Resident memory of every process this test spawned (the server and its
/// helpers, e.g. a node wrapper plus the native TypeScript binary).
fn server_rss_mib() -> u64 {
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,rss="])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let procs: Vec<[u64; 3]> = out
        .lines()
        .filter_map(|l| {
            let mut n = l.split_whitespace().map(|v| v.parse::<u64>().ok());
            Some([n.next()??, n.next()??, n.next()??])
        })
        .collect();
    let mut tree = vec![u64::from(std::process::id())];
    let mut kib = 0;
    while let Some(parent) = tree.pop() {
        for [pid, _, rss] in procs.iter().filter(|p| p[1] == parent) {
            kib += rss;
            tree.push(*pid);
        }
    }
    kib / 1024
}

fn verdict_for<'a>(
    edges: &[CallEdge],
    verdicts: &'a [Option<SemanticVerdict>],
    callee: &str,
) -> Option<&'a SemanticVerdict> {
    edges
        .iter()
        .position(|e| e.callee_name == callee)
        .and_then(|i| verdicts[i].as_ref())
}

/// `node_modules`: linked into the fixture as the project's own packages.
fn evaluate(f: &Fixture, node_modules: Option<&std::path::Path>) {
    // `shutdown_all` below drains the process-wide backend registry.
    let _registry = crate::lsp::router::stub_test_lock();
    let (_dir, root, inputs) = materialize(f);
    if let Some(nm) = node_modules {
        let link = std::path::Path::new(&root).join("node_modules");
        #[cfg(unix)]
        std::os::unix::fs::symlink(nm, link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(nm, link).unwrap();
    }
    let call_graph = CallGraph::build(&inputs);
    let edges: Vec<CallEdge> = call_graph
        .edges
        .iter()
        .filter(|e| e.callee_name == f.ambiguous || e.callee_name == f.decoy)
        .cloned()
        .collect();
    let structural = resolve_edge_callee_targets(&inputs, &edges);
    for (e, s) in edges.iter().zip(&structural) {
        let expected_uncertain = matches!(
            s,
            StructuralTarget::Ambiguous | StructuralTarget::Resolved { .. }
        );
        assert!(
            expected_uncertain,
            "{}: {} must be a semantic candidate",
            f.lang, e.callee_name
        );
    }
    let hashes: &HashMap<String, String> = &call_graph.file_hashes;
    let run = |graph: &CodeGraph| {
        escalate_calls(
            graph,
            &root,
            &inputs,
            &edges,
            &structural,
            hashes,
            SemanticMode::Eager,
            EscalationBudget::BACKGROUND,
        )
    };

    // Cold: server start + indexing, until both questions are answered.
    let started = Instant::now();
    let deadline = started + Duration::from_mins(4);
    let cold_graph = CodeGraph::open_in_memory().unwrap();
    let cold = loop {
        let out = run(&cold_graph);
        let answered = verdict_for(&edges, &out.verdicts, f.ambiguous).is_some()
            && verdict_for(&edges, &out.verdicts, f.decoy).is_some();
        if answered || Instant::now() > deadline {
            break out;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    let cold_ms = started.elapsed().as_millis();

    let backend = match verdict_for(&edges, &cold.verdicts, f.ambiguous) {
        Some(SemanticVerdict::Verified { file, backend }) => {
            assert_eq!(file, f.expect_target, "{}: ambiguous call target", f.lang);
            backend.clone()
        }
        other => panic!("{}: ambiguous call not verified: {other:?}", f.lang),
    };
    assert_eq!(
        verdict_for(&edges, &cold.verdicts, f.decoy),
        Some(&SemanticVerdict::NotInProject),
        "{}: the decoy's name-match guess must be vetoed",
        f.lang
    );
    let false_edges = cold
        .verdicts
        .iter()
        .zip(&edges)
        .filter(|(v, e)| match v {
            Some(SemanticVerdict::Verified { file, .. }) => {
                e.callee_name == f.decoy || file != f.expect_target
            }
            _ => false,
        })
        .count();
    assert_eq!(false_edges, 0, "{}: no false verified edge", f.lang);

    // Warm: a fresh cache against the running server.
    let warm_graph = CodeGraph::open_in_memory().unwrap();
    let t = Instant::now();
    let warm = run(&warm_graph);
    let warm_query_ms = t.elapsed().as_secs_f64() * 1000.0 / warm.stats.live_queries.max(1) as f64;

    // Cached: the same questions again, answered without the server.
    let t = Instant::now();
    let cached = run(&warm_graph);
    let cached_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(
        cached.stats.live_queries, 0,
        "{}: cached run asks nothing",
        f.lang
    );
    assert_eq!(
        cached.verdicts, warm.verdicts,
        "{}: cache reproduces answers",
        f.lang
    );

    let implements = match f.implements {
        None => "n/a".to_string(),
        Some((declaring, expected)) => {
            let pass = loop {
                let pass = resolve_implements_edges(
                    &warm_graph,
                    &root,
                    &inputs,
                    hashes,
                    SemanticMode::Eager,
                );
                if !pass.edges.is_empty() || Instant::now() > deadline {
                    break pass;
                }
                std::thread::sleep(Duration::from_millis(500));
            };
            let found: Vec<&str> = pass
                .edges
                .iter()
                .inspect(|e| assert_eq!(e.abstract_file, declaring))
                .map(|e| e.impl_file.as_str())
                .collect();
            assert_eq!(found, expected, "{}: implementors", f.lang);
            "ok".to_string()
        }
    };

    let rss = server_rss_mib();
    println!(
        "SEMANTIC_EVAL|{}|{backend}|ambiguous=ok|decoy_veto=ok|implements={implements}|false_edges=0|cold_ms={cold_ms}|live_queries={}|warm_query_ms={warm_query_ms:.2}|cached_ms={cached_ms:.2}|server_rss_mib={rss}",
        f.lang, warm.stats.live_queries
    );
    crate::lsp::router::shutdown_all();
}

#[test]
#[ignore = "needs rust-analyzer"]
fn eval_rust_analyzer() {
    evaluate(&RUST, None);
}

#[test]
#[ignore = "needs TypeScript ≥ 7, or ≤ 6 with typescript-language-server"]
fn eval_typescript() {
    evaluate(&TYPESCRIPT, None);
}

#[test]
#[ignore = "needs typescript-language-server + LEAN_CTX_EVAL_TS_NODE_MODULES"]
fn eval_typescript_language_server_with_project_typescript() {
    let node_modules = std::env::var_os("LEAN_CTX_EVAL_TS_NODE_MODULES")
        .expect("LEAN_CTX_EVAL_TS_NODE_MODULES: a node_modules dir with typescript ≤ 6");
    evaluate(&TYPESCRIPT, Some(node_modules.as_ref()));
}

#[test]
#[ignore = "needs pylsp"]
fn eval_pylsp() {
    evaluate(&PYTHON, None);
}

#[test]
#[ignore = "needs gopls + go"]
fn eval_gopls() {
    evaluate(&GO, None);
}

/// A minimal JetBrains bridge: `/health` plus `/definition` answering the
/// fixture's ambiguous call with `src/b.rs`, recording the `path` each
/// `/definition` request carried. Blocking accept, one thread per
/// connection, so a slow scheduler cannot starve a request. Serves until
/// dropped.
struct FakeBridge {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    definition_paths: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl FakeBridge {
    fn start() -> Self {
        use std::sync::atomic::Ordering;
        use std::sync::{Arc, Mutex};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let definition_paths = Arc::new(Mutex::new(Vec::new()));
        let (stop_flag, paths) = (Arc::clone(&stop), Arc::clone(&definition_paths));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop_flag.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let paths = Arc::clone(&paths);
                std::thread::spawn(move || Self::serve(stream, &paths));
            }
        });
        Self {
            port,
            stop,
            definition_paths,
        }
    }

    fn serve(mut stream: std::net::TcpStream, paths: &std::sync::Mutex<Vec<String>>) {
        use std::io::{Read, Write};
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut req = Vec::new();
        let mut buf = [0u8; 4096];
        let body_start = loop {
            let Ok(n) = stream.read(&mut buf) else { return };
            if n == 0 {
                return;
            }
            req.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&req);
            if let Some(head_end) = text.find("\r\n\r\n") {
                let len = text[..head_end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if req.len() >= head_end + 4 + len {
                    break head_end + 4;
                }
            }
        };
        let body = if req.starts_with(b"POST /definition") {
            if let Some(path) = serde_json::from_slice::<serde_json::Value>(&req[body_start..])
                .ok()
                .and_then(|v| v.get("path")?.as_str().map(str::to_string))
            {
                paths
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(path);
            }
            r#"{"locations":[{"path":"src/b.rs","range":{"start":{"line":2,"character":11},"end":{"line":2,"character":15}}}]}"#
        } else {
            "{}"
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
    }

    fn definition_paths(&self) -> Vec<String> {
        self.definition_paths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for FakeBridge {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        // Wake the blocking accept so the thread sees the flag.
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// `auto` mode with a live IDE: the router attaches to the bridge (never
/// spawning a server), the call site goes out as a project-relative path
/// (on Windows too, where the root uses `\`), and the answer flows through
/// location mapping into a verified edge carrying the IDE's identity.
#[test]
fn auto_mode_verifies_through_a_live_jetbrains_bridge() {
    let _env = crate::core::data_dir::test_env_lock();
    // `shutdown_all` below drains the process-wide backend registry.
    let _registry = crate::lsp::router::stub_test_lock();
    let data = tempfile::tempdir().unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());

    let (_dir, root, inputs) = materialize(&RUST);
    let bridge = FakeBridge::start();
    let port_file = crate::lsp::port_discovery::port_file_path(&root).unwrap();
    std::fs::create_dir_all(port_file.parent().unwrap()).unwrap();
    std::fs::write(
        &port_file,
        serde_json::json!({
            "port": bridge.port,
            "token": "tok",
            "pid": std::process::id(),
            "project_root": root,
            "ide_version": "2026.2",
        })
        .to_string(),
    )
    .unwrap();

    let call_graph = CallGraph::build(&inputs);
    let edges: Vec<CallEdge> = call_graph
        .edges
        .iter()
        .filter(|e| e.callee_name == "save")
        .cloned()
        .collect();
    let structural = resolve_edge_callee_targets(&inputs, &edges);
    let graph = CodeGraph::open_in_memory().unwrap();
    let out = escalate_calls(
        &graph,
        &root,
        &inputs,
        &edges,
        &structural,
        &call_graph.file_hashes,
        SemanticMode::Auto,
        EscalationBudget::INTERACTIVE,
    );

    // Which stage failed, if any: discovery, reachability, or the query —
    // the latter re-asked directly to surface its error.
    let port = crate::lsp::port_discovery::read_port_file(&root);
    let direct = edges[0].callee_pos.map(|pos| {
        super::resolve::resolve_definition(
            &root,
            &edges[0].caller_file,
            pos,
            super::resolve::Access {
                policy: crate::lsp::router::StartPolicy::ReuseOnly,
                wait: true,
                timeout: Some(Duration::from_secs(10)),
                deadline: None,
            },
            &inputs,
        )
    });
    let diagnosis = format!(
        "port file read: {}, health: {}, stats: {:?}, unsettled: {:?}, direct: {direct:?}",
        port.is_some(),
        port.as_ref()
            .is_some_and(crate::lsp::port_discovery::health_ok),
        out.stats,
        out.unsettled
    );
    let sent = bridge.definition_paths();
    crate::lsp::router::shutdown_all();
    drop(bridge);
    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    assert_eq!(
        out.verdicts,
        vec![Some(SemanticVerdict::Verified {
            file: "src/b.rs".into(),
            backend: "jetbrains:jetbrains@2026.2".into(),
        })],
        "{diagnosis}"
    );
    assert_eq!(out.unsettled, vec![false]);
    assert!(
        !sent.is_empty() && sent.iter().all(|p| p == "src/app.rs"),
        "the call site goes out project-relative: {sent:?}"
    );
}
