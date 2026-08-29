use super::*;
use crate::core::context_package::content::PackageContent;
use crate::core::context_package::manifest::{
    CompatibilitySpec, PackageIntegrity, PackageLayer, PackageProvenance, PackageStats,
};
use chrono::Utc;

fn signed_bundle_doc() -> String {
    let content = PackageContent::default();
    // Arbitrary content text: verification hashes the document bytes and
    // never re-parses content into a typed struct.
    let content_json = r#"{"note":"hello","weight":1.0}"#.to_string();
    let content_hash = sha256_hex(content_json.as_bytes());
    let sha = sha256_hex(format!("vt-pkg:1.0.0:{content_hash}").as_bytes());

    let mut manifest = PackageManifest {
        schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
        conformance_level: None,
        kind: crate::core::context_package::manifest::PackageKind::default(),
        name: "vt-pkg".into(),
        version: "1.0.0".into(),
        description: "verify test".into(),
        author: None,
        scope: None,
        created_at: Utc::now(),
        updated_at: None,
        layers: vec![PackageLayer::Knowledge],
        dependencies: vec![],
        tags: vec![],
        visibility: None,
        integrity: PackageIntegrity {
            sha256: sha,
            content_hash,
            byte_size: content_json.len() as u64,
        },
        provenance: PackageProvenance {
            tool: "lean-ctx".into(),
            tool_version: "0.0.0".into(),
            project_hash: None,
            source_session_id: None,
        },
        compatibility: CompatibilitySpec::default(),
        stats: PackageStats::default(),
        signature: None,
        graph_summary: None,
        marketplace: None,
    };
    let key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
    super::super::signing::sign_package(&mut manifest, &content, &key);

    format!(
        "{{\"manifest\":{},\"content\":{}}}",
        serde_json::to_string(&manifest).unwrap(),
        content_json
    )
}

#[test]
fn valid_signed_package_passes_all_checks() {
    let report = verify_package_text(&signed_bundle_doc());
    assert!(report.valid(), "errors: {:?}", report.errors);
    assert_eq!(report.structure, CheckOutcome::Pass);
    assert_eq!(report.content_hash, CheckOutcome::Pass);
    assert_eq!(report.package_hash, CheckOutcome::Pass);
    assert_eq!(report.signature, CheckOutcome::Pass);
}

fn test_domain_digest(domain: &str, value: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update(b"\n");
    hasher.update(serde_json::to_vec(value).unwrap());
    format!(
        "sha256:{}",
        crate::core::agent_identity::hex_encode(&hasher.finalize())
    )
}

