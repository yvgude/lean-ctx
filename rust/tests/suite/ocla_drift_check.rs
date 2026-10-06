// SPDX-License-Identifier: Apache-2.0
//! Drift gate for the standalone OSS OCLA contract crate.

use sha2::{Digest, Sha256};

const CANONICAL_SOURCES: &[&str] = &[
    include_str!("../../crates/lean-ctx-ocla/src/lib.rs"),
    include_str!("../../crates/lean-ctx-ocla/src/failure.rs"),
    include_str!("../../crates/lean-ctx-ocla/src/manifest.rs"),
    include_str!("../../crates/lean-ctx-ocla/src/observation.rs"),
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

#[test]
fn oss_ocla_public_api_matches_pinned_fixture() {
    let expected = include_str!("../fixtures/ocla_public_api.sha256").trim();
    let actual = canonical_public_api_hash();
    assert_eq!(
        actual, expected,
        "OSS OCLA public API drifted; review the diff and update the pinned fixture intentionally"
    );
}
