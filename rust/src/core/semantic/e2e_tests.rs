// SPDX-License-Identifier: Apache-2.0
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

use super::relations::{Relation, RelationBudget, resolve_relation_edges};
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
    /// `(subtype file, sorted supertype files)`, asked where the server
    /// offers a type hierarchy.
    extends: Option<(&'static str, &'static [&'static str])>,
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
        ("src/mem.rs", "Mem", 1, 1, "struct"),
    ],
    ambiguous: "save",
    expect_target: "src/b.rs",
    decoy: "parse",
    implements: Some(("src/store.rs", &["src/mem.rs", "src/pg.rs"])),
    extends: Some(("src/mem.rs", &["src/store.rs"])),
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
            "import { Base } from \"./base\";\nimport { Store } from \"./store\";\nexport class Mem extends Base implements Store {\n  put(): void {}\n}\n",
        ),
        (
            "src/base.ts",
            "export class Base {\n  id(): number {\n    return 1;\n  }\n}\n",
        ),
    ],
    symbols: &[
        ("src/a.ts", "save", 2, 2, "method"),
        ("src/b.ts", "save", 2, 2, "method"),
        ("src/app.ts", "checkout", 2, 5, "fn"),
        ("src/util.ts", "parseInt", 1, 3, "fn"),
        ("src/store.ts", "Store", 1, 3, "interface"),
        ("src/mem.ts", "Mem", 3, 5, "class"),
    ],
    ambiguous: "save",
    expect_target: "src/b.ts",
    decoy: "parseInt",
    implements: Some(("src/store.ts", &["src/mem.ts", "src/pg.ts"])),
    extends: Some(("src/mem.ts", &["src/base.ts", "src/store.ts"])),
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
    extends: None,
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
        ("mem.go", "Mem", 3, 3, "struct"),
    ],
    ambiguous: "Save",
    expect_target: "b.go",
    decoy: "Itoa",
    implements: Some(("store.go", &["mem.go", "pg.go"])),
    extends: Some(("mem.go", &["store.go"])),
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

/// What answers the fixture's questions.
#[derive(Clone, Copy)]
enum Engine<'a> {
    /// A language server lean-ctx starts (`eager`). `node_modules` is linked
    /// into the fixture as the project's own packages.
    Server {
        node_modules: Option<&'a std::path::Path>,
    },
    /// A real editor running the lean-ctx extension's semantic bridge; lean-ctx
    /// starts nothing (`auto`). `cli` launches the editor (`code`, `cursor`).
    Editor {
        cli: &'a std::path::Path,
        extension: &'a std::path::Path,
    },
}

/// An editor window launched for one evaluation, closed again on drop.
struct EditorWindow {
    user_data_dir: tempfile::TempDir,
    _extensions_dir: tempfile::TempDir,
}

impl EditorWindow {
    fn open(
        cli: &std::path::Path,
        extension: &std::path::Path,
        root: &str,
        data_dir: &std::path::Path,
    ) -> Self {
        let user_data_dir = tempfile::tempdir().unwrap();
        let extensions_dir = tempfile::tempdir().unwrap();
        // A fresh user-data-dir forces a new editor instance, which inherits
        // this environment (and so `LEAN_CTX_DATA_DIR`).
        let status = std::process::Command::new(cli)
            .args([
                "--new-window",
                "--disable-workspace-trust",
                "--skip-welcome",
            ])
            .args(["--skip-release-notes", "--disable-telemetry"])
            .arg("--user-data-dir")
            .arg(user_data_dir.path())
            .arg("--extensions-dir")
            .arg(extensions_dir.path())
            .arg(format!(
                "--extensionDevelopmentPath={}",
                extension.display()
            ))
            .arg(root)
            .env("LEAN_CTX_DATA_DIR", data_dir)
            .status()
            .expect("editor CLI starts");
        assert!(status.success(), "editor CLI failed: {status}");
        Self {
            user_data_dir,
            _extensions_dir: extensions_dir,
        }
    }
}

