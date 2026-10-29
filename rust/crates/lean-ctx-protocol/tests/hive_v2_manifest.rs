// SPDX-License-Identifier: Apache-2.0
//! The engine's copy of the C0 2.0 fixtures (`tests/fixtures/hive-v2`) is
//! a byte copy of the Hive contract tree, made by
//! `scripts/sync-hive-c0.sh`. Its MANIFEST.sha256 is canonical text: one
//! `<sha256>  <path>` line per file, the files exactly, `LC_ALL=C` sorted,
//! and every file hashes as listed. A hand edit or a partial copy fails here.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hive-v2")
}

fn files_under(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            files_under(root, &path, out);
        } else {
            let rel = path.strip_prefix(root).expect("under root");
            out.insert(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[test]
fn the_hive_v2_copy_verifies_against_its_manifest() {
    let root = fixture_root();
    let manifest = std::fs::read_to_string(root.join("MANIFEST.sha256")).expect("MANIFEST.sha256");
    assert!(manifest.ends_with('\n'), "MANIFEST ends with a newline");
    let mut listed: Vec<&str> = Vec::new();
    for (n, line) in manifest.lines().enumerate() {
        let (hash, path) = line
            .split_once("  ")
            .unwrap_or_else(|| panic!("MANIFEST line {}: not `<sha256>  <path>`", n + 1));
        assert!(
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "MANIFEST line {}: bad hash {hash:?}",
            n + 1
        );
        // Raw bytes: a line-ending conversion of the checkout fails here too.
        let bytes = std::fs::read(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        let actual = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(actual, hash, "{path}: content differs from MANIFEST");
        listed.push(path);
    }
    // `LC_ALL=C sort` is byte order.
    let mut sorted = listed.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        listed, sorted,
        "MANIFEST is not LC_ALL=C sorted (or lists a file twice)"
    );

    let mut present = BTreeSet::new();
    files_under(&root, &root, &mut present);
    present.remove("MANIFEST.sha256");
    let listed: BTreeSet<String> = listed.into_iter().map(str::to_owned).collect();
    let unlisted: Vec<&String> = present.difference(&listed).collect();
    let missing: Vec<&String> = listed.difference(&present).collect();
    assert!(
        unlisted.is_empty() && missing.is_empty(),
        "copy and MANIFEST disagree; resync with scripts/sync-hive-c0.sh\n\
         unlisted: {unlisted:?}\nmissing: {missing:?}"
    );
}
