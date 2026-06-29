//! End-to-end tests for deeper addon integration (#1102).
//!
//! These drive the *production* gateway path — `proxy` → `scrub_output` →
//! `postprocess` → typed adapters — against a **spawned** Node.js MCP server
//! (`tests/fixtures/mcp_stdio_addon.mjs`), with no protocol mocks. They assert:
//!   * L2 spill   — oversized output becomes a `ctx_expand` retrieval handle
//!   * #498        — post-processed output is a deterministic fn of (content, budget)
//!   * security    — secrets are redacted *before* post-processing sees the text
//!   * L3 index    — untyped output is consolidated into the BM25 index
//!   * codebase-pack — Repomix-shaped pack → archive handle + surfaced `outputId`
//!   * code-graph  — edge output → property-graph cross-source edges
//!   * memory      — memory search → `addon_memory` knowledge facts
//!   * compression — a downstream addon runs as a named lean-ctx `Compressor`
//!
//! Skips cleanly when `node` is unavailable. Serialized on one key: every test
//! drives the global session `pool`, so they must never interleave.
//!
//! Isolation model: a single per-process `LEAN_CTX_DATA_DIR` is set once and
//! never reset (data-dir-keyed stores hash the *project root*, so a unique temp
//! project root per test isolates them). This avoids the env race that detached
//! background ingest threads would otherwise hit if each test mutated the global
//! data-dir env around them — exactly the pattern `data_dir::isolated_data_dir`
//! uses internally.

use std::process::{Command, Stdio};
use std::sync::Once;
use std::time::Duration;

use serde_json::{Map, Value, json};
use serial_test::serial;

use lean_ctx::core::bm25_index::BM25Index;
use lean_ctx::core::extension_registry::Compressor;
use lean_ctx::core::gateway::adapters::compression::GatewayCompressor;
use lean_ctx::core::gateway::{GatewayConfig, GatewayServer, TransportKind, pool, proxy};
use lean_ctx::core::knowledge::ProjectKnowledge;
use lean_ctx::core::property_graph::CodeGraph;

/// Absolute path to the Node MCP fixture that emits adapter-shaped payloads.
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/mcp_stdio_addon.mjs"
);

/// Whether a `node` runtime is on PATH (tests assert nothing when it is absent).
fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// Edition-2024 makes `env::set_var` unsafe; the one-time setup runs under a
// `Once` and tests are serialized, so the mutation is race-free in this binary.
fn set_env(key: &str, value: &str) {
    unsafe { std::env::set_var(key, value) };
}
fn unset_env(key: &str) {
    unsafe { std::env::remove_var(key) };
}

/// Point the whole test binary at one private data dir + enable the archive,
/// exactly once. Never reset between tests — per-test isolation comes from a
/// unique project root (stores are keyed by `hash(project_root)`).
fn ensure_env() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("lean-ctx-addon-e2e-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        set_env("LEAN_CTX_DATA_DIR", dir.to_str().unwrap());
        set_env("LEAN_CTX_ARCHIVE", "1");
        unset_env("LEAN_CTX_GATEWAY");
    });
}

/// A gateway server entry that spawns the fixture over real stdio.
fn fixture_server(name: &str, integration: &str) -> GatewayServer {
    GatewayServer {
        name: name.into(),
        transport: TransportKind::Stdio,
        enabled: true,
        command: "node".into(),
        args: vec![FIXTURE.to_string()],
        integration: integration.into(),
        ..Default::default()
    }
}

/// A minimal enabled gateway config wrapping a single server (all post-processing
/// flags off by default — each test opts into exactly what it exercises).
fn base_cfg(server: GatewayServer) -> GatewayConfig {
    GatewayConfig {
        enabled: true,
        servers: vec![server],
        ..Default::default()
    }
}

/// `{"text": <value>}` argument map for the echo/compress tools.
fn text_args(value: impl Into<String>) -> Map<String, Value> {
    let mut args = Map::new();
    args.insert("text".into(), json!(value.into()));
    args
}

/// Poll `cond` (a side-channel store write landing on a background thread) for up
/// to ~6 s. Returns the final state so the caller asserts a real outcome.
fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..60 {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    cond()
}

