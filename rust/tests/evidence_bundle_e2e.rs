//! Evidence bundle generator proofs (GL #425 — determinism + content).
//!
//! The independent verification side (signature, chain replay, 1-byte
//! mutation detection) is tested in `packages/leanctx-verify/tests/` —
//! deliberately against the contract, not against this generator.

use lean_ctx::core::audit_trail::{self, AuditEntryData, AuditEventType};
use lean_ctx::core::evidence_bundle::{BundleSpec, generate};
use serial_test::serial;
use std::io::Read;

#[test]
#[serial]
fn bundle_is_deterministic_and_complete() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", tmp.path()) };

    for (i, tool) in ["ctx_read", "ctx_search", "ctx_shell"].iter().enumerate() {
        audit_trail::record(AuditEntryData {
            agent_id: "agent-1".into(),
            tool: (*tool).to_string(),
            action: None,
            input_hash: audit_trail::hash_input(&serde_json::Map::new()),
            output_tokens: i as u32,
            role: "developer".into(),
            event_type: AuditEventType::ToolCall,
        });
    }

    let now = chrono::Utc::now();
    let spec = |out: &std::path::Path| BundleSpec {
        from: (now - chrono::Duration::hours(1)).to_rfc3339(),
        to: (now + chrono::Duration::hours(1)).to_rfc3339(),
        framework: Some("eu-ai-act".to_string()),
        pack: None,
        out: Some(out.to_path_buf()),
    };

    let out_a = tmp.path().join("a.zip");
    let out_b = tmp.path().join("b.zip");
    let a = generate(&spec(&out_a)).expect("bundle a");
    let b = generate(&spec(&out_b)).expect("bundle b");

    // Determinism: identical inputs ⇒ identical bytes ⇒ identical hash.
    assert_eq!(
        a.sha256, b.sha256,
        "same input must produce the same bundle hash"
    );
    assert_eq!(
        std::fs::read(&out_a).expect("a"),
        std::fs::read(&out_b).expect("b"),
        "byte-identical archives"
    );
    assert_eq!(a.entries, 3);

    // Completeness: contract layout present.
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(std::fs::read(&out_a).expect("a"))).expect("zip");
    let names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).expect("entry").name().to_string())
        .collect();
    assert!(names.contains(&"manifest.json".to_string()));
    assert!(names.contains(&"audit/trail.jsonl".to_string()));
    assert!(names.contains(&"coverage/cgb.json".to_string()));
    assert!(names.contains(&"coverage/eu-ai-act.json".to_string()));
    assert!(
        names
            .iter()
            .any(|n| n.starts_with("policies/") && n.ends_with(".resolved.json"))
    );

    // Manifest invariants: chain bounds match the recorded segment and the
    // manifest carries a signature over a recomputable digest.
    let mut manifest_raw = String::new();
    archive
        .by_name("manifest.json")
        .expect("manifest")
        .read_to_string(&mut manifest_raw)
        .expect("read");
    let manifest: serde_json::Value = serde_json::from_str(&manifest_raw).expect("json");
    assert_eq!(manifest["version"], 1);
    assert_eq!(manifest["chain"]["entries"], 3);
    assert_eq!(manifest["chain"]["anchor_prev_hash"], "genesis");
    assert_eq!(manifest["signing"]["algorithm"], "ed25519");
    assert_eq!(
        manifest["signing"]["signature"].as_str().map(str::len),
        Some(128),
        "ed25519 signature present"
    );

    // No wall-clock fields: the manifest must not contain a created_at.
    assert!(manifest.get("created_at").is_none());

    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}

/// Regression (GL #425): concurrent appends from multiple threads/handles
/// forked the chain when `prev_hash` came from a per-process cache. With the
/// advisory file lock + tail read, N concurrent writers must produce ONE
/// valid chain of N entries.
#[test]
#[serial]
fn concurrent_appends_do_not_fork_the_chain() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", tmp.path()) };

    let threads: Vec<_> = (0..4)
        .map(|t| {
            std::thread::spawn(move || {
                for i in 0..25 {
                    audit_trail::record(AuditEntryData {
                        agent_id: format!("agent-{t}"),
                        tool: format!("tool-{i}"),
                        action: None,
                        input_hash: audit_trail::hash_input(&serde_json::Map::new()),
                        output_tokens: i,
                        role: "developer".into(),
                        event_type: AuditEventType::ToolCall,
                    });
                }
            })
        })
        .collect();
    for t in threads {
        t.join().expect("thread");
    }

    let chain = audit_trail::verify_chain();
    assert_eq!(chain.total_entries, 100, "all appends persisted");
    assert!(
        chain.valid,
        "no fork: first_invalid_at = {:?}",
        chain.first_invalid_at
    );

    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}

#[test]
#[serial]
fn empty_period_is_an_error_not_an_empty_attestation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", tmp.path()) };

    audit_trail::record(AuditEntryData {
        agent_id: "agent-1".into(),
        tool: "ctx_read".into(),
        action: None,
        input_hash: audit_trail::hash_input(&serde_json::Map::new()),
        output_tokens: 1,
        role: "developer".into(),
        event_type: AuditEventType::ToolCall,
    });

    let spec = BundleSpec {
        from: "1999-01-01T00:00:00Z".to_string(),
        to: "1999-01-02T00:00:00Z".to_string(),
        framework: None,
        pack: None,
        out: Some(tmp.path().join("never.zip")),
    };
    let err = generate(&spec).expect_err("empty period must fail");
    assert!(err.contains("no audit entries"), "{err}");

    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}
