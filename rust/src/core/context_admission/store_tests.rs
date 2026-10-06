// SPDX-License-Identifier: Apache-2.0
//! Derived stores, recovery and provider admission (G5): owner decision E3
//! and plan cases 29–40, end to end through the real stores. Credential
//! fixtures are split with `concat!` so the repository's own secret scanners
//! never see a literal key.

use std::collections::HashMap;
use std::path::Path;

use super::stores::{StoreAdmission, current_epoch};
use super::*;
use crate::core::bm25_index::BM25Index;

const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
const VALID_CARD: &str = "4111 1111 1111 1111";

struct Isolated {
    _data: crate::core::data_dir::IsolatedDataDir,
}

fn isolated(config: &str) -> Isolated {
    let data = crate::core::data_dir::isolated_data_dir();
    write_config(config);
    Isolated { _data: data }
}

fn write_config(config: &str) {
    let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.toml"), config).expect("config");
}

fn builtin() -> StoreAdmission {
    StoreAdmission::with_policy(AdmissionPolicy::builtin(
        ContextGatewayConfig::default(),
        SecretDetectionConfig::default(),
    ))
}

fn leaky_source() -> String {
    format!(
        "fn main() {{\n    let aws_key = \"{AWS_KEY}\";\n    let card = \"{VALID_CARD}\";\n}}\n"
    )
}

fn assert_clean(text: &str, what: &str) {
    assert!(!text.contains(AWS_KEY), "{what} carries the raw credential");
    assert!(
        !text.contains(VALID_CARD),
        "{what} carries the raw card number"
    );
}

/// Every file under `dir`, recursively, as lossy text.
fn files_under(dir: &Path) -> Vec<(std::path::PathBuf, String)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else if let Ok(bytes) = std::fs::read(&path) {
            out.push((path, String::from_utf8_lossy(&bytes).into_owned()));
        }
    }
    out
}

// ─── Store admission (E3) ───────────────────────────────────────────────────

#[test]
fn e3_store_admission_masks_values_and_refuses_restricted_sources() {
    let admission = builtin();
    let admitted = admission
        .admit(&leaky_source(), Path::new("src/main.rs"))
        .expect("an ordinary source is admitted");
    assert_clean(&admitted, "admitted store text");
    assert!(
        admitted.contains("fn main()"),
        "the rest of the source survives"
    );

    for restricted in [".env", "deploy/.ssh/config", "home/.aws/credentials"] {
        assert!(
            admission
                .admit("TOKEN=abc\n", Path::new(restricted))
                .is_none(),
            "{restricted} is restricted and must never enter a store"
        );
    }
    let text = admission
        .admit_text(&format!("curl -H 'x-key: {AWS_KEY}'"))
        .expect("a command line is admitted");
    assert_clean(&text, "admitted command line");
}

