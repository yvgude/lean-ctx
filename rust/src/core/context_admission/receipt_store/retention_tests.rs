// SPDX-License-Identifier: Apache-2.0

use std::time::{Duration, SystemTime};

use super::*;

const DAY: Duration = Duration::from_hours(24);

fn hex(n: u32) -> String {
    format!("{n:064x}")
}

/// A synthetic receipt file of `size` bytes, `days_old` old, in `layout`
/// (`"json"` plain as older versions wrote it, `"json.zst"` packed).
fn receipt(dir: &Path, n: u32, layout: &str, size: usize, days_old: u64) -> PathBuf {
    let receipts = dir.join("receipts");
    std::fs::create_dir_all(&receipts).unwrap();
    let path = receipts.join(format!("{}.{layout}", hex(n)));
    // Random bytes do not compress: a packed and a plain file weigh the same.
    let mut body = vec![0u8; size];
    getrandom::fill(&mut body).unwrap();
    let body = if layout == "json" {
        body
    } else {
        pack(&body).unwrap()
    };
    std::fs::write(&path, body).unwrap();
    age_file(&path, days_old);
    path
}

fn age_file(path: &Path, days_old: u64) {
    let at = SystemTime::now() - DAY * u32::try_from(days_old).unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(at))
        .unwrap();
}

fn index(dir: &Path, sub: &str, name: &str, receipts: &[u32], days_old: u64) -> PathBuf {
    let path = dir.join(sub).join(format!("{name}.json"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let listed: Vec<String> = receipts.iter().map(|n| hex(*n)).collect();
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({"schema_version": 1, "receipts": listed})).unwrap(),
    )
    .unwrap();
    age_file(&path, days_old);
    path
}

fn exists(dir: &Path, n: u32) -> bool {
    let receipts = dir.join("receipts");
    receipts.join(format!("{}.json.zst", hex(n))).exists()
        || receipts.join(format!("{}.json", hex(n))).exists()
}

fn policy(target_bytes: Option<u64>) -> Policy {
    Policy {
        retention: Some(30 * DAY),
        protect: DAY,
        target_bytes,
        ceiling_bytes: None,
    }
}

/// An over-cap store in both layouts is brought down to its target, oldest
/// first: recent receipts, receipts a latest index lists and receipts of a
/// task in progress stay; expired ones go; a task index none of whose
/// receipts is left goes too; plain receipts are packed, their time kept.
#[test]
fn an_over_cap_store_is_pruned_oldest_first_and_keeps_its_protected_set() {
    let store = tempfile::tempdir().unwrap();
    let dir = store.path();
    for n in 0..40 {
        // Oldest first: n = 0 is 21 days old, n = 39 two days.
        let layout = if n % 2 == 0 { "json" } else { "json.zst" };
        receipt(dir, n, layout, 64 * 1024, 21 - u64::from(n) / 2);
    }
    receipt(dir, 100, "json.zst", 64 * 1024, 0); // inside the protected window
    receipt(dir, 101, "json", 64 * 1024, 45); // expired, unprotected
    receipt(dir, 102, "json.zst", 64 * 1024, 45); // expired, but latest-indexed
    receipt(dir, 103, "json.zst", 64 * 1024, 30); // old, of a task in progress
    index(dir, "latest", "p", &[102], 0);
    index(dir, "tasks", "open", &[103], 0);
    let done_task = index(dir, "tasks", "done", &[0, 1], 3);
    std::fs::create_dir_all(dir.join("traces")).unwrap();
    let trace = dir.join("traces").join(format!("{}.json", hex(0)));
    std::fs::write(&trace, b"{}").unwrap();

    let before = super::super::capacity::booked_bytes(dir).unwrap();
    let target = before / 2;
    let done = maintain_in(dir, SystemTime::now(), policy(Some(target))).unwrap();

    let after = super::super::capacity::booked_bytes(dir).unwrap();
    assert!(after <= target, "{after} > {target}");
    assert!(done.removed > 0 && done.packed > 0, "{done:?}");
    for kept in [100, 102, 103] {
        assert!(exists(dir, kept), "protected receipt {kept} was removed");
    }
    assert!(!exists(dir, 101), "an expired unprotected receipt stays");
    assert!(
        !exists(dir, 0) && !trace.exists(),
        "the oldest goes with its sidecar: {done:?}"
    );
    assert!(exists(dir, 39), "the newest unprotected receipts stay");
    assert!(
        !done_task.exists(),
        "a task index with no receipt left stays"
    );
    assert!(dir.join("tasks").join("open.json").exists());
    // No plain receipt is left, and packing kept each receipt's time.
    let plain = std::fs::read_dir(dir.join("receipts"))
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
        .count();
    assert_eq!(plain, 0);
    let packed_38 = dir.join("receipts").join(format!("{}.json.zst", hex(38)));
    let modified = std::fs::metadata(packed_38).unwrap().modified().unwrap();
    assert!(SystemTime::now().duration_since(modified).unwrap() > DAY);
    // The ledger agrees with the files.
    let (bytes, _) = super::super::capacity::count_for_test(dir);
    assert_eq!(bytes, after);
    // A second pass has nothing left to do.
    let again = maintain_in(dir, SystemTime::now(), policy(Some(target))).unwrap();
    assert_eq!(
        (again.removed, again.packed, again.removed_tasks),
        (0, 0, 0)
    );
}