impl Drop for EditorWindow {
    fn drop(&mut self) {
        // Every process of this instance carries its unique user-data-dir.
        let marker = self.user_data_dir.path().to_string_lossy().to_string();
        #[cfg(unix)]
        let _ = std::process::Command::new("pkill")
            .arg("-f")
            .arg(&marker)
            .status();
        #[cfg(windows)]
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command"])
            .arg(format!(
                "Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -like '*{}*' }} | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force }}",
                marker.replace('\'', "''")
            ))
            .status();
    }
}

fn evaluate(f: &Fixture, engine: Engine) {
    // `shutdown_all` below drains the process-wide backend registry; the
    // editor case also points the data dir at its own announcement dir.
    let _env = crate::core::data_dir::test_env_lock();
    let _registry = crate::lsp::router::stub_test_lock();
    let (_dir, root, inputs) = materialize(f);
    let data_dir = tempfile::tempdir().unwrap();
    let (mode, _window) = match engine {
        Engine::Server { node_modules } => {
            if let Some(nm) = node_modules {
                let link = std::path::Path::new(&root).join("node_modules");
                #[cfg(unix)]
                std::os::unix::fs::symlink(nm, link).unwrap();
                #[cfg(windows)]
                std::os::windows::fs::symlink_dir(nm, link).unwrap();
            }
            (SemanticMode::Eager, None)
        }
        Engine::Editor { cli, extension } => {
            crate::test_env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());
            let window = EditorWindow::open(cli, extension, &root, data_dir.path());
            let until = Instant::now() + Duration::from_mins(2);
            while crate::lsp::editor_bridge::discover(&root).is_none() {
                assert!(Instant::now() < until, "{}: no bridge announced", f.lang);
                crate::lsp::editor_bridge::forget(&root);
                std::thread::sleep(Duration::from_millis(500));
            }
            (SemanticMode::Auto, Some(window))
        }
    };
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
            mode,
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
    if _window.is_none() {
        // The server's handshake is recorded for status, doctor and dashboard.
        let negotiated = super::coverage::negotiated(f.lang)
            .unwrap_or_else(|| panic!("{}: negotiated features not recorded", f.lang));
        assert!(
            negotiated.features.iter().any(|f| f == "definition"),
            "{}: {negotiated:?}",
            f.lang
        );
        println!("SEMANTIC_NEGOTIATED|{}|{negotiated:?}", f.lang);
    }
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

    // One relation pass, repeated until `done` holds or the cold deadline
    // passes (a server still indexing answers nothing definitive).
    let relation_pass =
        |relation, only_file, done: &dyn Fn(&super::relations::RelationPass) -> bool| loop {
            let pass = resolve_relation_edges(
                &warm_graph,
                &root,
                &inputs,
                hashes,
                mode,
                relation,
                only_file,
                RelationBudget::BACKGROUND,
            );
            if done(&pass) || Instant::now() > deadline {
                break pass;
            }
            std::thread::sleep(Duration::from_millis(500));
        };

    let implements = match f.implements {
        None => "n/a".to_string(),
        Some((declaring, expected)) => {
            let pass = relation_pass(Relation::Implements, None, &|p| !p.edges.is_empty());
            let found: Vec<&str> = pass
                .edges
                .iter()
                .inspect(|e| assert_eq!(e.to, declaring))
                .map(|e| e.from.as_str())
                .collect();
            assert_eq!(found, expected, "{}: implementors", f.lang);
            "ok".to_string()
        }
    };

    // `references`: the caller is verified as a user of the ambiguous
    // target's file, and — the decoy — not of the file whose same-named
    // symbol it never touches.
    let caller = edges
        .iter()
        .find(|e| e.callee_name == f.ambiguous)
        .map(|e| e.caller_file.clone())
        .unwrap();
    let decoy_file = f.symbols.iter().find(|s| s.1 == f.decoy).unwrap().0;
    let target_refs = relation_pass(Relation::References, Some(f.expect_target), &|p| {
        p.edges.iter().any(|e| e.from == caller)
    });
    assert!(
        target_refs
            .edges
            .iter()
            .any(|e| e.from == caller && e.to == f.expect_target),
        "{}: {caller} references {}: {:?}",
        f.lang,
        f.expect_target,
        target_refs.edges
    );
    let decoy_refs = relation_pass(Relation::References, Some(decoy_file), &|p| {
        p.settled.contains(decoy_file)
    });
    assert!(
        decoy_refs.settled.contains(decoy_file)
            && decoy_refs.edges.iter().all(|e| e.from != caller),
        "{}: no reference to the decoy's file: {decoy_refs:?}",
        f.lang
    );

    // `extends`: asked once the backend is warm. A backend without a type
    // hierarchy for the language leaves the subtype unsettled — reported,
    // not failed; a settled answer must name exactly the supertypes.
    let extends = match f.extends {
        None => "n/a".to_string(),
        Some((sub, expected)) => {
            let warm_deadline = Instant::now() + Duration::from_secs(20);
            let pass = loop {
                let pass = relation_pass(Relation::Extends, None, &|_| true);
                if pass.settled.contains(sub) || Instant::now() > warm_deadline {
                    break pass;
                }
                std::thread::sleep(Duration::from_millis(500));
            };
            if pass.settled.contains(sub) {
                let found: Vec<&str> = pass
                    .edges
                    .iter()
                    .filter(|e| e.from == sub)
                    .map(|e| e.to.as_str())
                    .collect();
                assert_eq!(found, expected, "{}: supertypes of {sub}", f.lang);
                "ok".to_string()
            } else {
                "no-type-hierarchy".to_string()
            }
        }
    };

    // An editor is no child of this test: its memory is not measured.
    let rss = if _window.is_some() {
        "n/a".to_string()
    } else {
        server_rss_mib().to_string()
    };
    println!(
        "SEMANTIC_EVAL|{}|{backend}|ambiguous=ok|decoy_veto=ok|implements={implements}|references=ok|extends={extends}|false_edges=0|cold_ms={cold_ms}|live_queries={}|warm_query_ms={warm_query_ms:.2}|cached_ms={cached_ms:.2}|server_rss_mib={rss}",
        f.lang, warm.stats.live_queries
    );
    crate::lsp::router::shutdown_all();
    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
