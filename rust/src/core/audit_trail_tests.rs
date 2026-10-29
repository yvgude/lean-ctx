// SPDX-License-Identifier: Apache-2.0

use super::*;

/// Append `count` valid, chained, unsigned tool-call entries to `path`
/// without per-entry fsync, continuing from the file's current head.
fn append_generated(path: &Path, count: usize) {
    use std::io::BufWriter;
    let mut prev = fs::File::open(path).ok().map_or_else(
        || "genesis".to_string(),
        |f| read_last_hash_tail(&f).unwrap(),
    );
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open trail");
    let mut out = BufWriter::with_capacity(1 << 20, file);
    for i in 0..count {
        let data = AuditEntryData {
            agent_id: format!("bench-agent-{}", i % 7),
            tool: "ctx_read".to_string(),
            action: Some(format!("secret_detected n=1 path=src/module_{i}.rs")),
            input_hash: hash_input(&serde_json::Map::new()),
            output_tokens: 1200,
            role: "coder".to_string(),
            event_type: AuditEventType::ToolCall,
        };
        let partial = serde_json::json!({
            "agent_id": data.agent_id, "tool": data.tool, "action": data.action,
            "input_hash": data.input_hash, "output_tokens": data.output_tokens,
            "role": data.role, "event_type": data.event_type,
        });
        let entry_hash = compute_entry_hash(&prev, &serde_json::to_string(&partial).unwrap());
        let entry = AuditEntry {
            timestamp: "2026-10-19T12:00:00.000000+00:00".to_string(),
            agent_id: data.agent_id,
            tool: data.tool,
            action: data.action,
            input_hash: data.input_hash,
            output_tokens: data.output_tokens,
            role: data.role,
            event_type: data.event_type,
            prev_hash: prev,
            entry_hash: entry_hash.clone(),
            signature: None,
        };
        serde_json::to_writer(&mut out, &entry).unwrap();
        out.write_all(b"\n").unwrap();
        prev = entry_hash;
    }
    out.flush().unwrap();
}

/// Wall-time benchmark on a generated trail; kept out of the default run.
/// `LEAN_CTX_AUDIT_BENCH_MB=1024 cargo test --profile dev-fast --lib
/// audit_trail::tests::bench_large_trail_verification -- --ignored --nocapture`
#[test]
#[ignore = "benchmark: generates a large trail"]
fn bench_large_trail_verification() {
    use std::time::Instant;
    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = trail_path().expect("trail path");
    let mb: u64 = std::env::var("LEAN_CTX_AUDIT_BENCH_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256);
    let started = Instant::now();
    // Real trails are written by `try_record`, which creates the local
    // audit key that signs checkpoints.
    try_record(drift_data()).expect("signed first entry");
    while fs::metadata(&path).map_or(0, |m| m.len()) < mb * 1024 * 1024 {
        append_generated(&path, 100_000);
    }
    let size = fs::metadata(&path).unwrap().len();
    eprintln!(
        "generated {} MiB in {:?}",
        size / (1024 * 1024),
        started.elapsed()
    );
    let time = |label: &str, f: &dyn Fn() -> bool| {
        let t = Instant::now();
        let ok = f();
        eprintln!("{label}: {:?} (ok={ok})", t.elapsed());
    };
    time("verify_chain_at #1", &|| verify_chain_at(&path).valid);
    time("verify_chain_at #2", &|| verify_chain_at(&path).valid);
    append_generated(&path, 1_000);
    time("verify_chain_at after 1k appended", &|| {
        verify_chain_at(&path).valid
    });
    time("verified_security_evidence (billing status)", &|| {
        crate::core::value::proof::verified_security_evidence().is_some()
    });
    time("latest_unacknowledged_drift", &|| {
        latest_unacknowledged_drift("bench-agent-1").is_ok()
    });
    time("verify_chain_full (explicit audit)", &|| {
        verify_chain_full().valid
    });
}

fn drift_data() -> AuditEntryData {
    AuditEntryData {
        agent_id: "drift-agent".to_string(),
        tool: "agent_registry".to_string(),
        action: Some("attestation drift dimensions=binary+config".to_string()),
        input_hash: hash_input(&serde_json::Map::new()),
        output_tokens: 0,
        role: "coder".to_string(),
        event_type: AuditEventType::AgentDriftDetected,
    }
}

/// The drift event must be a first-class, on-disk-stable event type:
/// the OCP Part 4 contract enumerates the snake_case label, so a rename
/// would silently invalidate every consumer's schema check.
#[test]
fn drift_event_type_has_a_stable_wire_label() {
    let json = serde_json::to_string(&AuditEventType::AgentDriftDetected).expect("serialize");
    assert_eq!(json, "\"agent_drift_detected\"");
    let parsed: AuditEventType =
        serde_json::from_str("\"agent_drift_detected\"").expect("deserialize");
    assert!(matches!(parsed, AuditEventType::AgentDriftDetected));
}

#[test]
fn try_record_appends_a_chained_drift_entry() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let first = try_record(drift_data()).expect("first append");
    assert_eq!(first.prev_hash, "genesis");
    let second = try_record(drift_data()).expect("second append");
    assert_eq!(second.prev_hash, first.entry_hash);

    let entries = load_recent(10);
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|e| matches!(e.event_type, AuditEventType::AgentDriftDetected))
    );
    let chain = verify_chain();
    assert!(
        chain.valid,
        "chain must verify: {:?}",
        chain.first_invalid_at
    );
    assert_eq!(chain.total_entries, 2);
}