/// L2: output above the token budget is spilled to the content-addressed archive
/// and the model receives a compact summary + `ctx_expand` handle instead of the
/// verbatim blob.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(gateway_postprocess)]
async fn proxy_spills_oversized_output_to_a_retrieval_handle() {
    if !node_available() {
        eprintln!("skipping spill E2E: `node` unavailable");
        return;
    }
    ensure_env();
    pool::clear();

    let mut cfg = base_cfg(fixture_server("addonfix", ""));
    cfg.handle_spill = true;
    cfg.output_budget_tokens = 16;

    // Multi-line so the spill's 20-line head summary is far smaller than the
    // full payload (a single giant line would be kept whole).
    let big: String = (0..400)
        .map(|i| format!("alpha beta gamma delta line {i}\n"))
        .collect();
    let out = proxy(&cfg, "addonfix::echo", text_args(big.clone()), "")
        .await
        .expect("proxy spill call");

    assert!(
        out.contains("ctx_expand"),
        "oversized output must spill to a retrieval handle, got: {out}"
    );
    assert!(
        out.len() < big.len(),
        "the handle ({} bytes) must be smaller than the verbatim payload ({} bytes)",
        out.len(),
        big.len()
    );

    pool::clear();
}

/// #498: with L1 compression on, the post-processed text is byte-identical across
/// two independent calls with the same input — provider prompt-caching safe.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(gateway_postprocess)]
async fn proxy_compressed_output_is_deterministic() {
    if !node_available() {
        eprintln!("skipping determinism E2E: `node` unavailable");
        return;
    }
    ensure_env();
    pool::clear();

    let mut cfg = base_cfg(fixture_server("addonfix", ""));
    cfg.compress_output = true;
    cfg.output_budget_tokens = 24;

    let text: String = (0..200)
        .map(|i| format!("config option {i} = value-{i}\n"))
        .collect();

    let first = proxy(&cfg, "addonfix::echo", text_args(text.clone()), "")
        .await
        .expect("first compress call");
    let second = proxy(&cfg, "addonfix::echo", text_args(text.clone()), "")
        .await
        .expect("second compress call");

    assert_eq!(
        first, second,
        "post-processed output must be a deterministic fn of (content, budget) (#498)"
    );
    assert!(!first.is_empty(), "compression must not erase the output");

    pool::clear();
}

/// Security: `scrub_output` runs inside `proxy` *before* post-processing, so a
/// secret in downstream output never reaches the model — even with all flags off.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(gateway_postprocess)]
async fn proxy_redacts_secrets_before_postprocessing() {
    if !node_available() {
        eprintln!("skipping redaction E2E: `node` unavailable");
        return;
    }
    ensure_env();
    pool::clear();

    let cfg = base_cfg(fixture_server("addonfix", ""));
    // A GitHub PAT the addon tries to echo back — the redaction layer's
    // unit test proves this exact shape is caught regardless of config.
    let secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
    let out = proxy(
        &cfg,
        "addonfix::echo",
        text_args(format!("api_key={secret} trailing")),
        "",
    )
    .await
    .expect("proxy redaction call");

    assert!(
        !out.contains(secret),
        "the secret must be redacted before it reaches the model, got: {out}"
    );
    assert!(
        out.contains("trailing"),
        "redaction must be surgical (non-secret text preserved), got: {out}"
    );

    pool::clear();
}

/// L3: untyped (no `integration`) output is consolidated into the project's BM25
/// index on a background thread, so `ctx_search` finds it under the addon's URI.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(gateway_postprocess)]
async fn proxy_indexes_untyped_output_for_search() {
    if !node_available() {
        eprintln!("skipping L3 index E2E: `node` unavailable");
        return;
    }
    ensure_env();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap().to_string();
    pool::clear();

    let mut cfg = base_cfg(fixture_server("addonfix", ""));
    cfg.index_output = true;

    let marker = "zylophonicmarker";
    proxy(
        &cfg,
        "addonfix::echo",
        text_args(format!(
            "{marker} the refund handler lives in src/payments/refund_engine.rs"
        )),
        &root,
    )
    .await
    .expect("proxy index call");

    let indexed = wait_until(|| {
        BM25Index::load(project.path())
            .map(|idx| {
                idx.search(marker, 5)
                    .iter()
                    .any(|h| h.file_path.starts_with("addonfix://tool_output/"))
            })
            .unwrap_or(false)
    });
    assert!(
        indexed,
        "untyped gateway output must be consolidated into the BM25 index (L3)"
    );

    pool::clear();
}