fn signed_checkpoint_bundle() -> serde_json::Value {
    use crate::core::context_package::content::{
        CHECKPOINT_PACKAGE_SCHEMA_V1, CheckpointPackageContentV1,
    };

    let workspace_id = "123e4567-e89b-42d3-a456-426614174000";
    let checkpoint_id = "123e4567-e89b-42d3-a456-426614174001";
    let source = serde_json::json!({
        "schema_version": "leanctx.source-anchor/v1",
        "source_id": "source-1",
        "kind": "filesystem",
        "canonical_id": "file://source.txt",
        "revision": {"kind": "filesystem", "value": format!("sha256:{}", "1".repeat(64))},
        "freshness": {"observed_at": "2026-08-27T00:00:00Z", "status": "current"},
        "recovery": null,
        "trust": {"level": "local", "evidence_refs": []},
        "scope": {"kind": "project", "value": "project"},
        "engine_binding": null
    });
    let entry = serde_json::json!({
        "schema_version": "leanctx.project-context-entry/v1",
        "entry_id": "123e4567-e89b-42d3-a456-426614174002",
        "category": "facts",
        "value": "portable fact",
        "source_ids": ["source-1"],
        "session_id": null,
        "receipt_refs": [],
        "recovery_refs": [format!("recovery:sha256:{}", "2".repeat(64))]
    });
    let policy = serde_json::json!({
        "schema_version": "leanctx.workspace-policy/v1",
        "allowed_categories": ["constraints", "decisions", "facts", "source_refs", "unresolved_questions"],
        "max_events": 4096,
        "max_context_entries": 256,
        "max_entry_bytes": 65536,
        "max_context_bytes": 1048576,
        "max_sources": 128,
        "max_sessions": 128,
        "allow_external_sources": false
    });
    let package_pin = serde_json::json!({
        "schema_version": "leanctx.package-pin/v1",
        "name": "dependency",
        "version": "1.0.0",
        "artifact_digest": format!("sha256:{}", "3".repeat(64)),
        "manifest_digest": format!("sha256:{}", "4".repeat(64)),
        "content_hash": format!("sha256:{}", "5".repeat(64)),
        "signature_state": "signed_valid",
        "signer_public_key": "6".repeat(64),
        "trust_state": "trusted",
        "policy_decision": "admitted"
    });
    let package_pins = serde_json::json!([package_pin]);
    let lock_digest = test_domain_digest("leanctx.package.lock.v1", &package_pins);
    let logical = serde_json::json!({
        "schema_version": "leanctx.workspace.state/v1",
        "workspace_id": workspace_id,
        "policy": policy,
        "sources": [source],
        "entries": [entry],
        "package_pins": package_pins,
        "package_lock_digest": lock_digest
    });
    let state_digest = test_domain_digest("leanctx.workspace.state.v1", &logical);
    let policy_digest = test_domain_digest("leanctx.workspace.policy.v1", &logical["policy"]);
    let project_context_digest =
        test_domain_digest("leanctx.project-context.state.v1", &logical["entries"]);
    let mut checkpoint = serde_json::json!({
        "schema_version": "leanctx.context-checkpoint/v2",
        "checkpoint_id": checkpoint_id,
        "workspace_id": workspace_id,
        "state_digest": state_digest,
        "state_schema_version": "leanctx.workspace.state/v1",
        "workspace_state_ref": format!("event:sha256:{}", "5".repeat(64)),
        "logical_state": logical,
        "source_anchors": logical["sources"],
        "recovery_refs": [format!("recovery:sha256:{}", "2".repeat(64))],
        "package_pins": logical["package_pins"],
        "package_lock_digest": lock_digest,
        "policy_digest": policy_digest,
        "project_context_digest": project_context_digest,
        "lineage": {"kind": "workspace", "workspace_id": workspace_id, "state_id": format!("sha256:{}", "6".repeat(64))},
        "engine_identity": {"interface_version": "1.0.0", "schema_version": 1, "transport_version": 1},
        "sdk_contract": "leanctx-product-sdk-research/p6"
    });
    let envelope_digest = test_domain_digest("leanctx.checkpoint.envelope.v2", &checkpoint);
    checkpoint["envelope_digest"] = envelope_digest.into();
    let portable = CheckpointPackageContentV1 {
        schema_version: CHECKPOINT_PACKAGE_SCHEMA_V1.into(),
        migration_provenance: Some(serde_json::json!({
            "origin": "SnapshotV1",
            "legacy_snapshot_id": "snapshot-1",
            "legacy_snapshot_digest": format!("sha256:{}", "7".repeat(64)),
            "migration_contract": "leanctx.snapshot-v1-migration/v1",
            "checkpoint_id": checkpoint_id,
            "state_digest": state_digest,
            "limitations": ["local recovery requires explicit rebinding"]
        })),
        checkpoint,
        non_portable_fields: vec![],
    };
    let (mut manifest, content) =
        crate::core::context_package::PackageBuilder::new("checkpoint-fixture", "1.0.0")
            .description("checkpoint fixture")
            .checkpoint(portable)
            .build()
            .unwrap();
    let content_value = serde_json::to_value(&content).unwrap();
    let content_json = serde_json::to_string(&content_value).unwrap();
    let content_hash = sha256_hex(content_json.as_bytes());
    manifest.integrity.content_hash.clone_from(&content_hash);
    manifest.integrity.sha256 =
        sha256_hex(format!("{}:{}:{content_hash}", manifest.name, manifest.version).as_bytes());
    manifest.integrity.byte_size = content_json.len() as u64;
    let key = ed25519_dalek::SigningKey::from_bytes(&[11u8; 32]);
    super::super::signing::sign_package(&mut manifest, &content, &key);
    serde_json::json!({"manifest": manifest, "content": content_value})
}

fn rehash_package_without_resigning(bundle: &mut serde_json::Value) {
    let content = serde_json::to_string(&bundle["content"]).unwrap();
    let content_hash = sha256_hex(content.as_bytes());
    let name = bundle["manifest"]["name"].as_str().unwrap();
    let version = bundle["manifest"]["version"].as_str().unwrap();
    let package_hash = sha256_hex(format!("{name}:{version}:{content_hash}").as_bytes());
    bundle["manifest"]["integrity"]["content_hash"] = content_hash.into();
    bundle["manifest"]["integrity"]["sha256"] = package_hash.into();
    bundle["manifest"]["integrity"]["byte_size"] = content.len().into();
}