#[test]
fn drift_entries_require_a_valid_local_signature() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let entry = try_record(drift_data()).expect("append signed drift");
    assert!(
        entry.signature.is_some(),
        "drift writes must fail without a signature"
    );

    let path = iso.path().join("audit").join("trail.jsonl");
    let mut value: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&path).expect("trail").trim_end())
            .expect("entry JSON");
    value["signature"] = serde_json::Value::String("00".repeat(64));
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string(&value).expect("JSON")),
    )
    .expect("forge signature");

    let chain = verify_chain();
    assert!(
        !chain.valid,
        "a hash-linked forged drift entry must fail authentication"
    );
    assert_eq!(chain.first_invalid_at, Some(0));
}

/// A blocked sink must surface as an error rather than being swallowed —
/// that is the whole reason `try_record` exists. `record` keeps the
/// historical fire-and-forget contract on the same failure.
#[test]
fn try_record_surfaces_sink_failure_that_record_swallows() {
    let iso = crate::core::data_dir::isolated_data_dir();
    // A regular file where the audit directory belongs makes
    // `create_dir_all` fail, so no trail path can be resolved.
    std::fs::write(iso.path().join("audit"), b"not a directory").expect("block audit dir");

    let error = try_record(drift_data()).expect_err("blocked sink must fail");
    assert!(error.contains("audit trail unavailable"), "{error}");
    record(drift_data());
    assert!(load_recent(10).is_empty(), "nothing may be persisted");
}