/// codebase-pack (Repomix): a `pack_codebase` result is archived verbatim and the
/// model gets a structure summary + the surfaced `outputId` + a `ctx_expand`
/// handle — lean-ctx becomes the single retrieval layer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(gateway_postprocess)]
async fn proxy_codebase_pack_returns_a_handle_with_outputid() {
    if !node_available() {
        eprintln!("skipping codebase-pack E2E: `node` unavailable");
        return;
    }
    ensure_env();
    pool::clear();

    let cfg = base_cfg(fixture_server("addonfix", "codebase-pack"));
    let out = proxy(&cfg, "addonfix::pack_codebase", Map::new(), "")
        .await
        .expect("proxy codebase-pack call");

    assert!(out.contains("rmx_e2e_001"), "surfaces repomix outputId: {out}");
    assert!(out.contains("ctx_expand"), "offers a retrieval handle: {out}");
    assert!(
        out.contains("directoryStructure"),
        "keeps a structure summary: {out}"
    );

    pool::clear();
}

/// code-graph (Graphify): edge output is folded into the property graph as
/// cross-source edges (so `ctx_callgraph` benefits) on a background thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(gateway_postprocess)]
async fn proxy_code_graph_ingests_cross_source_edges() {
    if !node_available() {
        eprintln!("skipping code-graph E2E: `node` unavailable");
        return;
    }
    ensure_env();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap().to_string();
    pool::clear();

    let mut cfg = base_cfg(fixture_server("addonfix", "code-graph"));
    cfg.index_output = true;

    proxy(&cfg, "addonfix::query_graph", Map::new(), &root)
        .await
        .expect("proxy code-graph call");

    // Read through ONE long-lived connection and poll read-only `SELECT`s: each
    // query sees the detached writer's latest commit, while re-opening would run
    // `CREATE TABLE` (a write) every tick and starve that writer of the SQLite
    // lock (the production ingest tolerates a lost race by design).
    let pg = CodeGraph::open(root.as_str()).expect("open property graph");
    let ingested = wait_until(|| {
        pg.all_cross_source_edges()
            .iter()
            .any(|e| e.from == "src/auth.rs" && e.to == "src/db.rs")
    });
    assert!(
        ingested,
        "code-graph output must become property-graph cross-source edges"
    );

    pool::clear();
}

/// memory (Mem0 & co.): a memory-search result becomes `addon_memory` knowledge
/// facts (so `ctx_knowledge` recalls them) on a background thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(gateway_postprocess)]
async fn proxy_memory_ingests_knowledge_facts() {
    if !node_available() {
        eprintln!("skipping memory E2E: `node` unavailable");
        return;
    }
    ensure_env();
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap().to_string();
    pool::clear();

    let mut cfg = base_cfg(fixture_server("addonfix", "memory"));
    cfg.index_output = true;

    proxy(&cfg, "addonfix::search_memories", Map::new(), &root)
        .await
        .expect("proxy memory call");

    let remembered = wait_until(|| {
        ProjectKnowledge::load(root.as_str())
            .map(|k| {
                k.facts
                    .iter()
                    .any(|f| f.category == "addon_memory" && f.value.contains("structured logging"))
            })
            .unwrap_or(false)
    });
    assert!(
        remembered,
        "memory-search output must become addon_memory knowledge facts"
    );

    pool::clear();
}

/// compression (Headroom/RTK): a downstream compression addon, configured with
/// `integration = "compression"`, runs through the gateway when invoked via the
/// `GatewayCompressor` — the real config → resolve → spawn → call → scrub path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial(gateway_postprocess)]
async fn gateway_compressor_roundtrips_through_a_downstream_addon() {
    if !node_available() {
        eprintln!("skipping compression E2E: `node` unavailable");
        return;
    }
    ensure_env();
    let config_dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "[gateway]\nenabled = true\n\n\
         [[gateway.servers]]\nname = \"addonfix\"\ntransport = \"stdio\"\n\
         command = \"node\"\nargs = [\"{FIXTURE}\"]\nintegration = \"compression\"\n"
    );
    std::fs::write(config_dir.path().join("config.toml"), toml).unwrap();
    set_env("LEAN_CTX_CONFIG_DIR", config_dir.path().to_str().unwrap());
    pool::clear();

    let compressor = GatewayCompressor::new("addonfix");
    let out = compressor.compress("hello world", None);

    assert_eq!(
        out, "compressed:hello world",
        "the downstream compression addon must run through the gateway"
    );

    pool::clear();
    unset_env("LEAN_CTX_CONFIG_DIR");
}