/// Only protected evidence left: nothing is removed, however full.
#[test]
fn protected_evidence_is_never_removed_for_room() {
    let store = tempfile::tempdir().unwrap();
    let dir = store.path();
    for n in 0..4 {
        receipt(dir, n, "json.zst", 32 * 1024, 0);
    }
    let full = Policy {
        ceiling_bytes: Some(64 * 1024),
        ..policy(Some(1))
    };
    let done = maintain_in(dir, SystemTime::now(), full).unwrap();
    assert_eq!(done.removed, 0);
    assert!((0..4).all(|n| exists(dir, n)));
    assert!(done.full && proven_full(dir), "{done:?}");
}

fn write_entry(dir: &Path, n: u32) -> Result<(), String> {
    let path = dir.join("receipts").join(format!("{}.json.zst", hex(n)));
    super::super::capacity::write(
        dir,
        &path,
        1024,
        super::super::capacity::Kind::Entry,
        || std::fs::write(&path, [0u8; 1024]).map_err(|e| e.to_string()),
    )
}

/// A ledger as a copy of another store carries it.
fn claim_ledger(dir: &Path, bytes: u64, objects: u64) {
    std::fs::write(
        dir.join(".usage.json"),
        format!(r#"{{"schema_version":1,"bytes":{bytes},"objects":{objects},"reserved":[]}}"#),
    )
    .unwrap();
}

/// A write over the ceiling is admitted until a pass has counted the files
/// and found protected evidence filling the store; the pass then brings it
/// down to its target, and only protected evidence refuses.
#[test]
fn a_write_over_the_ceiling_is_refused_only_when_a_count_proves_it_full() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let store = tempfile::tempdir().unwrap();
    let dir = store.path();
    // Two MB of old evidence against a one MB ceiling.
    for n in 0..32 {
        receipt(dir, n, "json.zst", 64 * 1024, 5);
    }
    let _ceiling = crate::test_env::EnvVarGuard::set("LEAN_CTX_GATEWAY_STORE_MAX_MB", "1");
    write_entry(dir, 500).expect("admitted: nothing proves the store full");
    let done = maintain_in(dir, SystemTime::now(), Policy::current()).unwrap();
    assert!(!done.full && done.removed > 0, "{done:?}");
    assert!(super::super::capacity::booked_bytes(dir).unwrap() <= 1_048_576);
    write_entry(dir, 501).expect("room was made");

    // Fill it with protected evidence: once a pass counted that, the write
    // is refused.
    for n in 600..640 {
        receipt(dir, n, "json.zst", 64 * 1024, 0);
    }
    super::super::capacity::recount_later(dir);
    write_entry(dir, 502).expect("still admitted: not counted yet");
    let done = maintain_in(dir, SystemTime::now(), Policy::current()).unwrap();
    assert!(done.full && proven_full(dir), "{done:?}");
    assert!(write_entry(dir, 700).is_err());
    assert!(!exists(dir, 700));
}