#[test]
fn unknown_additive_event_verifies_as_forward_compatible_not_tampered() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = trail_path().expect("trail path");
    let event_type = "x-agent_future_observed";
    let partial = serde_json::json!({
        "agent_id": "forward-agent",
        "tool": "agent_registry",
        "action": null,
        "input_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "output_tokens": 0,
        "role": "coder",
        "event_type": event_type,
    });
    let data_json = serde_json::to_string(&partial).expect("partial serializes");
    let entry_hash = compute_entry_hash("genesis", &data_json);
    let mut entry = serde_json::json!({
        "timestamp": "2026-09-06T20:00:00Z",
        "agent_id": "forward-agent",
        "tool": "agent_registry",
        "action": null,
        "input_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "output_tokens": 0,
        "role": "coder",
        "event_type": event_type,
        "prev_hash": "genesis",
        "entry_hash": entry_hash,
    });
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string(&entry).expect("entry serializes")
        ),
    )
    .expect("write forward entry");

    let forward = verify_chain();
    assert!(forward.valid, "unknown additive event is not tampering");
    assert_eq!(forward.total_entries, 1);
    assert_eq!(forward.unknown_event_types, 1);
    assert_eq!(forward.first_invalid_at, None);
    let recent = load_recent(1);
    assert!(matches!(
        recent.as_slice(),
        [AuditEntry {
            event_type: AuditEventType::Unknown(label),
            ..
        }] if label == event_type
    ));

    try_record(drift_data()).expect("append after unknown event");
    let continued = verify_chain();
    assert!(continued.valid);
    assert_eq!(continued.total_entries, 2);
    assert_eq!(continued.unknown_event_types, 1);

    entry["entry_hash"] = serde_json::json!("0".repeat(64));
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::to_string(&entry).expect("tampered serializes")
        ),
    )
    .expect("write tampered entry");
    let tampered = verify_chain();
    assert!(
        !tampered.valid,
        "actual hash corruption must remain invalid"
    );
    assert_eq!(tampered.first_invalid_at, Some(0));
}

#[test]
fn unnamespaced_unknown_event_is_rejected_even_when_hash_linked() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = trail_path().expect("trail path");
    let event_type = "agent_future_observed";
    let partial = serde_json::json!({
        "agent_id": "forward-agent",
        "tool": "agent_registry",
        "action": null,
        "input_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "output_tokens": 0,
        "role": "coder",
        "event_type": event_type,
    });
    let data_json = serde_json::to_string(&partial).expect("partial serializes");
    let entry_hash = compute_entry_hash("genesis", &data_json);
    let entry = serde_json::json!({
        "timestamp": "2026-09-06T20:00:00Z",
        "agent_id": "forward-agent",
        "tool": "agent_registry",
        "action": null,
        "input_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "output_tokens": 0,
        "role": "coder",
        "event_type": event_type,
        "prev_hash": "genesis",
        "entry_hash": entry_hash,
    });
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string(&entry).unwrap()),
    )
    .expect("write future entry");

    let result = verify_chain();
    assert!(!result.valid);
    assert_eq!(result.first_invalid_at, Some(0));
    assert!(load_recent(1).is_empty());
}

#[test]
fn writers_reject_unsafe_unknown_event_labels() {
    let unsafe_event = AuditEventType::Unknown("future\nlabel".to_string());
    assert!(serde_json::to_string(&unsafe_event).is_err());
    assert!(
        serde_json::to_string(&AuditEventType::Unknown(
            "agent_future_observed".to_string()
        ))
        .is_err()
    );
    assert!(
        serde_json::to_string(&AuditEventType::Unknown(
            "x-agent_future_observed".to_string()
        ))
        .is_ok()
    );
    assert!(
        serde_json::to_string(&AuditEventType::Unknown(format!("x-a{}", "b".repeat(64)))).is_err(),
        "extension labels must stay report-bounded"
    );
}

#[test]
fn verifier_accepts_legacy_multiple_objects_on_one_line() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    try_record(drift_data()).expect("first append");
    try_record(drift_data()).expect("second append");
    let path = trail_path().expect("trail path");
    let joined = std::fs::read_to_string(&path)
        .expect("audit trail")
        .lines()
        .collect::<String>();
    std::fs::write(&path, format!("{joined}\n")).expect("legacy joined line");

    let chain = verify_chain();
    assert!(chain.valid, "legacy stream must remain verifiable");
    assert_eq!(chain.total_entries, 2);
    assert_eq!(load_recent(2).len(), 2);
}

#[test]
fn audited_state_fails_closed_when_trail_disappears() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    try_record(drift_data()).expect("append drift");
    let path = trail_path().expect("trail path");
    std::fs::remove_file(path).expect("remove audit trail");

    let chain = verify_chain();
    assert!(chain.valid, "missing file is an empty history generically");
    assert!(!chain.source_present);
    let error = latest_unacknowledged_drift("drift-agent").expect_err("evidence loss");
    assert!(error.contains("unavailable"), "{error}");
}

