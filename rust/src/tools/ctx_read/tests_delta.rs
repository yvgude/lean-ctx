//! Delta-explicit re-read tests (extracted from tests.rs for LOC gate).
//!
//! Tests that verify explicit full/lines re-reads of changed cached files
//! are served as diffs (opt-in delta_explicit feature).

use std::time::Duration;

use super::handle_with_task_resolved;
use super::resolve_explicit_delta_mode;
use super::try_stub_hit_readonly_scoped;
use crate::core::cache::SessionCache;
use crate::tools::CrpMode;

// delta_explicit: serve explicit full/lines re-reads of changed cached files as
// diffs (opt-in). The decision is the pure `resolve_explicit_delta_mode`; the
// end-to-end diff base is exercised via the engine. Mirrors the
// `try_stub_hit_readonly` staleness-test conventions above.
// ---------------------------------------------------------------------------

/// Prime the cache with a full read of the file already on disk at `p`.
fn primed_full_cache(p: &str) -> SessionCache {
    let mut cache = SessionCache::new();
    let _ = handle_with_task_resolved(&mut cache, p, "full", CrpMode::Off, None);
    debug_assert!(
        cache.is_full_delivered(p),
        "fixture must deliver full content"
    );
    cache
}

/// Regression: an `auto` re-read of an unchanged, already-fully-delivered file
/// must collapse to the cheap `[unchanged]` stub — not re-deliver the whole body.
/// The auto path used to resolve modes with `cache: None`, so the resolver's
/// `("full","cache_hit")` short-circuit was dead and every `auto` re-read re-sent
/// the file ("re-reads aren't cached"). The cache-aware resolver restores it.
#[test]
fn auto_reread_of_fully_delivered_file_serves_unchanged_stub() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("warm.rs");
    let p = path.to_string_lossy().to_string();
    // Body big enough that a full re-delivery dwarfs the ~13-token stub.
    let body = (0..48)
        .map(|i| format!("fn function_number_{i}() {{ let value_{i} = {i} * 2; }}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, format!("{body}\n")).unwrap();

    // Cost of a full delivery, measured on a cold cache.
    let mut cold = SessionCache::new();
    let full = handle_with_task_resolved(&mut cold, &p, "full", CrpMode::Off, None);
    assert!(
        !full.content.contains("[unchanged"),
        "cold full read must deliver the body, not a stub"
    );

    // Warm cache: full body already delivered, file unchanged on disk.
    let mut cache = primed_full_cache(&p);
    let reread = handle_with_task_resolved(&mut cache, &p, "auto", CrpMode::Off, None);
    assert!(
        reread.content.contains("[unchanged"),
        "auto re-read of an unchanged fully-delivered file must serve the stub, got: {}",
        reread.content
    );
    assert!(
        reread.output_tokens.saturating_mul(4) < full.output_tokens,
        "stub ({} tok) must be far cheaper than a full re-delivery ({} tok)",
        reread.output_tokens,
        full.output_tokens
    );
}

/// Regression #841: a full->task->full sequence must NOT serve the [unchanged]
/// stub on the third read. The task read delivers partial content, so the
/// model's most recent view is NOT the full file -- re-delivering is mandatory.
#[test]
fn mode_change_clears_full_delivered_flag() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("readme.md");
    let p = path.to_string_lossy().to_string();
    let body = (0..50)
        .map(|i| {
            format!(
                "## Section {i}
Content for section {i}."
            )
        })
        .collect::<Vec<_>>()
        .join(
            "

",
        );
    std::fs::write(
        &path,
        format!(
            "{body}
"
        ),
    )
    .unwrap();

    let mut cache = SessionCache::new();

    // 1. Full read -- delivers everything, sets full_content_delivered.
    let full1 = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    assert!(
        !full1.content.contains("[unchanged"),
        "first full read must deliver the body"
    );
    assert!(
        cache.is_full_delivered(&p),
        "full_content_delivered must be set"
    );

    // 2. Task read -- partial/filtered content. The fix clears full_content_delivered.
    let task = handle_with_task_resolved(
        &mut cache,
        &p,
        "task",
        CrpMode::Off,
        Some("find section 42"),
    );
    assert!(
        !task.content.contains("[unchanged"),
        "task read must deliver filtered content, not a stub"
    );
    assert!(
        !cache.is_full_delivered(&p),
        "full_content_delivered must be cleared after a non-full read (#841)"
    );

    // 3. Full read -- must re-deliver actual content, NOT the [unchanged] stub.
    let full2 = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    assert!(
        !full2.content.contains("[unchanged"),
        "full read after a task read must deliver the body, not stub (#841): {}",
        full2.content
    );
    assert!(
        full2.content.contains("Section 0") || full2.content.contains("Section 49"),
        "re-delivered full read must contain actual file content"
    );
}

