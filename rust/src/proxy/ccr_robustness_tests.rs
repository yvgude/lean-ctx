//! CCR robustness regression suite (#983) — guards the content-addressed
//! recovery path against the classes of bug that bit comparable context layers
//! (Headroom #1023/#1209/#1236/#389/#1141/#1182/#709/#1450): a lossy rewrite
//! that drops a handle it cannot back, a retrieval after the tee file is gone
//! past its TTL, an in-band splice on a streaming request, the tee store
//! colliding with the read-stub bookkeeping, and the cold-stub index resurrecting
//! content across a restart.
//!
//! Lives in-crate (not `rust/tests/`) because the invariants are over
//! `pub(crate)` internals — `ccr::{persist*, resolve_tee, MIN_TEE_BYTES,
//! inband_marker, splice_inband_in_place}` and the read-stub index — that an
//! external integration crate cannot reach without weakening encapsulation.

use serde_json::json;

use crate::core::data_dir::test_env_lock;
use crate::core::hasher::hash_short;
use crate::core::recovery_verify::{RecoveryOutcome, verify_handle};
#[cfg(unix)]
use crate::proxy::ccr::read_tee;
use crate::proxy::ccr::{
    MIN_TEE_BYTES, inband_marker, persist, persist_json, persist_tabular, resolve_tee,
    splice_inband_in_place,
};
use crate::tools::ctx_expand;

/// A verbatim original comfortably above [`MIN_TEE_BYTES`], so the persist gate
/// always mints a handle (the sub-threshold case is its own test).
fn big(seed: &str) -> String {
    let line = format!("{seed} ");
    let mut s = String::new();
    while s.len() < MIN_TEE_BYTES + 64 {
        s.push_str(&line);
        s.push('\n');
    }
    s
}

/// Gap 1 — a lossy crush must never emit a handle it cannot back: below
/// [`MIN_TEE_BYTES`] every producer prefix returns `None`, so the caller keeps
/// the data verbatim instead of dropping a column behind a dead handle.
#[test]
fn persist_below_min_tee_bytes_yields_no_handle_for_any_prefix() {
    let _lock = test_env_lock();
    let small = "too small to bother persisting";
    assert!(small.len() < MIN_TEE_BYTES);
    assert!(persist(small).is_none());
    assert!(persist_json(small).is_none());
    assert!(persist_tabular(small).is_none());
}

/// Gap 2 — handles are content-addressed (idempotent, cache-safe #448/#498) and
/// segregated per producer, so the new `tbl_` store never aliases `proxy_`/`json_`.
#[test]
fn persist_is_idempotent_content_addressed_and_prefix_segregated() {
    let _lock = test_env_lock();
    let body = big("verbatim original row");
    let proxy_a = persist(&body).unwrap();
    let proxy_b = persist(&body).unwrap();
    assert_eq!(proxy_a, proxy_b, "same content -> same handle (cache-safe)");

    let json = persist_json(&body).unwrap();
    let tbl = persist_tabular(&body).unwrap();
    assert!(proxy_a.contains("proxy_") && json.contains("json_") && tbl.contains("tbl_"));
    assert_ne!(proxy_a, json);
    assert_ne!(json, tbl);
    assert_ne!(proxy_a, tbl);
    for h in [&proxy_a, &json, &tbl] {
        assert!(resolve_tee(h).is_some(), "handle resolves: {h}");
    }
}

/// Gap 3 — once the tee file is gone (24h TTL cleanup), retrieval degrades to a
/// graceful not-found message; it never panics and never serves stale content.
#[test]
fn ctx_expand_is_graceful_when_tee_file_deleted_past_ttl() {
    let _lock = test_env_lock();
    let body = big("recoverable until the ttl lapses");
    let handle = persist(&body).unwrap();
    let path = resolve_tee(&handle).expect("resolves before deletion");
    std::fs::remove_file(&path).expect("simulate 24h TTL cleanup");

    let out = ctx_expand::handle(&json!({ "id": handle }));
    assert!(
        out.contains("not found"),
        "graceful message expected: {out}"
    );
    assert!(
        !out.contains("recoverable until"),
        "must not serve stale content"
    );
}