#[test]
fn checkpoint_package_is_additive_signed_v2_and_generic_load_rejects() {
    let bundle = signed_checkpoint_bundle();
    let report = verify_package_text(&serde_json::to_string(&bundle).unwrap());
    assert!(report.valid(), "errors: {:?}", report.errors);
    assert_eq!(report.signature, CheckOutcome::Pass);
    let manifest: PackageManifest = serde_json::from_value(bundle["manifest"].clone()).unwrap();
    let content: PackageContent = serde_json::from_value(bundle["content"].clone()).unwrap();
    assert_eq!(manifest.schema_version, 2);
    assert_eq!(manifest.kind, PackageKind::Context);
    assert!(manifest.has_layer(PackageLayer::Checkpoint));
    assert!(super::super::loader::load_package(&manifest, &content, ".").is_err());
}

#[test]
fn pre_extension_reader_fails_closed_on_checkpoint_layer() {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum LegacyLayer {
        Knowledge,
        Graph,
        Session,
        Patterns,
        Gotchas,
    }
    #[derive(serde::Deserialize)]
    struct LegacyManifest {
        layers: Vec<LegacyLayer>,
    }
    let bundle = signed_checkpoint_bundle();
    let old = serde_json::from_value::<LegacyManifest>(bundle["manifest"].clone());
    assert!(old.is_err());
}

#[test]
fn every_checkpoint_critical_field_is_signature_bound() {
    let mutations: &[(&str, fn(&mut serde_json::Value))] = &[
        ("checkpoint_id", |value| {
            value["content"]["checkpoint"]["checkpoint"]["checkpoint_id"] =
                "123e4567-e89b-42d3-a456-426614174099".into();
        }),
        ("state_digest", |value| {
            value["content"]["checkpoint"]["checkpoint"]["state_digest"] =
                format!("sha256:{}", "0".repeat(64)).into();
        }),
        ("workspace_id", |value| {
            value["content"]["checkpoint"]["checkpoint"]["workspace_id"] =
                "123e4567-e89b-42d3-a456-426614174098".into();
        }),
        ("source_revision", |value| {
            value["content"]["checkpoint"]["checkpoint"]["source_anchors"][0]["revision"]["value"] =
                format!("sha256:{}", "8".repeat(64)).into();
        }),
        ("recovery_ref", |value| {
            value["content"]["checkpoint"]["checkpoint"]["recovery_refs"][0] =
                format!("recovery:sha256:{}", "8".repeat(64)).into();
        }),
        ("project_context", |value| {
            value["content"]["checkpoint"]["checkpoint"]["logical_state"]["entries"][0]["value"] =
                "tampered".into();
        }),
        ("policy_digest", |value| {
            value["content"]["checkpoint"]["checkpoint"]["policy_digest"] =
                format!("sha256:{}", "8".repeat(64)).into();
        }),
        ("package_pin", |value| {
            value["content"]["checkpoint"]["checkpoint"]["package_pins"][0]["artifact_digest"] =
                format!("sha256:{}", "8".repeat(64)).into();
        }),
        ("migration", |value| {
            value["content"]["checkpoint"]["migration_provenance"]["legacy_snapshot_id"] =
                "tampered".into();
        }),
        ("logical_state", |value| {
            value["content"]["checkpoint"]["checkpoint"]["logical_state"]["workspace_id"] =
                "123e4567-e89b-42d3-a456-426614174097".into();
        }),
    ];
    for (name, mutate) in mutations {
        let mut bundle = signed_checkpoint_bundle();
        mutate(&mut bundle);
        rehash_package_without_resigning(&mut bundle);
        let report = verify_package_text(&serde_json::to_string(&bundle).unwrap());
        assert!(!report.valid(), "{name} tamper unexpectedly passed");
    }
}

#[test]
fn checkpoint_layer_content_secret_and_path_rules_fail_closed() {
    let bundle = signed_checkpoint_bundle();
    let mut manifest: PackageManifest = serde_json::from_value(bundle["manifest"].clone()).unwrap();
    let mut content: PackageContent = serde_json::from_value(bundle["content"].clone()).unwrap();

    manifest.layers.clear();
    let errors = validate_kind_coherence(&manifest, &content).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.contains("present together"))
    );

    manifest.layers.push(PackageLayer::Checkpoint);
    content.checkpoint.as_mut().unwrap().checkpoint["logical_state"]["entries"][0]["value"] =
        ("AK".to_owned() + "IAABCDEFGHIJKLMNOP").into();
    content.checkpoint.as_mut().unwrap().checkpoint["source_anchors"][0]["engine_binding"] = serde_json::json!({
        "path": "source.txt",
        "project_root": "/machine/one/project",
        "media_type": "text/plain",
        "source_ref": null,
        "source_digest": null
    });
    let errors = validate_kind_coherence(&manifest, &content).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.contains("credential-shaped"))
    );
    assert!(
        errors
            .iter()
            .any(|error| error.contains("non_portable_fields"))
    );
}