#[ignore = "needs rust-analyzer"]
fn eval_rust_analyzer() {
    evaluate(&RUST, Engine::Server { node_modules: None });
}

#[test]
#[ignore = "needs TypeScript ≥ 7, or ≤ 6 with typescript-language-server"]
fn eval_typescript() {
    evaluate(&TYPESCRIPT, Engine::Server { node_modules: None });
}

/// The editor path end to end: a real VS Code / Cursor window running the
/// extension under `LEAN_CTX_EVAL_EXTENSION` (compiled), launched through
/// the CLI in `LEAN_CTX_EVAL_EDITOR` (`code`, `cursor`); its built-in
/// TypeScript support answers. lean-ctx starts nothing (`auto`).
#[test]
#[ignore = "needs VS Code or Cursor + LEAN_CTX_EVAL_EDITOR/LEAN_CTX_EVAL_EXTENSION"]
fn eval_editor_bridge() {
    let cli = std::env::var_os("LEAN_CTX_EVAL_EDITOR").expect("LEAN_CTX_EVAL_EDITOR");
    let extension = std::env::var_os("LEAN_CTX_EVAL_EXTENSION").expect("LEAN_CTX_EVAL_EXTENSION");
    evaluate(
        &TYPESCRIPT,
        Engine::Editor {
            cli: cli.as_ref(),
            extension: extension.as_ref(),
        },
    );
}

