use semver::Version;

use super::{
    CapabilityManifest, CapabilityProperties, CapabilityRegistry, CapabilityType, ExecutionMode,
    IOContract, LegacyManifestAdapterError, Permission, RegistryError, builtins,
};

use lean_ctx_protocol::{CapabilityKind, DataMovement, Determinism, Reversibility};

fn test_manifest() -> CapabilityManifest {
    CapabilityManifest {
        id: "leanctx.retrieval.test".to_owned(),
        version: Version::new(0, 1, 0),
        capability_type: CapabilityType::Retrieval,
        execution_mode: ExecutionMode::InProcess,
        input_contract: IOContract {
            content_type: "application/json".to_owned(),
            max_size_bytes: Some(1_024),
            schema: Some("https://leanctx.dev/schemas/retrieval-input.json".to_owned()),
        },
        output_contract: IOContract {
            content_type: "application/json".to_owned(),
            max_size_bytes: Some(4_096),
            schema: Some("https://leanctx.dev/schemas/retrieval-output.json".to_owned()),
        },
        properties: CapabilityProperties {
            lossy: false,
            recoverable: true,
            cache_safe: true,
            deterministic: true,
            max_latency_ms: Some(100),
        },
        permissions: vec![Permission::ReadFileSystem],
    }
}

#[test]
fn manifest_round_trips_through_json() {
    let manifest = test_manifest();

    let serialized = serde_json::to_string(&manifest).expect("manifest serializes");
    let restored: CapabilityManifest =
        serde_json::from_str(&serialized).expect("manifest deserializes");

    assert_eq!(restored, manifest);
    assert!(serialized.contains("\"version\":\"0.1.0\""));
}

#[test]
fn registry_registers_and_looks_up_capabilities() {
    let manifest = test_manifest();
    let mut registry = CapabilityRegistry::new();

    registry
        .register(manifest.clone())
        .expect("manifest is valid");

    assert_eq!(registry.get(&manifest.id), Some(&manifest));
    assert_eq!(
        registry.list_by_type(CapabilityType::Retrieval),
        vec![&manifest]
    );
    assert!(
        registry
            .list_by_type(CapabilityType::Compression)
            .is_empty()
    );
    assert_eq!(
        registry.register(manifest).unwrap_err(),
        RegistryError::DuplicateCapability("leanctx.retrieval.test".to_owned())
    );
}

#[test]
fn validation_rejects_invalid_manifests() {
    let mut manifest = test_manifest();
    manifest.id = " ".to_owned();

    assert_eq!(
        CapabilityRegistry::validate(&manifest).unwrap_err(),
        RegistryError::EmptyField("id")
    );

    manifest.id = "leanctx.retrieval.test".to_owned();
    manifest.execution_mode = ExecutionMode::Remote {
        endpoint: " ".to_owned(),
    };
    assert_eq!(
        CapabilityRegistry::validate(&manifest).unwrap_err(),
        RegistryError::EmptyRemoteEndpoint
    );
}

#[test]
fn builtin_structural_compression_manifest_loads() {
    let manifest = builtins::structural_compression_manifest();
    let mut registry = CapabilityRegistry::new();

    registry
        .register(manifest.clone())
        .expect("builtin manifest is valid");

    assert_eq!(manifest.id, "leanctx.compression.structural");
    assert_eq!(manifest.capability_type, CapabilityType::Compression);
    assert_eq!(manifest.execution_mode, ExecutionMode::InProcess);
    assert!(manifest.properties.lossy);
    assert!(manifest.properties.deterministic);
    assert!(manifest.permissions.contains(&Permission::ReadFileSystem));
    assert_eq!(registry.get(&manifest.id), Some(&manifest));
}

#[test]
fn legacy_adapter_maps_supported_types_and_preserves_all_fields() {
    let mut legacy = test_manifest();
    legacy.permissions = vec![
        Permission::ReadFileSystem,
        Permission::WriteFileSystem,
        Permission::NetworkAccess,
        Permission::ModelInference,
        Permission::ShellExecution,
    ];

    let canonical = legacy.try_into_v1().expect("legacy manifest converts");
    assert_eq!(canonical.capability_id.as_str(), legacy.id);
    assert_eq!(canonical.kind, CapabilityKind::SearchRetrieval);
    assert_eq!(canonical.version, "0.1.0");
    assert!(canonical.local);
    assert!(!canonical.remote);
    assert_eq!(canonical.data_movement, DataMovement::None);
    assert_eq!(canonical.reversibility, Reversibility::Reversible);
    assert_eq!(canonical.determinism, Determinism::Deterministic);
    assert!(canonical.measurement_support.latency);
    assert_eq!(
        canonical.extra.get("legacy_manifest"),
        Some(&serde_json::to_value(&legacy).expect("legacy serializes"))
    );
    canonical.validate().expect("adapted manifest validates");

    let via_trait =
        lean_ctx_protocol::CapabilityManifestV1::try_from(&legacy).expect("trait adapter converts");
    assert_eq!(via_trait, canonical);

    let mut compression = test_manifest();
    compression.capability_type = CapabilityType::Compression;
    let canonical_compression = compression
        .try_into_v1()
        .expect("legacy compression manifest converts");
    assert_eq!(
        canonical_compression.kind,
        CapabilityKind::ReadCompressionStrategy
    );
    assert_eq!(
        canonical_compression.extra.get("legacy_manifest"),
        Some(&serde_json::to_value(&compression).expect("legacy compression serializes"))
    );
    canonical_compression
        .validate()
        .expect("adapted compression manifest validates");
}

#[test]
fn legacy_adapter_maps_local_binary_and_remote_modes() {
    let mut local_binary = test_manifest();
    local_binary.execution_mode = ExecutionMode::LocalBinary;
    let local = local_binary
        .try_into_v1()
        .expect("local binary manifest converts");
    assert!(local.local);
    assert!(!local.remote);
    assert_eq!(local.data_movement, DataMovement::LocalOnly);

    let mut remote = test_manifest();
    remote.execution_mode = ExecutionMode::Remote {
        endpoint: "https://provider.example".to_owned(),
    };
    let remote_v1 = remote.try_into_v1().expect("remote manifest converts");
    assert!(!remote_v1.local);
    assert!(remote_v1.remote);
    assert_eq!(remote_v1.data_movement, DataMovement::Remote);
    assert_eq!(
        remote_v1.extra["legacy_manifest"]["execution_mode"]["Remote"]["endpoint"],
        "https://provider.example"
    );
}

#[test]
fn legacy_adapter_rejects_unrepresentable_capability_types() {
    for capability_type in [
        CapabilityType::Caching,
        CapabilityType::Selection,
        CapabilityType::Recovery,
        CapabilityType::Measurement,
        CapabilityType::Routing,
    ] {
        let mut legacy = test_manifest();
        legacy.capability_type = capability_type;
        assert_eq!(
            legacy.try_into_v1().unwrap_err(),
            LegacyManifestAdapterError::UnsupportedCapabilityType(capability_type)
        );
    }
}

#[test]
fn legacy_adapter_fails_closed_on_canonical_identifier_bounds() {
    let mut legacy = test_manifest();
    legacy.id = "a".repeat(257);
    assert!(matches!(
        legacy.try_into_v1(),
        Err(LegacyManifestAdapterError::InvalidField { field: "id", .. })
    ));
}
