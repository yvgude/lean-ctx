// SPDX-License-Identifier: Apache-2.0
//! Shipping assets must stay identical to their repository specifications.

use std::{fs, path::Path};

const MANIFESTS: &[&str] = &[
    "leanctx/context-optimization-v1.json",
    "leanctx/passthrough-v1.json",
    "example/word-count-optimizer-v1.json",
    "rtk/rtk-shell-v1.json",
];

const KITS: &[&str] = &["code-review/kit.toml"];

#[test]
fn packaged_manifests_match_the_canonical_contracts() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for manifest in MANIFESTS {
        let packaged = crate_root
            .join("assets/ocla/capability-manifests")
            .join(manifest);
        let canonical = crate_root
            .join("../docs/contracts/ocla/capability-manifests")
            .join(manifest);
        assert_eq!(
            fs::read_to_string(&packaged).expect("packaged manifest is present"),
            fs::read_to_string(&canonical).expect("canonical manifest is present"),
            "{manifest} diverged from its canonical contract"
        );
    }
}

#[test]
fn packaged_kits_match_the_canonical_contracts() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for kit in KITS {
        let packaged = crate_root.join("assets/kits").join(kit);
        let canonical = crate_root.join("../kits").join(kit);
        assert_eq!(
            fs::read_to_string(&packaged).expect("packaged kit is present"),
            fs::read_to_string(&canonical).expect("canonical kit is present"),
            "{kit} diverged from its canonical source"
        );
    }
}