#[test]
#[ignore = "needs typescript-language-server + LEAN_CTX_EVAL_TS_NODE_MODULES"]
fn eval_typescript_language_server_with_project_typescript() {
    let node_modules = std::env::var_os("LEAN_CTX_EVAL_TS_NODE_MODULES")
        .expect("LEAN_CTX_EVAL_TS_NODE_MODULES: a node_modules dir with typescript ≤ 6");
    evaluate(
        &TYPESCRIPT,
        Engine::Server {
            node_modules: Some(node_modules.as_ref()),
        },
    );
}

#[test]
#[ignore = "needs pylsp"]
fn eval_pylsp() {
    evaluate(&PYTHON, Engine::Server { node_modules: None });
}

#[test]
#[ignore = "needs gopls + go"]
fn eval_gopls() {
    evaluate(&GO, Engine::Server { node_modules: None });
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
        } else if req.starts_with(b"POST /references") {
            // A capped list: the IDE found more users than it returned.
            r#"{"locations":[{"path":"src/app.rs","range":{"start":{"line":1,"character":9},"end":{"line":1,"character":13}}}],"truncated":true,"total":9}"#
        } else if req.starts_with(b"GET /health") {
            r#"{"status":"ok","editor":"vscode"}"#
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

/// Runs the RUST fixture's ambiguous call in `auto` mode against a fake
/// bridge that `announce(root, port, data_dir)` makes discoverable, and
/// checks the outcome: verified to `src/b.rs` by `backend`, with the call
/// site sent project-relative (on Windows too, where the root uses `\`).
fn verifies_through_fake_bridge(announce: impl FnOnce(&str, u16, &std::path::Path), backend: &str) {
    let _env = crate::core::data_dir::test_env_lock();
    // `shutdown_all` below drains the process-wide backend registry.
    let _registry = crate::lsp::router::stub_test_lock();
    let data = tempfile::tempdir().unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());

    let (_dir, root, inputs) = materialize(&RUST);
    let bridge = FakeBridge::start();
    announce(&root, bridge.port, data.path());

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
        "port file read: {}, health: {}, editor bridge: {}, stats: {:?}, unsettled: {:?}, direct: {direct:?}",
        port.is_some(),
        port.as_ref()
            .is_some_and(crate::lsp::port_discovery::health_ok),
        crate::lsp::editor_bridge::discover(&root).is_some(),
        out.stats,
        out.unsettled
    );
    let sent = bridge.definition_paths();
    crate::lsp::router::shutdown_all();
    crate::lsp::editor_bridge::forget(&root);
    drop(bridge);
    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    assert_eq!(
        out.verdicts,
        vec![Some(SemanticVerdict::Verified {
            file: "src/b.rs".into(),
            backend: backend.into(),
        })],
        "{diagnosis}"
    );
    assert_eq!(out.unsettled, vec![false]);
    assert!(
        !sent.is_empty() && sent.iter().all(|p| p == "src/app.rs"),
        "the call site goes out project-relative: {sent:?}"
    );
}

/// `auto` mode with a live IDE: the router attaches to the JetBrains plugin
/// found through its port file (never spawning a server).
#[test]
fn auto_mode_verifies_through_a_live_jetbrains_bridge() {
    verifies_through_fake_bridge(
        |root, port, _| {
            let port_file = crate::lsp::port_discovery::port_file_path(root).unwrap();
            std::fs::create_dir_all(port_file.parent().unwrap()).unwrap();
            std::fs::write(
                &port_file,
                serde_json::json!({
                    "port": port, "token": "tok", "pid": std::process::id(),
                    "project_root": root, "ide_version": "2026.2",
                })
                .to_string(),
            )
            .unwrap();
        },
        "jetbrains:jetbrains@2026.2",
    );
}

