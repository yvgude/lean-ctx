// SPDX-License-Identifier: Apache-2.0
//! Single merged integration-test harness. See suite/mod.rs.
//!
//! The suite drives the `lean-ctx` binary, so it is a test target of this
//! package; its modules and fixtures stay beside the engine in `rust/tests/`.

#[path = "../../../tests/suite/mod.rs"]
mod suite;
#[path = "../../../tests/support/mod.rs"]
mod support;

/// The engine crate directory (`rust/`): the suite reads its sources, assets
/// and fixtures and runs the binary from there.
fn engine_dir() -> &'static std::path::Path {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/lean-ctx sits two levels below the engine")
}