#[cfg(unix)]
#[test]
fn audit_lock_contention_is_bounded() {
    use fs2::FileExt;
    use std::time::{Duration, Instant};

    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = trail_path().expect("trail path");
    let held = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .expect("trail lock file");
    held.lock_exclusive().expect("hold trail lock");

    let started = Instant::now();
    let error = try_record(drift_data()).expect_err("contention must fail closed");
    assert!(error.contains("timed out"), "unexpected error: {error}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "bounded audit lock exceeded two seconds"
    );
}

/// A verifier that meets an in-progress append waits for it under a
/// shared lock; if the append outlasts the bound, contention reads as an
/// unavailable trail, never as corruption ("invalid at entry 0" on
/// Windows, where the writer's lock is mandatory).
#[test]
fn verifier_reports_contention_as_unavailable_not_invalid() {
    use fs2::FileExt;

    let _iso = crate::core::data_dir::isolated_data_dir();
    try_record(drift_data()).expect("append drift");
    let path = trail_path().expect("trail path");
    let held = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("trail lock file");
    held.lock_exclusive()
        .expect("hold trail lock like an append");

    let contended = verify_chain();
    assert!(
        !contended.source_present,
        "contention is not a present-but-invalid trail"
    );
    let error = latest_unacknowledged_drift("drift-agent").expect_err("fail closed");
    assert!(error.contains("unavailable"), "{error}");

    FileExt::unlock(&held).expect("release");
    let released = verify_chain();
    assert!(released.valid && released.source_present);
    assert_eq!(released.total_entries, 1);
}

/// Flip one digit of a hashed field near the middle of the trail in
/// place (same inode, same length, far from both checkpoint edges) and
/// return the index of the entry it breaks.
fn tamper_middle(path: &Path) -> usize {
    use std::io::{Seek, SeekFrom};
    // Coarse filesystem clocks: let the mtime move past the last append.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let text = fs::read_to_string(path).expect("trail");
    let mid = text.len() / 2;
    let at = mid + text[mid..].find("module_").expect("generated entry") + "module_".len();
    let flipped = if text.as_bytes()[at] == b'9' {
        b"8"
    } else {
        b"9"
    };
    let mut file = OpenOptions::new().write(true).open(path).expect("open");
    file.seek(SeekFrom::Start(at as u64)).expect("seek");
    file.write_all(flipped).expect("tamper");
    text[..at].matches('\n').count()
}

/// A trail signed by the local audit key (so checkpoints can be stored)
/// with `extra` generated entries after the first, signed one.
fn checkpointable_trail(extra: usize) -> PathBuf {
    try_record(drift_data()).expect("signed first entry");
    let path = trail_path().expect("trail path");
    append_generated(&path, extra);
    path
}

/// The resumed pass reads only the appended suffix — which is why it
/// cannot see an edit deep inside the verified prefix. That gap is
/// bounded: a checkpoint older than a day is ignored, and the explicit
/// full pass reads from genesis.
#[test]
fn resumed_pass_reads_only_the_suffix_and_the_full_pass_bounds_it() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = checkpointable_trail(1_000);
    assert_eq!(verify_chain().total_entries, 1_001);

    let broken = tamper_middle(&path);
    append_generated(&path, 10);
    let resumed = verify_chain();
    assert!(resumed.valid, "a resumed pass verifies only the suffix");
    assert_eq!(resumed.total_entries, 1_011);

    let a_day_later = chrono::Utc::now().timestamp() + checkpoint::FULL_REVERIFY_SECS + 1;
    let (stale, _) = verify_chain_mode_at(&path, None, false, a_day_later);
    assert_eq!(stale.first_invalid_at, Some(broken));
    assert_eq!(verify_chain_full().first_invalid_at, Some(broken));
    assert_eq!(verify_chain().first_invalid_at, Some(broken));
}