#[test]
fn checkpoint_nested_contract_rejects_invalid_structure() {
    let bundle = signed_checkpoint_bundle();
    let manifest: PackageManifest = serde_json::from_value(bundle["manifest"].clone()).unwrap();
    let mut content: PackageContent = serde_json::from_value(bundle["content"].clone()).unwrap();
    let checkpoint = &mut content.checkpoint.as_mut().unwrap().checkpoint;
    checkpoint["logical_state"]["sources"][0]["trust"]["level"] = "verified".into();
    checkpoint["logical_state"]["sources"][0]["trust"]["evidence_refs"] =
        serde_json::json!(["forged"]);
    checkpoint["logical_state"]["entries"][0]["source_ids"] = serde_json::json!(["missing-source"]);
    checkpoint["logical_state"]["policy"]["allow_external_sources"] = "false".into();
    checkpoint["lineage"]["state_id"] = "not-a-digest".into();
    checkpoint["source_anchors"] = checkpoint["logical_state"]["sources"].clone();

    let errors = validate_kind_coherence(&manifest, &content).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.contains("source trust is invalid"))
    );
    assert!(errors.iter().any(|error| error.contains("unknown source")));
    assert!(
        errors
            .iter()
            .any(|error| error.contains("workspace policy is invalid"))
    );
    assert!(
        errors
            .iter()
            .any(|error| error.contains("lineage is invalid"))
    );
}

#[test]
fn checkpoint_nested_contract_matches_sdk_bounds_and_ordering() {
    let bundle = signed_checkpoint_bundle();
    let manifest: PackageManifest = serde_json::from_value(bundle["manifest"].clone()).unwrap();
    let mut content: PackageContent = serde_json::from_value(bundle["content"].clone()).unwrap();
    let portable = content.checkpoint.as_mut().unwrap();
    let checkpoint = &mut portable.checkpoint;
    let mut first = checkpoint["logical_state"]["sources"][0].clone();
    first["source_id"] = "source-b".into();
    first["freshness"]["observed_at"] = "2026-08-27T00:00:00+00:00".into();
    first["trust"]["evidence_refs"] = serde_json::json!(["z", "a"]);
    first["recovery"] = serde_json::json!({
        "kind": "archive",
        "immutable_ref": "/machine/recovery"
    });
    first["engine_binding"]["source_ref"] = "bad\nref".into();
    first["engine_binding"]["source_digest"] = format!("sha256:{}", "A".repeat(64)).into();
    let mut second = first.clone();
    second["source_id"] = "source-a".into();
    checkpoint["logical_state"]["sources"] = serde_json::json!([first, second]);
    checkpoint["source_anchors"] = checkpoint["logical_state"]["sources"].clone();
    checkpoint["logical_state"]["entries"][0]["source_ids"] = serde_json::json!(["source-b"]);
    checkpoint["logical_state"]["entries"][0]["value"] = "x".repeat(4097).into();
    checkpoint["logical_state"]["package_pins"][0]["name"] = "bad\nname".into();
    checkpoint["package_pins"] = checkpoint["logical_state"]["package_pins"].clone();

    let errors = validate_kind_coherence(&manifest, &content).unwrap_err();
    for expected in [
        "source ids must be unique and sorted",
        "source freshness is invalid",
        "source trust evidence is invalid",
        "engine binding is invalid",
        "context entry identity/value is invalid",
        "name/version exceeds bounds",
        "non_portable_fields",
    ] {
        assert!(
            errors.iter().any(|error| error.contains(expected)),
            "missing error for {expected}: {errors:?}"
        );
    }
}

#[test]
fn checkpoint_engine_binding_requires_sdk_canonical_projection() {
    let canonical = serde_json::json!({
        "path": "dir/source.txt",
        "project_root": "/project",
        "media_type": "text/plain"
    });
    let mut errors = Vec::new();
    validate_engine_binding(Some(&canonical), Some("filesystem"), &mut errors);
    assert!(errors.is_empty(), "canonical binding rejected: {errors:?}");

    let invalid = serde_json::json!({
        "path": "../escape.txt",
        "project_root": "/project/../other",
        "media_type": "text/plain",
        "source_ref": null,
        "source_digest": null
    });
    validate_engine_binding(Some(&invalid), Some("filesystem"), &mut errors);
    assert!(
        errors
            .iter()
            .any(|error| error.contains("engine binding is invalid"))
    );
}

