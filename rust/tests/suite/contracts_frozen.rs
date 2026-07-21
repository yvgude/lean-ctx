//! Contract-freeze CI gate (GL #394, CONTRACTS.md § Stability matrix).
//!
//! Two invariants, enforced on every CI run:
//!
//! 1. **Completeness** — every `docs/contracts/*.md` file is classified in
//!    `core::contracts::contract_docs()` (frozen / stable / experimental) and
//!    every classified entry actually exists on disk. No contract can stay
//!    unclassified (pattern: `feature_keys_partition` in plans.rs).
//! 2. **Immutability** — the content hash of every `frozen` contract doc
//!    matches the committed snapshot `docs/contracts/frozen-hashes.json`. A
//!    drifting hash means someone edited a frozen artifact: semantic changes
//!    must land as a new `-v2.md` file instead. Deliberate typo fixes update
//!    the snapshot via `LEANCTX_UPDATE_FROZEN_HASHES=1 cargo test --test
//!    main suite::contracts_frozen` and must be justified in the PR.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lean_ctx::core::contracts::{ContractStatus, contract_docs};
use sha2::{Digest, Sha256};

fn contracts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/contracts")
}

fn snapshot_path() -> PathBuf {
    contracts_dir().join("frozen-hashes.json")
}

/// Hash with CRLF normalized away so Windows checkouts (autocrlf) agree with
/// the committed snapshot.
fn content_hash(path: &Path) -> String {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let normalized: Vec<u8> = {
        let mut out = Vec::with_capacity(raw.len());
        let mut iter = raw.iter().peekable();
        while let Some(&b) = iter.next() {
            if b == b'\r' && iter.peek() == Some(&&b'\n') {
                continue;
            }
            out.push(b);
        }
        out
    };
    let mut hasher = Sha256::new();
    hasher.update(&normalized);
    use std::fmt::Write;
    hasher.finalize().iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[test]
fn every_contract_doc_is_classified() {
    let dir = contracts_dir();
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            std::path::Path::new(n)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        })
        .collect();
    on_disk.sort();

    let mut classified: Vec<String> = contract_docs()
        .iter()
        .map(|d| d.doc_file.to_string())
        .collect();
    classified.sort();

    let unclassified: Vec<_> = on_disk.iter().filter(|f| !classified.contains(f)).collect();
    assert!(
        unclassified.is_empty(),
        "unclassified contract docs (add them to core::contracts::contract_docs() \
         with a frozen/stable/experimental status): {unclassified:?}"
    );

    let missing: Vec<_> = classified.iter().filter(|f| !on_disk.contains(f)).collect();
    assert!(
        missing.is_empty(),
        "contract_docs() lists files that do not exist in docs/contracts/: {missing:?}"
    );
}

#[test]
fn frozen_contract_docs_are_immutable() {
    let dir = contracts_dir();
    let current: BTreeMap<String, String> = contract_docs()
        .iter()
        .filter(|d| d.status == ContractStatus::Frozen)
        .map(|d| (d.doc_file.to_string(), content_hash(&dir.join(d.doc_file))))
        .collect();
    assert!(!current.is_empty(), "freeze gate without frozen contracts");

    let snap_path = snapshot_path();
    if std::env::var_os("LEANCTX_UPDATE_FROZEN_HASHES").is_some() {
        let json = serde_json::to_string_pretty(&current).expect("serialize snapshot");
        std::fs::write(&snap_path, json + "\n").expect("write snapshot");
        eprintln!("frozen-hashes.json regenerated — justify this in the PR");
        return;
    }

    let snapshot: BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(&snap_path).unwrap_or_else(|e| {
            panic!(
                "missing {} — generate it once via \
                 LEANCTX_UPDATE_FROZEN_HASHES=1 cargo test --test main suite::contracts_frozen ({e})",
                snap_path.display()
            )
        }))
        .expect("frozen-hashes.json is valid JSON");

    for (file, hash) in &current {
        match snapshot.get(file) {
            None => panic!(
                "{file} is frozen but missing from frozen-hashes.json — \
                 regenerate via LEANCTX_UPDATE_FROZEN_HASHES=1 cargo test --test main suite::contracts_frozen"
            ),
            Some(expected) if expected != hash => panic!(
                "FROZEN CONTRACT MODIFIED: docs/contracts/{file} changed.\n\
                 Frozen contracts are immutable (CONTRACTS.md § Contract file rule).\n\
                 → semantic change: create the next version file (e.g. -v2.md) and classify it; \
                 leave the v1 file untouched.\n\
                 → deliberate typo fix: LEANCTX_UPDATE_FROZEN_HASHES=1 cargo test --test \
                 main suite::contracts_frozen, and justify the edit in the PR."
            ),
            Some(_) => {}
        }
    }

    let stale: Vec<_> = snapshot
        .keys()
        .filter(|k| !current.contains_key(*k))
        .collect();
    assert!(
        stale.is_empty(),
        "frozen-hashes.json lists files that are no longer frozen/present: {stale:?} — regenerate the snapshot"
    );
}