/// A copied store whose ledger books 7 GB for a few files, and whose stamp
/// says full: neither is this store's count, so writes are admitted, and
/// the next pass replaces the ledger with the count.
#[test]
fn a_copied_ledger_that_overstates_refuses_nothing_and_is_recounted() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let store = tempfile::tempdir().unwrap();
    let dir = store.path();
    for n in 0..8 {
        receipt(dir, n, "json.zst", 4 * 1024, 0);
    }
    claim_ledger(dir, 7 << 30, 250_056);
    std::fs::write(dir.join(".maintenance"), br#"{"store":"0:1","full":true}"#).unwrap();
    let _ceiling = crate::test_env::EnvVarGuard::set("LEAN_CTX_GATEWAY_STORE_MAX_MB", "1");
    assert!(!proven_full(dir), "another store's stamp proves nothing");
    assert!(last_pass(dir).is_none(), "nor is it this store's last pass");
    for n in 100..110 {
        write_entry(dir, n).expect("admitted against a copied ledger");
    }
    let done = maintain_in(dir, SystemTime::now(), Policy::current()).unwrap();
    assert!(!done.full, "{done:?}");
    assert!(last_pass(dir).is_some());
    let (bytes, objects) = super::super::capacity::count_for_test(dir);
    assert_eq!(super::super::capacity::booked_bytes(dir).unwrap(), bytes);
    assert_eq!(objects, 18);
    write_entry(dir, 200).expect("admitted after the recount");
}

/// A ledger that understates (it books nothing for two MB of old evidence,
/// over a two MB ceiling with the next entry) admits writes, and the next pass counts the
/// files and brings the store to its target.
#[test]
fn a_ledger_that_understates_is_recounted_and_the_store_brought_down() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let store = tempfile::tempdir().unwrap();
    let dir = store.path();
    for n in 0..32 {
        receipt(dir, n, "json.zst", 64 * 1024, 5);
    }
    claim_ledger(dir, 0, 0);
    let _ceiling = crate::test_env::EnvVarGuard::set("LEAN_CTX_GATEWAY_STORE_MAX_MB", "2");
    write_entry(dir, 100).expect("fits as booked");
    let done = maintain_in(dir, SystemTime::now(), Policy::current()).unwrap();
    assert!(done.removed > 0 && !done.full, "{done:?}");
    let (bytes, _) = super::super::capacity::count_for_test(dir);
    assert_eq!(super::super::capacity::booked_bytes(dir).unwrap(), bytes);
    assert!(bytes <= Policy::current().target_bytes.unwrap(), "{bytes}");
    write_entry(dir, 101).expect("room after the pass");
}

/// A store of 100 000 files with a copied, overstated ledger, then with a
/// stale one: the first write lists no directory — nothing is counted on
/// the write path — and is stored. Counted operations, not wall-clock time,
/// so a loaded machine cannot fail it; the times are printed only.
#[test]
fn the_first_write_to_a_store_of_100k_files_counts_nothing() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let store = tempfile::tempdir().unwrap();
    let dir = store.path();
    let receipts = dir.join("receipts");
    std::fs::create_dir_all(&receipts).unwrap();
    for n in 0..100_000u32 {
        std::fs::write(receipts.join(format!("{}.json.zst", hex(n))), [0u8; 64]).unwrap();
    }
    claim_ledger(dir, 7 << 30, 250_056);
    let _ceiling = crate::test_env::EnvVarGuard::set("LEAN_CTX_GATEWAY_STORE_MAX_MB", "1024");
    for (n, stale) in [(200_000, false), (200_001, true)] {
        if stale {
            super::super::capacity::recount_later(dir);
        }
        crate::core::atomic_fs::listings::take();
        let started = std::time::Instant::now();
        write_entry(dir, n).expect("admitted");
        eprintln!("stale={stale}: first write {:?}", started.elapsed());
        assert_eq!(
            crate::core::atomic_fs::listings::take(),
            0,
            "stale={stale}: the write path listed a directory"
        );
        assert!(exists(dir, n));
    }

    // A receipt write lists no directory: the sweep for orphaned temps
    // listed all 100 000 receipts on every write, under the ledger lock.
    let p50 = |swept: bool, first: u32| {
        crate::core::atomic_fs::listings::take();
        let mut took: Vec<Duration> = (first..first + 41)
            .map(|n| {
                let path = receipts.join(format!("{}.json.zst", hex(n)));
                let started = std::time::Instant::now();
                super::super::capacity::write(
                    dir,
                    &path,
                    1024,
                    super::super::capacity::Kind::FollowUp,
                    || {
                        let written = if swept {
                            crate::core::atomic_fs::try_atomic_write(&path, &[0u8; 1024], None)
                        } else {
                            crate::core::atomic_fs::try_atomic_write_unswept(&path, &[0u8; 1024])
                        };
                        written.map_err(|e| e.to_string())
                    },
                )
                .expect("written");
                started.elapsed()
            })
            .collect();
        took.sort();
        (
            took[took.len() / 2],
            crate::core::atomic_fs::listings::take(),
        )
    };
    let (swept, swept_listings) = p50(true, 300_000);
    let (unswept, unswept_listings) = p50(false, 400_000);
    eprintln!("receipt write p50 in a 100k store: swept {swept:?}, unswept {unswept:?}");
    assert!(
        swept_listings >= 41,
        "the sweep lists the store: {swept_listings}"
    );
    assert_eq!(unswept_listings, 0, "a receipt write lists no directory");
}