/// Regression #841 complement: full->full (no mode change) must still serve
/// the cheap [unchanged] stub -- the fix must not break the happy path.
#[test]
fn full_reread_still_serves_stub_when_no_mode_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stable.rs");
    let p = path.to_string_lossy().to_string();
    let body = (0..48)
        .map(|i| format!("fn function_{i}() {{ let v = {i}; }}"))
        .collect::<Vec<_>>()
        .join(
            "
",
        );
    std::fs::write(
        &path,
        format!(
            "{body}
"
        ),
    )
    .unwrap();

    let mut cache = SessionCache::new();

    // 1. Full read.
    let full1 = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    assert!(
        !full1.content.contains("[unchanged"),
        "first read must deliver body"
    );

    // 2. Full re-read -- same mode, file unchanged -> stub.
    let full2 = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    assert!(
        full2.content.contains("[unchanged"),
        "full->full re-read of unchanged file must serve the stub: {}",
        full2.content
    );
}

// ---------------------------------------------------------------------------
// Conversation scoping (#954): the `[unchanged]` stub is only valid for a
// re-read from the *same* conversation that received the full content. The
// current conversation is injected via `try_stub_hit_readonly_scoped` so these
// assertions are deterministic regardless of the host's `active_transcript.json`.
// ---------------------------------------------------------------------------

#[test]
fn conversation_scoped_stub_served_for_same_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("warm.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() { let x = 1; }\n").unwrap();

    let cache = primed_full_cache(&p);
    // Re-reading from the very conversation the fixture delivered under must
    // collapse to the cheap stub.
    let delivered = cache.get(&p).unwrap().delivered_conversation.clone();
    let out = try_stub_hit_readonly_scoped(&cache, &p, delivered.as_deref());
    assert!(
        out.is_some_and(|o| o.content.contains("[unchanged")),
        "same-conversation re-read must serve the stub"
    );
}

#[test]
fn conversation_scoped_stub_withheld_for_other_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("warm.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() { let x = 1; }\n").unwrap();

    let cache = primed_full_cache(&p);
    let foreign = "conversation-that-never-read-this-file";
    // Guard against the fixture (improbably) using this exact id.
    assert_ne!(
        cache.get(&p).unwrap().delivered_conversation.as_deref(),
        Some(foreign),
        "test fixture id collided with the foreign id"
    );
    let out = try_stub_hit_readonly_scoped(&cache, &p, Some(foreign));
    assert!(
        out.is_none(),
        "a foreign conversation must get a full re-read, never a misleading [unchanged] stub"
    );
}

/// #1128: the full-read fallback must never mint a stub of its own. The gate
/// above is the single decision point, so once a read reaches
/// `handle_full_with_auto_delta` it owes the caller content — regardless of what
/// an earlier (possibly foreign) conversation was delivered.
#[test]
fn full_read_fallback_never_serves_a_stub() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("warm.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() { let x = 1; }\n").unwrap();

    let mut cache = primed_full_cache(&p);
    // Same bytes on disk, so `store` reports a hit and the old code took the
    // ungated `full_content_delivered` branch here.
    let (out, _) = super::handle_full_with_auto_delta(&mut cache, &p, "F1", &p, "rs", None);
    assert!(
        !out.contains("[unchanged"),
        "full-read fallback must not decide stubbing on its own: {out}"
    );
    assert!(
        out.contains("fn main"),
        "full-read fallback must deliver content: {out}"
    );
}