/// Anything but a pure append — an in-place rewrite, a truncation, a
/// replaced file — must not be answered from the checkpoint.
#[test]
fn rewrite_truncation_and_rotation_force_a_full_pass() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = checkpointable_trail(1_000);
    let original = fs::read(&path).expect("trail");
    assert_eq!(verify_chain().total_entries, 1_001);

    // Same length, nothing appended.
    let broken = tamper_middle(&path);
    assert_eq!(verify_chain().first_invalid_at, Some(broken));

    fs::write(&path, &original).expect("restore");
    assert_eq!(verify_chain().total_entries, 1_001);
    // Truncated to drop the last entry.
    let cut = original[..original.len() - 1]
        .iter()
        .rposition(|b| *b == b'\n')
        .expect("line boundary")
        + 1;
    OpenOptions::new()
        .write(true)
        .open(&path)
        .and_then(|f| f.set_len(cut as u64))
        .expect("truncate");
    let truncated = verify_chain();
    assert!(truncated.valid);
    assert_eq!(truncated.total_entries, 1_000);

    // Rotated: a new file with the same bytes at the verified edges.
    #[cfg(unix)]
    {
        fs::write(&path, &original).expect("restore");
        assert_eq!(verify_chain().total_entries, 1_001);
        let rotated = path.with_extension("rotated");
        fs::rename(&path, &rotated).expect("rotate");
        fs::copy(&rotated, &path).expect("replacement");
        let broken = tamper_middle(&path);
        append_generated(&path, 1);
        assert_eq!(verify_chain().first_invalid_at, Some(broken));
    }
}

/// The per-agent drift state rides along in the checkpoint: a resumed
/// pass must report the same open drift a full pass would, and clear it
/// on the matching acknowledgement.
#[test]
fn drift_state_survives_a_resumed_pass() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = checkpointable_trail(0);
    let open = latest_unacknowledged_drift("drift-agent")
        .expect("trail")
        .expect("open drift");

    append_generated(&path, 1);
    assert_eq!(
        latest_unacknowledged_drift("drift-agent").expect("trail"),
        Some(open.clone())
    );
    try_record(AuditEntryData {
        action: Some(open.1),
        event_type: AuditEventType::AgentDriftAcknowledged,
        ..drift_data()
    })
    .expect("acknowledge");
    assert_eq!(
        latest_unacknowledged_drift("drift-agent").expect("trail"),
        None
    );
}

/// The recent listing reads back from the end only, and returns exactly
/// what the full listing ends with — across block boundaries, with a cut
/// first line, and for a limit beyond the whole trail.
#[test]
fn recent_entries_come_from_the_tail_and_match_the_full_listing() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = trail_path().expect("trail path");
    assert!(load_recent_at(&path, 10).is_empty());
    append_generated(&path, 3_000);
    assert!(fs::metadata(&path).unwrap().len() > 3 * TAIL_BLOCK);
    let all = load_all_at(&path);
    assert_eq!(all.len(), 3_000);
    let hashes = |entries: &[AuditEntry]| -> Vec<String> {
        entries.iter().map(|e| e.entry_hash.clone()).collect()
    };
    for limit in [1, 7, 600, 2_999, 3_000, 5_000] {
        let recent = load_recent_at(&path, limit);
        let expected = &all[all.len().saturating_sub(limit)..];
        assert_eq!(hashes(&recent), hashes(expected), "limit {limit}");
    }
    assert!(load_recent_at(&path, 0).is_empty());
}

