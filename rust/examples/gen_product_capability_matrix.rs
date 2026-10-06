// SPDX-License-Identifier: Apache-2.0

//! Regenerates the package mirror and the capability matrix from
//! `product/capabilities.toml`. `--public` first rewrites the source itself
//! into its public projection (strategic fields of non-public capabilities
//! withheld); the public export runs it once, after which the plain run is a
//! no-op there.

use std::fs;
use std::path::PathBuf;

use lean_ctx::core::product_capabilities::{ProductCapabilityRegistry, public_projection};

fn main() {
    let public = std::env::args().skip(1).any(|arg| arg == "--public");
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source_path = manifest_dir.join("../product/capabilities.toml");
    let package_mirror = manifest_dir.join("data/product-capabilities.toml");
    let output = manifest_dir.join("../docs/reference/generated/product-capabilities.md");
    let mut source = fs::read_to_string(&source_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", source_path.display()));
    if public {
        source = public_projection(&source)
            .unwrap_or_else(|error| panic!("cannot project {}: {error}", source_path.display()));
        fs::write(&source_path, &source)
            .unwrap_or_else(|error| panic!("failed to write {}: {error}", source_path.display()));
    }
    let registry = ProductCapabilityRegistry::parse(&source)
        .unwrap_or_else(|error| panic!("invalid {}: {error}", source_path.display()));
    fs::write(&package_mirror, &source)
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", package_mirror.display()));
    fs::write(&output, registry.render_markdown())
        .unwrap_or_else(|error| panic!("failed to write {}: {error}", output.display()));
    println!("{}", output.display());
}