#[test]
fn conversation_scoped_stub_served_when_no_context() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("warm.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() { let x = 1; }\n").unwrap();

    let cache = primed_full_cache(&p);
    // current = None (hooks absent) preserves legacy process-scoped behavior.
    let out = try_stub_hit_readonly_scoped(&cache, &p, None);
    assert!(
        out.is_some_and(|o| o.content.contains("[unchanged")),
        "absent conversation context must keep legacy stub behavior"
    );
}

// ---------------------------------------------------------------------------
// Persistent cold stub (#955): after a daemon restart / idle clear the live
// cache is empty, so an unchanged re-read must be served from the persisted
// index — but only for the SAME known conversation and an unchanged file. The
// record is forged directly (modelling one that outlived the restart) and the
// current conversation is injected, so the assertions are host-independent.
// ---------------------------------------------------------------------------

/// Primes a real full delivery to capture authentic (hash, mtime, line_count,
/// file_ref), then forges a persisted record under `conv`. Clears the global
/// index before priming (so the prime isn't short-circuited by a stale record)
/// and after (to drop the prime's own write-through) — leaving exactly the one
/// forged record.
fn seed_cold_record(p: &str, conv: &str) {
    crate::core::read_stub_index::clear_for_test();
    let primed = primed_full_cache(p);
    let entry = primed.get(p).unwrap();
    let rec = crate::core::read_stub_index::StubRecord::new(
        crate::core::pathutil::normalize_tool_path(p),
        entry.hash.clone(),
        entry.stored_mtime,
        entry.line_count,
        primed.get_file_ref_readonly(p).unwrap_or_default(),
        Some(conv.to_string()),
    );
    crate::core::read_stub_index::clear_for_test();
    crate::core::read_stub_index::record(rec);
}

#[test]
#[serial_test::serial(stub_index)]
fn cold_fallback_serves_stub_for_same_conversation_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("warm.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() { let x = 1; }\n").unwrap();

    seed_cold_record(&p, "conv-a");
    // Empty cache models a fresh daemon: the warm path misses, cold fallback fires.
    let cold = SessionCache::new();
    let out = try_stub_hit_readonly_scoped(&cold, &p, Some("conv-a"));
    crate::core::read_stub_index::clear_for_test();
    assert!(
        out.is_some_and(|o| o.content.contains("[unchanged")),
        "same-conversation re-read after restart must serve the persisted stub"
    );
}

#[test]
#[serial_test::serial(stub_index)]
fn cold_fallback_withheld_for_other_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("warm.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() { let x = 1; }\n").unwrap();

    seed_cold_record(&p, "conv-a");
    let cold = SessionCache::new();
    let out = try_stub_hit_readonly_scoped(&cold, &p, Some("conv-b"));
    crate::core::read_stub_index::clear_for_test();
    assert!(
        out.is_none(),
        "a different conversation must get a cold full read, never a persisted stub"
    );
}

#[test]
#[serial_test::serial(stub_index)]
fn cold_fallback_withheld_without_conversation_context() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("warm.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() { let x = 1; }\n").unwrap();

    seed_cold_record(&p, "conv-a");
    let cold = SessionCache::new();
    // Unlike the WARM path, an absent conversation cannot prove the content is in
    // the new process's context → no cold stub (the stricter gate keeps #954's
    // cross-chat hazard closed across restarts).
    let out = try_stub_hit_readonly_scoped(&cold, &p, None);
    crate::core::read_stub_index::clear_for_test();
    assert!(
        out.is_none(),
        "absent conversation context must NOT serve a cold persisted stub"
    );
}

#[test]
#[serial_test::serial(stub_index)]
fn cold_fallback_withheld_when_file_changed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("warm.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() { let x = 1; }\n").unwrap();

    seed_cold_record(&p, "conv-a");
    // Content changed during downtime → mtime/md5 mismatch → no stub.
    std::fs::write(&path, "fn main() { let x = 2; let y = 3; }\n").unwrap();
    let cold = SessionCache::new();
    let out = try_stub_hit_readonly_scoped(&cold, &p, Some("conv-a"));
    crate::core::read_stub_index::clear_for_test();
    assert!(
        out.is_none(),
        "a file changed on disk must get a cold full read, never a stale stub"
    );
}