#[test]
fn e3_bm25_builds_never_index_raw_values_or_restricted_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("lib.rs"), leaky_source()).expect("source");
    std::fs::write(root.join(".env"), format!("AWS_KEY={AWS_KEY}\n")).expect("env");
    let files = vec!["lib.rs".to_string(), ".env".to_string()];
    let admission = builtin();
    let hint = HashMap::new();

    for (label, index) in [
        (
            "sequential",
            BM25Index::build_sequential(root, &hint, &files, &admission),
        ),
        (
            "parallel",
            BM25Index::build_parallel(root, &hint, &files, &admission),
        ),
    ] {
        assert!(!index.chunks.is_empty(), "{label}: lib.rs is indexed");
        for chunk in &index.chunks {
            assert_ne!(chunk.file_path, ".env", "{label}: .env must not be indexed");
            assert_clean(&chunk.content, &format!("{label} chunk"));
            let lowered = AWS_KEY.to_lowercase();
            assert!(
                chunk.tokens.iter().all(|t| !t.contains(&lowered)),
                "{label}: no index token derives from the raw key"
            );
        }
        assert!(
            index.search(AWS_KEY, 10).is_empty(),
            "{label}: a masked value cannot be found"
        );
    }
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn e3_persisted_index_from_another_policy_is_never_served() {
    let _env = isolated("");
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("lib.rs"), "fn alpha() {}\n").expect("source");

    let mut index = BM25Index::build_from_directory(root);
    let current = StoreAdmission::current();
    assert_eq!(index.admission_policy.as_deref(), Some(current.digest()));

    index.admission_policy = Some("another-policy".into());
    index.save(root).expect("save");
    assert!(
        BM25Index::load(root).is_none(),
        "an index admitted under another policy is invalid"
    );
    index.admission_policy = None;
    index.save(root).expect("save");
    assert!(
        BM25Index::load(root).is_none(),
        "a legacy index without a policy binding is invalid"
    );
    let rebuilt = BM25Index::load_or_build(root);
    assert_eq!(rebuilt.admission_policy.as_deref(), Some(current.digest()));
    assert!(
        BM25Index::load(root).is_some(),
        "the rebuilt index is served"
    );
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn policy_change_drops_resident_stores_and_refuses_stale_inserts() {
    let _env = isolated("");
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("a.rs");
    std::fs::write(&file, "fn a() {}\n").expect("file");
    let state = crate::core::content_cache::FileState::from_path(&file).expect("state");

    let before = StoreAdmission::current();
    crate::core::content_cache::insert(&file, state, "fn a() {}\n".into(), &before);
    assert!(crate::core::content_cache::get(&file, state).is_some());

    write_config("[context_gateway]\npii = \"off\"\n");
    let after = StoreAdmission::current();
    assert_ne!(before.digest(), after.digest(), "the policy changed");
    assert!(
        current_epoch() > before.epoch(),
        "observing it advanced the epoch"
    );
    assert!(
        crate::core::content_cache::get(&file, state).is_none(),
        "text admitted under the old policy is dropped"
    );

    // A build still running under the old policy cannot repopulate the cache.
    crate::core::content_cache::insert(&file, state, "fn a() {}\n".into(), &before);
    assert!(crate::core::content_cache::get(&file, state).is_none());
    // Neither can a snapshot that was never observed.
    crate::core::content_cache::insert(&file, state, "fn a() {}\n".into(), &builtin());
    assert!(crate::core::content_cache::get(&file, state).is_none());

    crate::core::content_cache::insert(&file, state, "fn a() {}\n".into(), &after);
    assert!(crate::core::content_cache::get(&file, state).is_some());
}