/// A temp a crashed write left in the store is removed by the walk and not
/// counted; a live writer's temp stays.
#[test]
fn the_walk_removes_orphaned_temps_and_keeps_live_ones() {
    let store = tempfile::tempdir().unwrap();
    let dir = store.path();
    receipt(dir, 1, "json.zst", 1024, 0);
    let receipts = dir.join("receipts");
    let orphan = receipts.join(format!(".{}.json.zst.lean-ctx.tmp.4294967294.1", hex(2)));
    std::fs::write(&orphan, [0u8; 4096]).unwrap();
    age_file(&orphan, 1);
    let live = receipts.join(format!(
        ".{}.json.zst.lean-ctx.tmp.{}.1",
        hex(3),
        std::process::id()
    ));
    std::fs::write(&live, [0u8; 512]).unwrap();
    maintain_in(dir, SystemTime::now(), policy(None)).unwrap();
    assert!(!orphan.exists(), "an orphaned temp stays");
    assert!(live.exists(), "a live writer's temp was removed");
    let (bytes, objects) = super::super::capacity::count_for_test(dir);
    assert_eq!(super::super::capacity::booked_bytes(dir).unwrap(), bytes);
    assert_eq!(objects, 2);
}

/// The task indexes are visited behind a cursor a pass leaves when it is
/// cut short: the next pass continues after it and clears it.
#[test]
fn an_unfinished_task_pass_is_continued_from_its_cursor() {
    let store = tempfile::tempdir().unwrap();
    let dir = store.path();
    std::fs::create_dir_all(dir.join("receipts")).unwrap();
    for name in ["a", "b", "c", "d"] {
        index(dir, "tasks", name, &[9], 3);
    }
    save_cursor(dir, "b.json");
    maintain_in(dir, SystemTime::now(), policy(None)).unwrap();
    let tasks = dir.join("tasks");
    assert!(tasks.join("a.json").exists() && tasks.join("b.json").exists());
    assert!(!tasks.join("c.json").exists() && !tasks.join("d.json").exists());
    assert!(!dir.join(".maintenance.cursor").exists());
}

/// A long-lived process that lives only minutes: every pass is cut short
/// after two batches, and still the passes converge — receipt removals are
/// final, the task indexes continue behind the saved cursor — until one
/// finishes and is stamped.
#[test]
fn passes_cut_short_after_a_few_batches_converge_across_restarts() {
    let store = tempfile::tempdir().unwrap();
    let dir = store.path();
    for n in 0..600 {
        receipt(dir, n, "json.zst", 64, 45);
    }
    for n in 0..1000u32 {
        index(dir, "tasks", &format!("t{n:04}"), &[n % 600], 3);
    }
    let mut runs = 0;
    loop {
        runs += 1;
        assert!(runs <= 10, "the passes do not converge");
        ENDS_AFTER.with(|left| left.set(Some(2)));
        let pass = std::panic::catch_unwind(|| maintain_in(dir, SystemTime::now(), policy(None)));
        ENDS_AFTER.with(|left| left.set(None));
        match pass {
            Ok(done) => {
                done.unwrap();
                break;
            }
            // Cut short: no stamp, and the next process continues.
            Err(_) => assert!(own_stamp(dir).is_none()),
        }
    }
    assert!(runs > 2, "the cut was not exercised: {runs}");
    assert!((0..600).all(|n| !exists(dir, n)));
    let left = std::fs::read_dir(dir.join("tasks")).unwrap().count();
    assert_eq!(left, 0, "after {runs} runs");
    assert!(own_stamp(dir).is_some() && !dir.join(".maintenance.cursor").exists());
    let (bytes, _) = super::super::capacity::count_for_test(dir);
    assert_eq!(super::super::capacity::booked_bytes(dir).unwrap(), bytes);
}