#[test]
fn delta_explicit_changed_file_diverts_full_reread_to_diff() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("changed.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() {}\n").unwrap();

    let mut cache = primed_full_cache(&p);

    // File changes on disk after the first full read.
    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "fn main() { changed(); }\n").unwrap();

    let decision = resolve_explicit_delta_mode(
        &cache, &p, "full", /*explicit*/ true, /*fresh*/ false, true,
    );
    assert_eq!(
        decision.mode, "diff",
        "changed full re-read must divert to diff"
    );
    let note = decision
        .note
        .expect("a diff diversion must carry an advisory note");
    assert!(
        note.contains("[delta-explicit]"),
        "note tag missing: {note}"
    );
    assert!(
        note.contains("fresh=true"),
        "note must mention the bypass: {note}"
    );

    // End-to-end: the engine renders the diff against the FULL cached content.
    let out = handle_with_task_resolved(&mut cache, &p, "diff", CrpMode::Off, None);
    assert_eq!(out.resolved_mode, "diff");
    assert!(
        out.content.contains("[diff]"),
        "engine must emit a diff: {}",
        out.content
    );
    assert!(
        out.content.contains("changed()"),
        "diff must reflect the new on-disk content: {}",
        out.content
    );
}

#[test]
fn delta_explicit_changed_lines_request_diverts_to_diff() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lines.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn a() {}\nfn b() {}\n").unwrap();

    let cache = primed_full_cache(&p);

    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "fn a() { x(); }\nfn b() {}\n").unwrap();

    let decision = resolve_explicit_delta_mode(&cache, &p, "lines:1-1", true, false, true);
    assert_eq!(
        decision.mode, "diff",
        "a changed-file lines: re-read must divert to diff, not re-extract a window"
    );
    assert!(decision.note.is_some());
}

#[test]
fn delta_explicit_diff_base_is_full_cached_content_not_compressed() {
    // Fix #2 guard: the diff base must be the full source the cache stored, even
    // when the most recent read of the file was a COMPRESSED view (map). If the
    // base were the compressed view, the diff would be garbage.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.rs");
    let p = path.to_string_lossy().to_string();
    let mut content = String::new();
    for i in 0..60 {
        content.push_str(&format!(
            "pub fn original_fn_{i}(x: i32) -> i32 {{ x + {i} }}\n"
        ));
    }
    std::fs::write(&path, &content).unwrap();

    let mut cache = SessionCache::new();
    // Cache the full content, then read a compressed (map) view — last_mode=map,
    // but the entry still stores the full source.
    let _ = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    let _ = handle_with_task_resolved(&mut cache, &p, "map", CrpMode::Off, None);

    // Change exactly one line on disk.
    std::thread::sleep(Duration::from_secs(1));
    let changed = content.replace(
        "pub fn original_fn_7(x: i32) -> i32 { x + 7 }",
        "pub fn original_fn_7(x: i32) -> i32 { x + 70707 }",
    );
    std::fs::write(&path, &changed).unwrap();

    let out = handle_with_task_resolved(&mut cache, &p, "diff", CrpMode::Off, None);
    assert!(
        out.content.contains("[diff]"),
        "expected a diff: {}",
        out.content
    );
    // The marker appears only if the diff compared against the FULL original
    // source (a compressed map base would never contain this literal).
    assert!(
        out.content.contains("70707"),
        "diff must be computed against full cached source, got: {}",
        out.content
    );
    // And it must be a one-line edit, not a wholesale replacement of a
    // compressed base against the full file.
    assert!(
        out.content.contains("+1/-1") || out.content.contains("-1/+1"),
        "single-line change should diff as +1/-1: {}",
        out.content
    );
}