/// A reader holds the trail lock only while it takes the length: after a
/// verification or a listing, an append gets the lock at once, and what
/// the reader saw is the prefix up to that length.
#[cfg(unix)]
#[test]
fn readers_do_not_hold_the_trail_lock_through_their_pass() {
    use fs2::FileExt;

    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = checkpointable_trail(500);
    let reader = fs::File::open(&path).expect("trail");
    lock_trail_shared(&reader).expect("reader lock");
    let len = reader.metadata().unwrap().len();
    release_read_lock(&reader);
    let writer = OpenOptions::new().append(true).open(&path).expect("trail");
    writer
        .try_lock_exclusive()
        .expect("append is not blocked by the reader");
    FileExt::unlock(&writer).expect("unlock");

    try_record(drift_data()).expect("append after the snapshot");
    let mut prefix = Vec::new();
    (&reader)
        .take(len)
        .read_to_end(&mut prefix)
        .expect("prefix");
    let mut state = ChainState::genesis();
    let (read, clean) = state
        .verify_stream(&mut prefix.as_slice())
        .expect("prefix verifies");
    assert!(clean);
    assert_eq!(read, len);
    assert_eq!(state.total, 501);
    assert!(verify_chain_at(&path).valid);
    assert_eq!(verify_chain_at(&path).total_entries, 502);
}

/// The unlocked read rests on the trail being append-only in place: a
/// replacement goes through rename, which leaves an open reader its own
/// file, while a cut in place is caught as a changed trail.
#[cfg(unix)]
#[test]
fn a_reader_keeps_its_snapshot_across_a_rename_and_notices_a_cut() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = checkpointable_trail(200);
    let reader = fs::File::open(&path).expect("trail");
    lock_trail_shared(&reader).expect("reader lock");
    let len = reader.metadata().unwrap().len();
    release_read_lock(&reader);

    // Rotation by rename: a new, unrelated file takes the path.
    fs::rename(&path, path.with_extension("old")).expect("rotate");
    fs::write(&path, b"replacement\n").expect("replacement");
    let mut state = ChainState::genesis();
    let mut prefix = std::io::BufReader::new((&reader).take(len));
    let (read, clean) = state.verify_stream(&mut prefix).expect("snapshot verifies");
    assert!(clean && read == len && state.total == 201);
    assert!(still_holds_prefix(&reader, len));

    // A cut in place shrinks the reader's own file below what it took.
    OpenOptions::new()
        .write(true)
        .open(path.with_extension("old"))
        .and_then(|f| f.set_len(len / 2))
        .expect("cut");
    assert!(!still_holds_prefix(&reader, len));
}

fn hashes(entries: &[AuditEntry]) -> Vec<String> {
    entries.iter().map(|e| e.entry_hash.clone()).collect()
}

/// One plain entry, appended as if at `days` days from now.
fn append_on_day(path: &Path, days: i64) -> AuditEntry {
    let now = chrono::Utc::now() + chrono::Duration::days(days);
    try_record_at_time(path, drift_data_unsigned(), now).expect("append")
}

fn drift_data_unsigned() -> AuditEntryData {
    AuditEntryData {
        event_type: AuditEventType::ToolCall,
        ..drift_data()
    }
}