// ─── Recovery is re-authorized (cases 29–36) ────────────────────────────────

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_29_recovered_text_is_readmitted_under_the_current_policy() {
    let _env = isolated("");
    let recovered = recovery::admit_recovered(&leaky_source(), "archive").expect("admitted");
    assert_clean(&recovered, "recovered text");

    // A policy tightened since storage applies to what is recovered.
    write_config("[context_gateway]\nsecrets = \"block\"\n");
    let refused = recovery::admit_recovered(&leaky_source(), "archive")
        .expect_err("a blocked secret is not recovered");
    assert!(refused.contains("secret.blocked"), "{refused}");
    assert_clean(&refused, "the refusal");
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_30_archives_store_and_return_admitted_text_only() {
    let _env = isolated("");
    let output = format!("{}\n{}", leaky_source(), "row\n".repeat(400));
    let id = crate::core::archive::store(
        "ctx_shell",
        &format!("deploy --key {AWS_KEY}"),
        &output,
        None,
    )
    .expect("archived");
    for (path, text) in files_under(&crate::core::data_dir::lean_ctx_data_dir().expect("dir")) {
        assert_clean(&text, &path.display().to_string());
    }
    let expanded = crate::tools::ctx_expand::handle(&serde_json::json!({ "id": id }));
    assert!(expanded.contains("fn main()"), "{expanded}");
    assert_clean(&expanded, "ctx_expand archive");
    let searched = crate::tools::ctx_expand::handle(
        &serde_json::json!({"action": "search_all", "query": "deploy"}),
    );
    assert_clean(&searched, "ctx_expand search_all");
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_31_tee_store_holds_and_returns_admitted_text_only() {
    let _env = isolated("");
    let output = format!("{}\n{}", leaky_source(), "line\n".repeat(200));
    let handle = crate::proxy::ccr::persist(&output).expect("tee handle");
    let on_disk = std::fs::read_to_string(&handle).expect("tee file");
    assert_clean(&on_disk, "tee file");
    let recovered = crate::proxy::ccr::read_tee_detailed(Path::new(&handle)).expect("read");
    assert_clean(&recovered, "recovered tee");
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_32_session_cache_recovery_readmits_cached_and_fresh_copies() {
    let _env = isolated("");
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("handover.rs");
    std::fs::write(&file, leaky_source()).expect("file");
    let path = file.to_str().expect("utf-8 path");
    let mut cache = crate::core::cache::SessionCache::new();
    cache.store(path, &leaky_source());

    let (cached, _) = cache
        .current_full_content(path)
        .expect("entry")
        .expect("admitted");
    assert_clean(&cached, "cached copy");

    // Edited on disk: the fresh re-read is admitted too.
    std::fs::write(&file, format!("// edited\n{}", leaky_source())).expect("edit");
    let (fresh, _) = cache
        .current_full_content(path)
        .expect("entry")
        .expect("admitted");
    assert!(fresh.contains("// edited"), "the current version is served");
    assert_clean(&fresh, "fresh copy");
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_33_handoffs_persist_admitted_text_and_skip_restricted_files() {
    let _env = isolated("");
    let project = tempfile::tempdir().expect("tempdir");
    let root = project.path().to_str().expect("utf-8 root");
    std::fs::write(project.path().join("notes.rs"), leaky_source()).expect("notes");
    std::fs::write(project.path().join(".env"), format!("K={AWS_KEY}\n")).expect("env");
    let cache = crate::core::cache::SessionCache::new();

    let out = crate::tools::ctx_share::handle(
        "push",
        Some("agent-a"),
        None,
        Some("notes.rs,.env"),
        Some(&format!("use {AWS_KEY}")),
        &cache,
        root,
    );
    assert!(out.contains("Shared 1 files"), "{out}");
    assert!(out.contains("Withheld by the context gateway"), "{out}");
    let data = crate::core::data_dir::lean_ctx_data_dir().expect("dir");
    for (path, text) in files_under(&data).into_iter().chain(
        files_under(project.path())
            .into_iter()
            .filter(|(p, _)| p.extension().is_some_and(|e| e == "json")),
    ) {
        assert_clean(&text, &path.display().to_string());
    }
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_34_knowledge_stores_admitted_facts_and_recalls_through_the_gateway() {
    let _env = isolated("");
    let mut knowledge = crate::core::knowledge::ProjectKnowledge::new("/tmp/g5-knowledge");
    let policy = crate::core::memory_policy::MemoryPolicy::default();
    knowledge.remember(
        "deploy",
        "credentials",
        &format!("key {AWS_KEY}, card {VALID_CARD}"),
        "s1",
        0.9,
        &policy,
    );
    let fact = knowledge.facts.last().expect("fact stored");
    assert_clean(&fact.value, "stored fact");
    assert!(fact.value.starts_with("key "), "{}", fact.value);
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_35_context_packages_are_admitted_on_save_and_on_resume() {
    let _env = isolated("");
    let project = tempfile::tempdir().expect("tempdir");
    let root = project.path().to_str().expect("utf-8 root");
    let mut session = crate::core::session::SessionState::new();
    session.set_task(&format!("rotate {AWS_KEY}"), None);
    session.add_finding(
        Some("src/main.rs"),
        Some(2),
        &format!("card {VALID_CARD} in fixture"),
    );

    let path = crate::core::context_package::save_package(&session, root, None, None, None)
        .expect("package saved");
    let on_disk = std::fs::read_to_string(&path).expect("package");
    assert_clean(&on_disk, "saved package");
    assert!(
        on_disk.contains("in fixture"),
        "the finding survives, masked"
    );

    // A package written elsewhere (or before admission) is re-admitted.
    let foreign = project.path().join("foreign.ctx.json");
    std::fs::write(
        &foreign,
        on_disk.replace("in fixture", &format!("key {AWS_KEY}")),
    )
    .expect("foreign package");
    let mut target = crate::core::session::SessionState::new();
    crate::core::context_package::resume_package(&mut target, &foreign).expect("resumed");
    for finding in &target.findings {
        assert_clean(&finding.summary, "resumed finding");
    }
}

// ─── Provider admission (cases 37–40) ───────────────────────────────────────

fn provider_chunk() -> crate::core::content_chunk::ContentChunk {
    crate::core::content_chunk::ContentChunk::from_provider(
        "github",
        "issue",
        "42",
        &format!("Leaked {AWS_KEY}"),
        crate::core::bm25_index::ChunkKind::Other,
        format!("The deploy key is {AWS_KEY} and the card {VALID_CARD}. See src/main.rs."),
        vec![format!("https://ci.example/run?token={AWS_KEY}")],
        Some(serde_json::json!({"author_card": VALID_CARD, "labels": ["bug"]})),
    )
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_37_provider_chunks_are_admitted_before_any_artifact_is_derived() {
    let _env = isolated("");
    let artifacts = crate::core::consolidation::consolidate(&[provider_chunk()]);
    assert!(
        !artifacts.bm25_chunks.is_empty(),
        "the issue is kept, masked"
    );
    let debug = format!("{artifacts:?}");
    assert_clean(&debug, "consolidation artifacts");
    let chunk = &artifacts.bm25_chunks[0];
    let lowered = AWS_KEY.to_lowercase();
    assert!(chunk.tokens.iter().all(|t| !t.contains(&lowered)));
    assert_eq!(
        chunk
            .metadata
            .as_ref()
            .and_then(|m| m.get("labels"))
            .cloned(),
        Some(serde_json::json!(["bug"])),
        "masked metadata keeps its structure"
    );
}

#[test]
fn case_38_artifacts_built_elsewhere_are_admitted_at_the_store_boundary() {
    let raw = crate::core::consolidation::ConsolidationArtifacts {
        bm25_chunks: vec![provider_chunk()],
        edges: vec![crate::core::graph_index::IndexEdge {
            from: "src/main.rs".into(),
            to: format!("github://issue/42?token={AWS_KEY}"),
            kind: "mentions".into(),
            weight: 1.0,
        }],
        facts: vec![crate::core::knowledge_provider_extract::ExtractedFact {
            origin: crate::core::knowledge::FactOrigin::Unverified,
            category: "deploy".into(),
            key: "key".into(),
            value: AWS_KEY.into(),
            confidence: 0.5,
        }],
        cache_entries: vec![crate::core::consolidation::CacheableProviderResult {
            uri: "github://issue/42".into(),
            content: leaky_source(),
            token_count: 10,
        }],
    };
    let (admitted, dropped) = provider::admit_artifacts(&raw, &builtin());
    assert_eq!(dropped, 0, "masking suffices for every object");
    assert_clean(&format!("{admitted:?}"), "admitted artifacts");
    assert_eq!(admitted.edges.len(), 1);
    assert_eq!(admitted.facts.len(), 1);
}

#[test]
fn case_39_a_withheld_provider_object_is_dropped_whole() {
    let config = ContextGatewayConfig {
        secrets: FilterAction::Block,
        ..ContextGatewayConfig::default()
    };
    let blocking = StoreAdmission::with_policy(AdmissionPolicy::builtin(
        config,
        SecretDetectionConfig::default(),
    ));
    assert!(provider::admit_chunk(&provider_chunk(), &blocking).is_none());
    let clean = crate::core::content_chunk::ContentChunk::from_provider(
        "github",
        "issue",
        "7",
        "Flaky test",
        crate::core::bm25_index::ChunkKind::Other,
        "The integration test times out on CI.".into(),
        Vec::new(),
        None,
    );
    let admitted = provider::admit_chunk(&clean, &blocking).expect("clean chunk kept");
    assert_eq!(admitted.content, clean.content);
    assert_eq!(
        admitted.tokens, clean.tokens,
        "unchanged text keeps its tokens"
    );
}

/// What store admission costs a full BM25 build of this crate's sources.
/// Run in release:
/// `cargo test --release --lib bm25_build_admission_cost -- --ignored --nocapture`.
#[test]
#[ignore = "measurement, not a correctness test"]
fn bm25_build_admission_cost() {
    use std::time::{Duration, Instant};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<String> = walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
        .filter_map(|e| {
            let rel = e.path().strip_prefix(&root).ok()?;
            Some(rel.to_string_lossy().into_owned())
        })
        .collect();
    files.sort();
    let bytes: u64 = files
        .iter()
        .filter_map(|rel| std::fs::metadata(root.join(rel)).ok())
        .map(|meta| meta.len())
        .sum();
    let hint = HashMap::new();
    let disabled = StoreAdmission::with_policy(AdmissionPolicy::builtin(
        ContextGatewayConfig {
            enabled: false,
            ..ContextGatewayConfig::default()
        },
        SecretDetectionConfig::default(),
    ));
    let enabled = builtin();
    let median = |admission: &StoreAdmission, cold: bool| -> Duration {
        let mut runs: Vec<Duration> = (0..5)
            .map(|_| {
                if cold {
                    clear_clean_memo();
                }
                let started = Instant::now();
                let index = BM25Index::build_parallel(&root, &hint, &files, admission);
                let elapsed = started.elapsed();
                assert!(!index.chunks.is_empty());
                elapsed
            })
            .collect();
        runs.sort();
        runs[2]
    };
    let base = median(&disabled, true);
    let cold = median(&enabled, true);
    let warm = median(&enabled, false);
    let pct = |d: Duration| (d.as_secs_f64() / base.as_secs_f64() - 1.0) * 100.0;
    println!(
        "{} files, {:.1} MB · gateway off {base:?} · admitted cold {cold:?} ({:+.1}%) · admitted, memo warm {warm:?} ({:+.1}%)",
        files.len(),
        bytes as f64 / 1e6,
        pct(cold),
        pct(warm)
    );
}