/// Gap 4 — the surgical selectors over a tee handle return exactly the requested
/// slice (the whole point of CCR: pull back a slice, not the entire original).
#[test]
fn ctx_expand_surgical_slices_over_tee_handle() {
    let _lock = test_env_lock();
    let body = (1..=60)
        .map(|i| format!("output row {i:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(body.len() >= MIN_TEE_BYTES);
    let handle = persist(&body).unwrap();

    let head = ctx_expand::handle(&json!({ "id": handle, "head": 2 }));
    assert!(head.contains("output row 001") && head.contains("output row 002"));
    assert!(
        !head.contains("output row 010"),
        "head leaked beyond 2: {head}"
    );

    let tail = ctx_expand::handle(&json!({ "id": handle, "tail": 2 }));
    assert!(tail.contains("output row 059") && tail.contains("output row 060"));
    assert!(
        !tail.contains("output row 001"),
        "tail leaked the head: {tail}"
    );

    let search = ctx_expand::handle(&json!({ "id": handle, "search": "row 042" }));
    assert!(search.contains("output row 042") && !search.contains("output row 001"));

    let range = ctx_expand::handle(&json!({ "id": handle, "start_line": 5, "end_line": 6 }));
    assert!(range.contains("output row 005") && range.contains("output row 006"));
    assert!(!range.contains("output row 004") && !range.contains("output row 007"));
}

/// Gap 5 — the tee resolver rejects path traversal, malformed hex (across every
/// prefix incl. the new `tbl_`) and a reference-store id, so a crafted handle can
/// never escape the store or alias another store (store separation + #936 ladder).
#[test]
fn resolve_tee_rejects_traversal_nontee_and_bad_hex_across_prefixes() {
    let _lock = test_env_lock();
    for bad in [
        "/etc/passwd",
        "../../secret",
        "proxy_nothex0000000.log",
        "json_zzzzzzzzzzzzzzzz.log",
        "tbl_zzzzzzzzzzzzzzzz.log",
        "ref_deadbeefdeadbeef", // reference-store id, not a tee
        "deadbeefdeadbeef",     // right shape, no backing file
    ] {
        assert!(resolve_tee(bad).is_none(), "must reject: {bad}");
    }
}

/// Gap 6 — an in-band `<lc_expand:HASH>` marker splices on a *streaming*-shaped
/// request just as on a non-streaming one (the `stream` flag is irrelevant to the
/// recursive string walk).
#[test]
fn inband_marker_splices_on_streaming_shaped_request() {
    let _lock = test_env_lock();
    let body = big("historical streaming line");
    let handle = persist(&body).unwrap();
    let marker = inband_marker(&handle).expect("a proxy tee handle yields a marker");

    let mut req = json!({
        "stream": true,
        "messages": [{ "role": "assistant", "content": format!("recall {marker} now") }],
    });
    assert!(
        splice_inband_in_place(&mut req),
        "a marker on a streaming request must splice"
    );
    let spliced = req["messages"][0]["content"].as_str().unwrap();
    assert!(spliced.contains("historical streaming line"));
    assert!(!spliced.contains("<lc_expand:"), "marker must be consumed");
}

/// Gap 7 — an unbacked (expired/wrong) marker is left verbatim rather than
/// silently deleted, and a marker-less body stays byte-identical (cache-safe).
#[test]
fn inband_splice_keeps_unresolvable_marker_and_is_noop_without_one() {
    let _lock = test_env_lock();
    let mut bad = json!({ "stream": true, "t": "x <lc_expand:deadbeefdeadbeef> y" });
    assert!(
        !splice_inband_in_place(&mut bad),
        "unbacked marker -> reports no change"
    );
    assert_eq!(
        bad["t"].as_str().unwrap(),
        "x <lc_expand:deadbeefdeadbeef> y",
        "kept verbatim, not deleted"
    );

    let mut clean =
        json!({ "stream": true, "messages": [{ "role": "user", "content": "no marker" }] });
    let before = clean.clone();
    assert!(!splice_inband_in_place(&mut clean));
    assert_eq!(clean, before, "marker-less body stays byte-identical");
}

/// Gap 8 — end-to-end for the lossy tabular crusher (#982): the dropped
/// high-entropy column is absent from the emitted text yet fully recoverable
/// out-of-band through the same `ctx_expand` path the footer advertises.
#[test]
fn tabular_lossy_dropped_column_is_recoverable_via_ctx_expand() {
    let _lock = test_env_lock();
    let mut csv = String::from("status,uuid\n");
    for i in 0..50 {
        csv.push_str(&format!("ok,uuid-{i:08}\n"));
    }
    assert!(csv.len() >= MIN_TEE_BYTES);

    let res = crate::core::tabular_crush::crush_text_lossy_if_beneficial(&csv, ',', 0.9)
        .expect("lossy crush drops the high-entropy column");
    assert!(!res.lossless, "dropping a column is lossy");
    assert!(
        !res.text.contains("uuid-00000042"),
        "the dropped value is gone from the text"
    );

    let handle = persist_tabular(&csv).expect("tbl handle");
    let out = ctx_expand::handle(&json!({ "id": handle, "search": "uuid-00000042" }));
    assert!(
        out.contains("uuid-00000042"),
        "dropped datum recoverable out-of-band: {out}"
    );
}

/// Gap 9 — the read-stub index persists only delivery *bookkeeping*, never the
/// file content, and that bookkeeping survives a simulated daemon restart so a
/// re-read collapses to the cheap `[unchanged]` stub (#955).
#[test]
#[serial_test::serial(stub_index)]
fn read_stub_bookkeeping_survives_restart_without_storing_content() {
    use crate::core::read_stub_index as rsi;

    rsi::clear_for_test();
    let dir = tempfile::tempdir().unwrap();
    let secret = "TOP-SECRET-FILE-BODY-MUST-NOT-PERSIST";
    rsi::record(rsi::StubRecord::new(
        "/proj/handover.md".to_string(),
        hash_short(secret), // the hash, never the content
        Some(std::time::SystemTime::now()),
        128,
        "F1".to_string(),
        Some("conv-restart".to_string()),
    ));
    rsi::persist_to_dir(dir.path());

    // Simulate a restart: wipe the in-memory store, then reload from disk.
    rsi::clear_for_test();
    assert!(
        rsi::lookup("/proj/handover.md").is_none(),
        "post-restart memory starts empty"
    );
    rsi::load_from_dir(dir.path());
    let back = rsi::lookup("/proj/handover.md").expect("bookkeeping survived the restart");
    assert_eq!(back.line_count, 128);

    let on_disk =
        std::fs::read_to_string(dir.path().join("read_cache").join("stub_index.json")).unwrap();
    assert!(
        !on_disk.contains(secret),
        "the index must hold bookkeeping only, never file content (#955)"
    );
    rsi::clear_for_test();
}

#[test]
fn recovery_verifier_checks_resolved_bytes_digest_and_expiry() {
    let _lock = test_env_lock();
    let body = big("recovery verification body");
    let handle = persist(&body).expect("tee handle");
    let digest = blake3::hash(body.as_bytes()).to_hex().to_string();

    let verified = verify_handle(&handle, Some(&digest));
    assert_eq!(verified.outcome, RecoveryOutcome::Verified);
    assert_eq!(verified.byte_len, Some(body.len()));
    assert_eq!(verified.digest.as_deref(), Some(digest.as_str()));
    assert!(verified.is_recoverable_for_model());

    let wrong_digest = blake3::hash(b"different content").to_hex().to_string();
    let mismatch = verify_handle(&handle, Some(&wrong_digest));
    assert_eq!(mismatch.outcome, RecoveryOutcome::DigestMismatch);
    assert_eq!(mismatch.byte_len, Some(body.len()));
    assert_eq!(mismatch.digest.as_deref(), Some(digest.as_str()));
    assert!(!mismatch.is_recoverable_for_model());

    let missing_seed = "unique missing recovery handle for cq04";
    let missing_handle = format!("proxy_{}.log", hash_short(missing_seed));
    if let Some(path) = resolve_tee(&missing_handle) {
        std::fs::remove_file(path).expect("clear stale test handle");
    }
    assert_eq!(
        verify_handle(&missing_handle, None).outcome,
        RecoveryOutcome::Missing
    );

    // Storage is admitted (G5): clean text is stored and yields a handle.
    let reference = crate::server::reference_store::store("expired reference")
        .expect("a clean reference is stored");
    crate::server::reference_store::expire_for_test(&reference);
    assert_eq!(
        verify_handle(&reference, None).outcome,
        RecoveryOutcome::Expired
    );

    let overlong = "x".repeat(4097);
    for malformed in [
        "",
        "garbage",
        "wrong_0123456789abcdef.log",
        "proxy_bad",
        "/etc/passwd",
        "%2e%2e/etc/passwd",
        overlong.as_str(),
    ] {
        assert_eq!(
            verify_handle(malformed, None).outcome,
            RecoveryOutcome::Malformed,
            "malformed handle must not resolve: {malformed}"
        );
    }
}

#[test]
fn recovery_paths_reject_traversal_symlink_escape_and_cross_store_handles() {
    let _lock = test_env_lock();
    let body = big("cross-store recovery verification");
    let ccr_handle = persist(&body).expect("tee handle");

    crate::test_env::set_var("LEAN_CTX_ARCHIVE", "1");
    let archive_id =
        crate::core::archive::store("ctx_shell", "cq04", &body, None).expect("archive handle");
    crate::test_env::remove_var("LEAN_CTX_ARCHIVE");

    assert!(crate::core::archive::retrieve(&ccr_handle).is_none());
    assert!(resolve_tee(&archive_id).is_none());
    let digest = blake3::hash(body.as_bytes()).to_hex().to_string();
    assert_eq!(
        verify_handle(&archive_id, Some(&digest)).outcome,
        RecoveryOutcome::Verified
    );

    let tee_root = crate::core::paths::state_dir()
        .expect("state directory")
        .join("tee")
        .canonicalize()
        .expect("tee directory");
    let basename = std::path::Path::new(&ccr_handle)
        .file_name()
        .and_then(|name| name.to_str())
        .expect("tee basename");
    let outside = tempfile::tempdir().expect("outside directory");
    for crafted in [
        format!("../{basename}"),
        outside.path().join(basename).to_string_lossy().into_owned(),
        format!("%2e%2e/{basename}"),
    ] {
        if let Some(resolved) = resolve_tee(&crafted) {
            let canonical = resolved.canonicalize().expect("canonical tee result");
            assert!(
                canonical.starts_with(&tee_root),
                "crafted handle escaped the tee store: {crafted}"
            );
        }
    }
    for traversal in ["../../etc/passwd", "%2e%2e/etc/passwd"] {
        assert!(resolve_tee(traversal).is_none());
    }

    let absolute_base = outside.path().join("absolute");
    std::fs::write(absolute_base.with_extension("txt"), "outside sentinel")
        .expect("write absolute traversal target");
    assert!(
        crate::core::archive::retrieve(&absolute_base.to_string_lossy()).is_none(),
        "absolute archive handles must not read outside the archive store"
    );

    let data_dir = outside.path().join("nested").join("data");
    std::fs::create_dir_all(&data_dir).expect("create archive test data directory");
    std::fs::write(outside.path().join("outside.txt"), "outside sentinel")
        .expect("write relative traversal target");
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data_dir.to_string_lossy().as_ref());
    let escaped = crate::core::archive::retrieve("../../outside");
    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    assert!(
        escaped.is_none(),
        "relative archive traversal must not read outside the archive store"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let target = outside.path().join("symlink-target.txt");
        std::fs::write(&target, "outside symlink sentinel").expect("write symlink target");
        let symlink_body = big("symlink escape recovery handle");
        let symlink_handle = format!("proxy_{}.log", hash_short(&symlink_body));
        let link = crate::core::paths::state_dir()
            .expect("state directory")
            .join("tee")
            .join(format!("proxy_{}.log", hash_short(&symlink_body)));
        let _ = std::fs::remove_file(&link);
        symlink(&target, &link).expect("install outside-store symlink");

        let refused = verify_handle(&symlink_handle, None);
        let ctx_expand_result = crate::tools::ctx_expand::handle(&serde_json::json!({
            "id": symlink_handle
        }));
        std::fs::remove_file(&link).expect("remove outside-store symlink");

        assert!(matches!(refused.outcome, RecoveryOutcome::Refused(_)));
        assert!(resolve_tee(&symlink_handle).is_none());
        assert!(
            !ctx_expand_result.contains("outside symlink sentinel"),
            "ctx_expand must not read through a symlink outside the tee store"
        );

        // The same planted link inside the archive store: a valid archive ID whose
        // content file points outside must not reach the model through ctx_expand.
        crate::test_env::set_var("LEAN_CTX_ARCHIVE", "1");
        let planted_body = big("archive symlink escape");
        let planted_id = crate::core::archive::store("ctx_shell", "cq04", &planted_body, None)
            .expect("archive handle");
        crate::test_env::remove_var("LEAN_CTX_ARCHIVE");
        let content_file = crate::core::archive::content_path_str(&planted_id);
        std::fs::remove_file(&content_file).expect("remove archived content");
        symlink(&target, &content_file).expect("plant archive symlink");
        let archive_expand = crate::tools::ctx_expand::handle(&serde_json::json!({
            "id": planted_id
        }));
        let archive_verify = verify_handle(&planted_id, None);
        let archive_read = crate::core::archive::retrieve(&planted_id);
        std::fs::remove_file(&content_file).expect("remove planted archive symlink");
        assert!(
            !archive_expand.contains("outside symlink sentinel"),
            "ctx_expand must not follow an archive link out of the store: {archive_expand}"
        );
        assert!(archive_read.is_none());
        assert!(matches!(
            archive_verify.outcome,
            RecoveryOutcome::Refused(_)
        ));
    }
}

/// A `tee` directory that is itself a link to another store must not let a
/// tee-shaped name read that store.
#[cfg(unix)]
#[test]
fn tee_store_that_links_to_another_store_is_refused() {
    use std::os::unix::fs::symlink;

    let _lock = test_env_lock();
    let body = big("tee store link");
    let handle = persist(&body).expect("tee handle");
    let state = crate::core::paths::state_dir().expect("state directory");
    let tee = state.join("tee");
    let real = state.join("tee-real-for-test");
    let _ = std::fs::remove_dir_all(&real);
    std::fs::rename(&tee, &real).expect("move tee store aside");
    symlink(&real, &tee).expect("link tee store");

    let resolved = resolve_tee(&handle);
    let read = read_tee(&handle);

    std::fs::remove_file(&tee).expect("remove tee link");
    std::fs::rename(&real, &tee).expect("restore tee store");
    assert!(resolved.is_none(), "a linked tee store must be refused");
    assert!(read.is_none());
    assert_eq!(read_tee(&handle).as_deref(), Some(body.as_str()));
}