/// `auto` mode with an editor extension's semantic bridge, announced in the
/// bridge directory under another spelling of the project root (trailing
/// separator): matched by canonical root, answered under the editor's
/// identity.
#[test]
fn auto_mode_verifies_through_an_editor_bridge() {
    verifies_through_fake_bridge(announce_editor_bridge, "editor:vscode@1.99.0+abc123def456");
}

/// Announces a fake bridge as the editor extension would — under another
/// spelling of the project root (trailing separator), with a provider
/// fingerprint that becomes part of the backend identity.
fn announce_editor_bridge(root: &str, port: u16, data: &std::path::Path) {
    let dir = data.join("editor-bridges");
    std::fs::create_dir_all(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(
        dir.join("vscode-test.json"),
        serde_json::json!({
            "port": port, "token": "tok", "pid": std::process::id(),
            "project_root": format!("{root}{}", std::path::MAIN_SEPARATOR),
            "editor": "vscode", "editor_version": "1.99.0",
            "provider_fingerprint": "abc123def4567890",
        })
        .to_string(),
    )
    .unwrap();
}

/// A capped `references` answer names real users — their edges are added —
/// but is not the whole answer: it neither settles the file nor withdraws
/// an earlier edge the cap may have hidden, and is not cached.
#[test]
fn truncated_references_add_edges_but_never_prune() {
    use super::{EdgeEvidence, EvidenceGrade, EvidenceOrigin};
    use crate::core::graph_enricher::apply_relation_edges;
    use crate::core::property_graph::{EdgeKind, Node};

    let _env = crate::core::data_dir::test_env_lock();
    let _registry = crate::lsp::router::stub_test_lock();
    let data = tempfile::tempdir().unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
    let (_dir, root, inputs) = materialize(&RUST);
    let bridge = FakeBridge::start();
    announce_editor_bridge(&root, bridge.port, data.path());

    let graph = CodeGraph::open_in_memory().unwrap();
    let id = |p: &str| graph.upsert_node(&Node::file(p)).unwrap();
    let (a, b) = (id("src/a.rs"), id("src/b.rs"));
    id("src/app.rs");
    // An earlier, complete answer had verified src/a.rs as a user.
    let earlier = EdgeEvidence::new(
        EvidenceGrade::VerifiedSemantic,
        EvidenceOrigin::Enrichment,
        Some("editor:vscode@1.99.0".into()),
        1,
    );
    graph
        .upsert_edge_with_evidence(a, b, &EdgeKind::References, &earlier)
        .unwrap();

    let hashes = CallGraph::build(&inputs).file_hashes;
    let pass = resolve_relation_edges(
        &graph,
        &root,
        &inputs,
        &hashes,
        SemanticMode::Auto,
        Relation::References,
        Some("src/b.rs"),
        RelationBudget::INTERACTIVE,
    );
    apply_relation_edges(&graph, Relation::References, &pass, |_, _| false).unwrap();
    let again = resolve_relation_edges(
        &graph,
        &root,
        &inputs,
        &hashes,
        SemanticMode::Auto,
        Relation::References,
        Some("src/b.rs"),
        RelationBudget::INTERACTIVE,
    );
    crate::lsp::editor_bridge::forget(&root);
    drop(bridge);
    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");

    assert_eq!(pass.answered, 1);
    assert!(
        !pass.settled.contains("src/b.rs"),
        "a capped answer settles nothing"
    );
    let pairs: Vec<(String, String)> = graph
        .file_edges_of_kind(&EdgeKind::References)
        .unwrap()
        .into_iter()
        .map(|(s, t, _)| (s, t))
        .collect();
    assert_eq!(
        pairs,
        [
            ("src/a.rs".to_string(), "src/b.rs".to_string()),
            ("src/app.rs".to_string(), "src/b.rs".to_string()),
        ],
        "the named user is added, the earlier one kept"
    );
    assert_eq!((again.cache_hits, again.live_queries), (0, 1), "not cached");
}
