// SPDX-License-Identifier: Apache-2.0
//! Drift gate for the standalone OSS OCLA contract crate.

use sha2::{Digest, Sha256};

/// Every public module of the crate (`lib.rs` declares exactly these).
const CANONICAL_SOURCES: &[&str] = &[
    include_str!("../../crates/lean-ctx-ocla/src/lib.rs"),
    include_str!("../../crates/lean-ctx-ocla/src/decision_verification.rs"),
    include_str!("../../crates/lean-ctx-ocla/src/delivery_scope.rs"),
    include_str!("../../crates/lean-ctx-ocla/src/manifest.rs"),
    include_str!("../../crates/lean-ctx-ocla/src/receipt_verification.rs"),
    include_str!("../../crates/lean-ctx-ocla/src/traits.rs"),
    include_str!("../../crates/lean-ctx-ocla/src/types.rs"),
];

fn normalized(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn public_api_surface(source: &str, output: &mut String) {
    let mut public_block = false;
    let mut block_depth = 0_i32;

    for raw_line in source.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') {
            continue;
        }
        let normalized_line = normalized(line);
        if public_block {
            if line.starts_with('}') {
                block_depth -= 1;
                if block_depth <= 0 {
                    public_block = false;
                }
            } else {
                output.push_str(&normalized_line);
                output.push('\n');
            }
            block_depth += line.matches('{').count() as i32;
            block_depth -= line.matches('}').count() as i32;
            if block_depth <= 0 {
                public_block = false;
            }
            continue;
        }

        let is_public_type = line.starts_with("pub struct ")
            || line.starts_with("pub enum ")
            || line.starts_with("pub trait ")
            || line.starts_with("pub type ");
        let is_public_item = (line.starts_with("pub ") && line.contains(" fn "))
            || line.starts_with("pub const ")
            || line.starts_with("pub static ")
            || line.starts_with("pub use ");
        if is_public_type {
            output.push_str(&normalized_line);
            output.push('\n');
            block_depth = line.matches('{').count() as i32;
            block_depth -= line.matches('}').count() as i32;
            public_block = block_depth > 0;
        } else if is_public_item {
            output.push_str(&normalized_line);
            output.push('\n');
        }
    }
}

fn canonical_public_api_hash() -> String {
    let mut surface = String::new();
    for source in CANONICAL_SOURCES {
        public_api_surface(source, &mut surface);
    }
    let digest = Sha256::digest(surface.as_bytes());
    digest
        .iter()
        .fold(String::with_capacity(64), |mut acc, byte| {
            use std::fmt::Write;
            write!(acc, "{byte:02x}").unwrap();
            acc
        })
}

/// The modules hashed above besides `lib.rs`, in `CANONICAL_SOURCES` order.
const HASHED_MODULES: &[&str] = &[
    "decision_verification",
    "delivery_scope",
    "manifest",
    "receipt_verification",
    "traits",
    "types",
];

#[test]
fn every_public_ocla_module_is_hashed() {
    let mut declared: Vec<&str> = CANONICAL_SOURCES[0]
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pub mod "))
        .filter_map(|rest| rest.strip_suffix(';'))
        .collect();
    declared.sort_unstable();
    assert_eq!(
        declared, HASHED_MODULES,
        "a public OCLA module is missing from the drift check; add its source to CANONICAL_SOURCES"
    );
    assert_eq!(CANONICAL_SOURCES.len(), HASHED_MODULES.len() + 1);
}

#[test]
fn oss_ocla_public_api_matches_pinned_fixture() {
    let expected = include_str!("../fixtures/ocla_public_api.sha256").trim();
    let actual = canonical_public_api_hash();
    assert_eq!(
        actual, expected,
        "OSS OCLA public API drifted; review the diff and update the pinned fixture intentionally"
    );
}
