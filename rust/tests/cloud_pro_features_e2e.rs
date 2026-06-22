//! Live, `#[ignore]`d end-to-end proof for the **whole advertised Pro surface**,
//! including the literal "my context follows me to another machine" experience.
//!
//! It drives the real engine client (`cloud_client`) against a real open backend
//! wired to a billing stub that resolves the caller's plan. The one-shot harness
//! `scripts/cloud_pro_features_e2e.sh` provisions an ephemeral Postgres + backend
//! + stub and runs this test once per phase via `LEANCTX_E2E_PHASE`:
//!
//! - `device-a` — a Pro machine *pushes* every bucket (knowledge, gotchas, the
//!   five telemetry streams, and the hosted index bundle).
//! - `device-b` — a *different* Pro machine (separate data dir, same account,
//!   same repo identity) *restores*: `pull_knowledge` returns the entry and
//!   `pull_index_bundle` reconstructs the index artifacts. This is the real
//!   cross-device hand-off.
//! - `free`     — a Free account is refused (`402`) on all eight gated buckets.
//!
//! Env (set by the harness):
//! - `LEAN_CTX_API_URL`    — backend base URL
//! - `LEAN_CTX_DATA_DIR`   — isolated data dir with `cloud/credentials.json`
//! - `LEANCTX_E2E_PHASE`   — `device-a` | `device-b` | `free`
//! - `LEANCTX_E2E_PROJECT` — scratch project root (device-a / device-b phases)
//!
//! Ciphertext-at-rest is asserted out-of-band by the harness, which greps
//! `knowledge_blobs.blob`, `gotcha_blobs.blob` and `index_bundles.bytes` for the
//! needle below — it must appear in none of them.

use lean_ctx::cloud_client;
use serde_json::{Value, json};
use std::path::PathBuf;

/// Secret embedded in all three *encrypted* buckets; the harness greps every
/// at-rest table for it and it must never appear in plaintext.
const NEEDLE: &str = "PRO-E2E-NEEDLE-9b1e4d77";

#[test]
#[ignore = "live E2E: needs a running backend + billing stub (run scripts/cloud_pro_features_e2e.sh)"]
fn pro_features_cross_device() {
    match std::env::var("LEANCTX_E2E_PHASE").as_deref() {
        Ok("device-a") => device_a_push(),
        Ok("device-b") => device_b_restore(),
        Ok("free") => free_is_gated(),
        other => panic!("set LEANCTX_E2E_PHASE=device-a|device-b|free (got {other:?})"),
    }
}

// ── Schema-valid payloads (mirror the server structs so the `Json<T>` extractor
//    accepts them and execution reaches the entitlement gate) ──────────────────

fn knowledge_entry() -> Value {
    json!({
        "category": "decision", "key": "cross-device", "value": NEEDLE,
        "updated_by": "pro@example.com", "updated_at": "2026-01-01T00:00:00Z",
    })
}
fn gotcha_entry() -> Value {
    json!({
        "pattern": NEEDLE, "fix": "seal client-side", "severity": "high",
        "category": "e2e", "occurrences": 1, "prevented_count": 0, "confidence": 0.9,
    })
}
fn command_entry() -> Value {
    json!({ "command": "cargo test", "source": "e2e", "count": 1, "tokens_saved": 10 })
}
fn cep_entry() -> Value {
    json!({ "recorded_at": "2026-01-01T00:00:00Z", "score": 0.87, "tokens_saved": 1234 })
}
fn gain_entry() -> Value {
    json!({
        "recorded_at": "2026-01-01T00:00:00Z", "total": 0.8, "compression": 0.7,
        "cost_efficiency": 0.6, "quality": 0.9, "consistency": 0.85,
    })
}
fn buddy_state() -> Value {
    json!({ "name": "e2e", "species": "otter", "level": 2, "xp": 10, "streak_days": 3 })
}
fn feedback_entry() -> Value {
    json!({ "language": "rust", "entropy": 0.5, "jaccard": 0.6, "sample_count": 10, "avg_efficiency": 0.8 })
}

fn project_root() -> PathBuf {
    PathBuf::from(std::env::var("LEANCTX_E2E_PROJECT").expect("LEANCTX_E2E_PROJECT"))
}

/// The hosted-index artifact dir for `root`, discovered from the engine's own
/// `NothingToBundle` error so the test needs no crate-internal path helper.
fn discover_vectors_dir(root: &std::path::Path) -> PathBuf {
    match cloud_client::push_index_bundle(root) {
        Ok((hash, _)) => panic!("expected NothingToBundle on an empty project, got hash {hash}"),
        Err(e) => {
            let raw = e
                .split("found in ")
                .nth(1)
                .and_then(|s| s.split(" \u{2014}").next())
                .unwrap_or_else(|| panic!("unexpected index error: {e}"))
                .trim();
            PathBuf::from(raw)
        }
    }
}