#[test]
fn delta_explicit_unchanged_lines_collapse_to_full_stub() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("same.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn a() {}\nfn b() {}\n").unwrap();

    let cache = primed_full_cache(&p);

    // No disk change. A lines: re-read of a fully-delivered file re-emits text
    // the model holds → collapse to the full-mode stub (no diff, no note).
    let decision = resolve_explicit_delta_mode(&cache, &p, "lines:1-1", true, false, true);
    assert_eq!(
        decision.mode, "full",
        "unchanged lines: of a full file must collapse to the stub"
    );
    assert!(
        decision.note.is_none(),
        "a silent stub collapse must not carry a note"
    );
}

#[test]
fn delta_explicit_unchanged_full_reread_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("same.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn a() {}\n").unwrap();

    let cache = primed_full_cache(&p);

    // An unchanged full re-read already hits the downstream `[unchanged]` stub;
    // the resolver leaves it untouched.
    let decision = resolve_explicit_delta_mode(&cache, &p, "full", true, false, true);
    assert_eq!(decision.mode, "full");
    assert!(decision.note.is_none());
}

#[test]
fn delta_explicit_off_preserves_current_behavior() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("changed.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() {}\n").unwrap();

    let cache = primed_full_cache(&p);

    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "fn main() { changed(); }\n").unwrap();

    // enabled=false → the mode is never rewritten, no matter the disk state.
    let decision =
        resolve_explicit_delta_mode(&cache, &p, "full", true, false, /*enabled*/ false);
    assert_eq!(
        decision.mode, "full",
        "feature OFF must preserve the requested mode"
    );
    assert!(decision.note.is_none());

    let lines = resolve_explicit_delta_mode(&cache, &p, "lines:1-1", true, false, false);
    assert_eq!(
        lines.mode, "lines:1-1",
        "feature OFF must not touch lines: either"
    );
    assert!(lines.note.is_none());
}

#[test]
fn delta_explicit_fresh_bypasses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("changed.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() {}\n").unwrap();

    let cache = primed_full_cache(&p);

    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "fn main() { changed(); }\n").unwrap();

    // fresh=true → always bypass even with the feature on and a changed file.
    let decision = resolve_explicit_delta_mode(&cache, &p, "full", true, /*fresh*/ true, true);
    assert_eq!(
        decision.mode, "full",
        "fresh=true must bypass the diff diversion"
    );
    assert!(decision.note.is_none());
}

#[test]
fn delta_explicit_first_read_unaffected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() {}\n").unwrap();

    // Nothing cached yet — the very first read can never be a diff.
    let cache = SessionCache::new();
    let decision = resolve_explicit_delta_mode(&cache, &p, "full", true, false, true);
    assert_eq!(
        decision.mode, "full",
        "an uncached first read must be served normally"
    );
    assert!(decision.note.is_none());

    let lines = resolve_explicit_delta_mode(&cache, &p, "lines:1-1", true, false, true);
    assert_eq!(lines.mode, "lines:1-1");
    assert!(lines.note.is_none());
}

#[test]
fn delta_explicit_only_fires_for_explicit_mode() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("changed.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() {}\n").unwrap();

    let cache = primed_full_cache(&p);

    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "fn main() { changed(); }\n").unwrap();

    // explicit_mode=false (mode was auto-resolved) → never diverted; auto-mode
    // already has its own staleness handling.
    let decision =
        resolve_explicit_delta_mode(&cache, &p, "full", /*explicit*/ false, false, true);
    assert_eq!(
        decision.mode, "full",
        "auto-resolved modes must not be diverted to diff"
    );
    assert!(decision.note.is_none());
}

#[test]
fn delta_explicit_decision_is_byte_stable() {
    // #498 determinism: the resolver's note carries no timestamp/counter, so
    // repeated calls on the same changed-file state are byte-identical.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("changed.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() {}\n").unwrap();

    let cache = primed_full_cache(&p);
    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "fn main() { changed(); }\n").unwrap();

    let d1 = resolve_explicit_delta_mode(&cache, &p, "full", true, false, true);
    let d2 = resolve_explicit_delta_mode(&cache, &p, "full", true, false, true);
    assert_eq!(
        d1, d2,
        "delta-explicit decision drifted between identical calls"
    );
}