/// A new day seals the active trail into a segment once a pass verified
/// it, never before; the chain then runs unbroken through the segments,
/// and every reader sees one trail.
#[test]
fn a_new_day_seals_the_verified_trail_and_the_chain_runs_through_its_segments() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = checkpointable_trail(100);

    // Unverified: a day change alone does not seal.
    append_on_day(&path, 1);
    assert!(segments::sealed(&path).is_empty());
    assert_eq!(verify_chain().total_entries, 102);

    // Verified: the next day's first append seals the trail as segment 0.
    let first_of_day = append_on_day(&path, 1);
    let sealed = segments::sealed(&path);
    assert_eq!(sealed.len(), 1);
    assert_eq!(sealed[0].0, 0);
    let segment = load_all_at(&sealed[0].1);
    let seal = segment.last().expect("seal");
    assert!(is_seal(&seal.event_type) && seal.signature.is_some());
    assert_eq!(seal.action.as_deref(), Some("segment=0"));
    assert_eq!(first_of_day.prev_hash, seal.entry_hash);
    assert_eq!(load_all_at(&sealed[0].1).len(), 103);

    let chain = verify_chain();
    assert!(chain.valid);
    assert_eq!(chain.total_entries, 104);
    assert_eq!(
        chain.unknown_event_types, 0,
        "a seal is not an unknown event"
    );
    assert!(verify_chain_full().valid);

    let all = load_all_at(&path);
    assert_eq!(all.len(), 104);
    assert_eq!(
        all.last().map(|e| e.entry_hash.clone()),
        Some(first_of_day.entry_hash)
    );
    for limit in [1, 2, 3, 104, 500] {
        let recent = load_recent_at(&path, limit);
        assert_eq!(
            hashes(&recent),
            hashes(&all[all.len().saturating_sub(limit)..])
        );
    }
    assert_eq!(trail_files_since(&path, None).len(), 2);
    let later = chrono::Utc::now() + chrono::Duration::hours(1);
    assert_eq!(trail_files_since(&path, Some(later)), vec![path.clone()]);
    let text = read_trail_since(&path, None).expect("read").expect("trail");
    assert_eq!(text.lines().count(), 104);

    // The next sealed day becomes segment 1; the chain still verifies,
    // also a week on, when every checkpoint of the active file expired.
    append_on_day(&path, 2);
    assert_eq!(segments::sealed(&path).len(), 2);
    let week = chrono::Utc::now().timestamp() + 7 * 24 * 60 * 60;
    let (late, _) = verify_chain_mode_at(&path, None, false, week);
    assert!(late.valid);
    assert_eq!(late.total_entries, 106);
}

/// Removing or editing a sealed segment breaks the chain.
#[test]
fn a_missing_or_edited_segment_breaks_the_chain() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = checkpointable_trail(50);
    assert!(verify_chain().valid);
    append_on_day(&path, 1);
    assert!(verify_chain().valid);
    append_on_day(&path, 2);
    let sealed = segments::sealed(&path);
    assert_eq!(sealed.len(), 2);
    assert!(verify_chain().valid);

    let broken = tamper_middle(&sealed[0].1);
    assert_eq!(verify_chain().first_invalid_at, Some(broken));
    assert_eq!(verify_chain_full().first_invalid_at, Some(broken));

    let aside = sealed[0].1.with_extension("aside");
    fs::rename(&sealed[0].1, &aside).expect("remove segment 0");
    assert!(!verify_chain().valid);
}

/// A writer that waited on the file another writer sealed meanwhile
/// must not append to the segment.
#[cfg(unix)]
#[test]
fn a_writer_never_appends_to_a_sealed_segment() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let path = checkpointable_trail(10);
    assert!(verify_chain().valid);
    let held = OpenOptions::new()
        .append(true)
        .read(true)
        .open(&path)
        .expect("held");
    assert!(is_current(&path, &held));
    append_on_day(&path, 1);
    assert!(!is_current(&path, &held), "the held file is now segment 0");
    let segment = segments::path(&path, 0);
    let sealed_len = fs::metadata(&segment).unwrap().len();
    try_record(drift_data_unsigned()).expect("append");
    assert_eq!(fs::metadata(&segment).unwrap().len(), sealed_len);
    assert!(verify_chain().valid);
}

/// Size and day rule of the seal.
#[test]
fn a_trail_is_due_at_its_size_cap_or_on_a_new_day() {
    let now = chrono::Utc::now();
    let today = now.to_rfc3339();
    let yesterday = (now - chrono::Duration::days(1)).to_rfc3339();
    assert!(!segments::due(0, Some(&yesterday), now));
    assert!(!segments::due(10, Some(&today), now));
    assert!(segments::due(10, Some(&yesterday), now));
    assert!(segments::due(
        segments::SEGMENT_MAX_BYTES,
        Some(&today),
        now
    ));
    assert_eq!(
        segments::sealed_seq(Some(&segments::seal_action(7))),
        Some(7)
    );
    assert_eq!(segments::sealed_seq(Some("segment=x")), None);
}