#[test]
fn default_package_content_serialization_is_byte_compatible() {
    let json = serde_json::to_string(&PackageContent::default()).unwrap();
    assert!(!json.contains("checkpoint"));
}

#[test]
fn unsigned_package_skips_signature() {
    let doc = signed_bundle_doc();
    let mut v: serde_json::Value = serde_json::from_str(&doc).unwrap();
    v["manifest"]["signature"] = serde_json::Value::Null;
    let report = verify_package_text(&serde_json::to_string(&v).unwrap());
    assert_eq!(report.signature, CheckOutcome::Skipped);
}

#[test]
fn tampered_content_fails_content_hash() {
    let doc = signed_bundle_doc().replace("\"hello\"", "\"evil\"");
    let report = verify_package_text(&doc);
    assert_eq!(report.content_hash, CheckOutcome::Fail);
    assert!(!report.valid());
}

#[test]
fn whitespace_only_changes_do_not_break_hashing() {
    // Pretty-printing the document moves bytes around the content member —
    // compaction must recover the writer's exact value literals (incl. 1.0).
    let doc = signed_bundle_doc()
        .replace("\"content\":{", "\"content\": {\n  ")
        .replace(",\"weight\"", ",\n  \"weight\"");
    let report = verify_package_text(&doc);
    assert!(report.valid(), "errors: {:?}", report.errors);
}

#[test]
fn corrupted_signature_fails() {
    let doc = signed_bundle_doc();
    let mut v: serde_json::Value = serde_json::from_str(&doc).unwrap();
    let sig = v["manifest"]["signature"]["value"].as_str().unwrap();
    let flipped = if let Some(rest) = sig.strip_prefix("0000") {
        format!("ffff{rest}")
    } else {
        format!("0000{}", &sig[4..])
    };
    v["manifest"]["signature"]["value"] = flipped.into();
    let report = verify_package_text(&serde_json::to_string(&v).unwrap());
    assert_eq!(report.signature, CheckOutcome::Fail);
}

#[test]
fn missing_manifest_fails_structure() {
    let report = verify_package_text("{\"content\":{}}");
    assert_eq!(report.structure, CheckOutcome::Fail);
    assert!(report.errors[0].contains("manifest"));
}

// --- kind ↔ payload coherence (GH #726) ---

const COHERENT_ADDON_TOML: &str = r#"
[addon]
name = "sample-addon"
version = "1.2.0"
description = "Markdown skills runtime"

[mcp]
transport = "stdio"
command = "sample-addon"
args = ["serve"]
"#;

fn kinded_manifest(kind: super::PackageKind, name: &str, version: &str) -> PackageManifest {
    PackageManifest {
        schema_version: crate::core::contracts::CONTEXT_PACKAGE_V2_SCHEMA_VERSION,
        conformance_level: None,
        kind,
        name: name.into(),
        version: version.into(),
        description: "coherence test".into(),
        author: None,
        scope: None,
        created_at: Utc::now(),
        updated_at: None,
        layers: vec![],
        dependencies: vec![],
        tags: vec![],
        visibility: None,
        integrity: PackageIntegrity {
            sha256: "a".repeat(64),
            content_hash: "b".repeat(64),
            byte_size: 1,
        },
        provenance: PackageProvenance {
            tool: "lean-ctx".into(),
            tool_version: "0.0.0".into(),
            project_hash: None,
            source_session_id: None,
        },
        compatibility: CompatibilitySpec::default(),
        stats: PackageStats::default(),
        signature: None,
        graph_summary: None,
        marketplace: None,
    }
}

fn addon_content(toml: &str) -> PackageContent {
    PackageContent {
        addon: Some(crate::core::context_package::content::AddonContent {
            manifest_toml: toml.to_string(),
        }),
        ..PackageContent::default()
    }
}

#[test]
fn context_pack_with_addon_payload_fails() {
    let manifest = kinded_manifest(super::PackageKind::Context, "plain-pack", "1.0.0");
    let errs = validate_kind_coherence(&manifest, &addon_content(COHERENT_ADDON_TOML))
        .expect_err("must fail");
    assert!(errs[0].contains("requires kind=addon"), "{errs:?}");
}
