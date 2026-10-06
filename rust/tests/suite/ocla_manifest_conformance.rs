// SPDX-License-Identifier: Apache-2.0
//! Repository conformance checks for OCLA capability manifests.

use lean_ctx::core::ocla::{OclaError, adapters::AdapterRegistry};
use lean_ctx_ocla::manifest::validate_manifest;
use lean_ctx_protocol::CapabilityManifestV1;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

fn manifest_paths(directory: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
    {
        let path = entry
            .unwrap_or_else(|error| panic!("read entry in {}: {error}", directory.display()))
            .path();
        if path.is_dir() {
            paths.extend(manifest_paths(&path));
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            paths.push(path);
        }
    }
    paths.sort();
    paths
}

fn load_manifest(path: &Path) -> CapabilityManifestV1 {
    let json =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    serde_json::from_str(&json)
        .unwrap_or_else(|error| panic!("deserialize {}: {error}", path.display()))
}

fn manifest_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/contracts/ocla/capability-manifests")
}

#[test]
fn repository_manifests_deserialize_and_validate() {
    let paths = manifest_paths(&manifest_directory());
    assert!(
        !paths.is_empty(),
        "repository must publish capability manifests"
    );

    let registry = AdapterRegistry::new();
    let mut capability_ids = BTreeSet::new();
    let mut registered_keys = Vec::new();
    for path in paths {
        let manifest = load_manifest(&path);
        assert_eq!(manifest.schema_version, 1, "{}", path.display());
        assert!(
            !manifest.capability_id.as_str().trim().is_empty(),
            "{}",
            path.display()
        );
        assert!(!manifest.surfaces.is_empty(), "{}", path.display());
        validate_manifest(&manifest)
            .unwrap_or_else(|error| panic!("validate {}: {error}", path.display()));
        registry
            .register_descriptor(manifest.clone())
            .unwrap_or_else(|error| panic!("register {}: {error:?}", path.display()));
        let capability_id = manifest.capability_id.as_str().to_owned();
        assert!(
            capability_ids.insert(capability_id.clone()),
            "duplicate capability ID in {}: {capability_id}",
            path.display()
        );
        registered_keys.push((capability_id, manifest.version.clone()));
    }

    assert_eq!(registry.len(), registered_keys.len());
    assert!(
        registry.list_available().is_empty(),
        "metadata-only descriptors must not be executable"
    );
    for (capability_id, version) in registered_keys {
        assert!(
            registry.lookup(&capability_id, &version).is_none(),
            "metadata-only descriptor must not resolve to an adapter: {capability_id}@{version}"
        );
    }
}

#[test]
fn real_registry_rejects_duplicate_capability_id_and_version() {
    let path = manifest_paths(&manifest_directory())
        .into_iter()
        .next()
        .expect("repository must publish a capability manifest");
    let manifest = load_manifest(&path);
    let registry = AdapterRegistry::new();

    registry
        .register_descriptor(manifest.clone())
        .expect("first registration succeeds");
    let error = registry
        .register_descriptor(manifest.clone())
        .expect_err("duplicate ID/version registration must fail");
    match error {
        OclaError::InvalidRequest(message) => assert_eq!(
            message,
            format!(
                "adapter already registered: {}@{}",
                manifest.capability_id.as_str(),
                manifest.version
            )
        ),
        other => panic!("duplicate registration returned unexpected error: {other:?}"),
    }
}