/// Device A: a Pro machine pushes every advertised bucket.
fn device_a_push() {
    let knowledge = knowledge_entry();
    assert!(
        cloud_client::push_knowledge(std::slice::from_ref(&knowledge))
            .expect("device-a: push_knowledge")
            .contains("synced")
    );

    let gotcha = gotcha_entry();
    cloud_client::push_gotchas(std::slice::from_ref(&gotcha)).expect("device-a: push_gotchas");

    // Five plaintext-telemetry buckets (server-readable by design; excluded from
    // the E2E claim) — each must pass the Pro gate and store.
    let command = command_entry();
    assert!(
        cloud_client::push_commands(std::slice::from_ref(&command))
            .expect("device-a: push_commands")
            .contains("synced")
    );
    let cep = cep_entry();
    assert!(
        cloud_client::push_cep(std::slice::from_ref(&cep))
            .expect("device-a: push_cep")
            .contains("synced")
    );
    let gain = gain_entry();
    assert!(
        cloud_client::push_gain(std::slice::from_ref(&gain))
            .expect("device-a: push_gain")
            .contains("synced")
    );
    cloud_client::push_buddy(&buddy_state()).expect("device-a: push_buddy");
    let feedback = feedback_entry();
    cloud_client::push_feedback(std::slice::from_ref(&feedback)).expect("device-a: push_feedback");

    // Hosted Personal Index: build the two artifacts, then pack→encrypt→upload.
    let root = project_root();
    let dir = discover_vectors_dir(&root);
    std::fs::create_dir_all(&dir).expect("create vectors dir");
    std::fs::write(dir.join("bm25_index.bin.zst"), b"e2e-bm25-artifact-bytes").unwrap();
    let embeddings = json!({ "vectors": [{ "id": "e2e", "value": NEEDLE }] });
    std::fs::write(
        dir.join("embeddings.json"),
        serde_json::to_vec(&embeddings).unwrap(),
    )
    .unwrap();
    let (_hash, size) =
        cloud_client::push_index_bundle(&root).expect("device-a: push_index_bundle");
    assert!(size > 0, "encrypted bundle should be non-empty");

    // Personal Cloud dashboard (`lean-ctx cloud status` / leanctx.com/account/cloud)
    // must reflect the active Pro entitlement.
    let dash = cloud_client::fetch_account_cloud().expect("device-a: fetch_account_cloud");
    assert_eq!(
        dash.get("cloud_sync").and_then(Value::as_bool),
        Some(true),
        "dashboard must report cloud_sync for Pro: {dash}"
    );
    assert_eq!(
        dash.get("plan").and_then(Value::as_str),
        Some("pro"),
        "dashboard must report the Pro plan: {dash}"
    );

    println!(
        "DEVICE-A-OK: knowledge+gotchas+commands+cep+gain+buddy+feedback+index pushed; dashboard=pro"
    );
}

/// Device B: a *different* machine on the same account restores Device A's data.
fn device_b_restore() {
    // Knowledge: the vault key derives from the account API key alone, so a fresh
    // machine with only the credentials reconstructs the store.
    let pulled = cloud_client::pull_knowledge().expect("device-b: pull_knowledge");
    assert!(
        pulled
            .iter()
            .any(|e| e.get("value").and_then(|v| v.as_str()) == Some(NEEDLE)),
        "device-b did not restore Device A's knowledge entry: {pulled:#?}"
    );

    // Hosted index: same repo identity → same bucket. Pull reconstructs the
    // artifacts into this machine's (previously empty) vectors dir.
    let root = project_root();
    let dir = discover_vectors_dir(&root); // empty here → reveals device B's dir
    cloud_client::pull_index_bundle(&root).expect("device-b: pull_index_bundle");
    let restored = std::fs::read_to_string(dir.join("embeddings.json"))
        .expect("device-b: pull must restore embeddings.json");
    assert!(
        restored.contains(NEEDLE),
        "device-b restored a bundle without the needle"
    );

    println!("DEVICE-B-OK: restored knowledge + hosted index from another device");
}

/// Free plan: every gated bucket must be refused with `402`. Against the same
/// healthy backend the Pro device just used, the only reason these fail is the
/// entitlement gate — a clean differential proof of the paywall.
fn free_is_gated() {
    let knowledge = knowledge_entry();
    let gotcha = gotcha_entry();
    let command = command_entry();
    let cep = cep_entry();
    let gain = gain_entry();
    let feedback = feedback_entry();

    gate_blocks(
        "push_knowledge",
        cloud_client::push_knowledge(std::slice::from_ref(&knowledge)),
    );
    gate_blocks(
        "push_commands",
        cloud_client::push_commands(std::slice::from_ref(&command)),
    );
    gate_blocks(
        "push_cep",
        cloud_client::push_cep(std::slice::from_ref(&cep)),
    );
    gate_blocks(
        "push_gotchas",
        cloud_client::push_gotchas(std::slice::from_ref(&gotcha)),
    );
    gate_blocks("push_buddy", cloud_client::push_buddy(&buddy_state()));
    gate_blocks(
        "push_feedback",
        cloud_client::push_feedback(std::slice::from_ref(&feedback)),
    );
    gate_blocks(
        "push_gain",
        cloud_client::push_gain(std::slice::from_ref(&gain)),
    );
    gate_blocks("index_status", cloud_client::index_bundle_status());

    println!("FREE-ALL-GATED-OK: all 8 cloud-sync buckets returned 402");
}

/// Assert a Free-plan call was refused by the payment gate (any `Ok` is a
/// paywall leak). Accepts the `402` status or the gate's prose.
fn gate_blocks<T: std::fmt::Debug>(name: &str, result: Result<T, String>) {
    match result {
        Ok(v) => panic!("PAYWALL LEAK: {name} succeeded for a Free account: {v:?}"),
        Err(e) => {
            println!("  {name}: blocked -> {e}");
            assert!(
                e.contains("402")
                    || e.to_lowercase().contains("payment")
                    || e.to_lowercase().contains("pro"),
                "{name}: expected a 402/payment gate error, got: {e}"
            );
        }
    }
}
