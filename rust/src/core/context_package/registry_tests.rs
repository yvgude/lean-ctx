// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::context_package::manifest::{CompatibilitySpec, PackageStats};

#[test]
fn registry_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let reg = LocalRegistry::open_at(dir.path()).unwrap();

    assert!(reg.list().unwrap().is_empty());

    let manifest = PackageManifest {
        schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
        conformance_level: None,
        kind: crate::core::context_package::manifest::PackageKind::default(),
        name: "test-pkg".into(),
        version: "1.0.0".into(),
        description: "test".into(),
        author: None,
        scope: None,
        created_at: Utc::now(),
        updated_at: None,
        layers: vec![super::super::manifest::PackageLayer::Knowledge],
        dependencies: vec![],
        tags: vec!["rust".into()],
        visibility: None,
        integrity: {
            let c = PackageContent::default();
            let j = serde_json::to_string(&c).unwrap();
            let mut h = Sha256::new();
            h.update(j.as_bytes());
            let ch = crate::core::agent_identity::hex_encode(&h.finalize());
            let composite = format!("test-pkg:1.0.0:{ch}");
            let mut h2 = Sha256::new();
            h2.update(composite.as_bytes());
            let sha = crate::core::agent_identity::hex_encode(&h2.finalize());
            super::super::manifest::PackageIntegrity {
                sha256: sha,
                content_hash: ch,
                byte_size: j.len() as u64,
            }
        },
        provenance: super::super::manifest::PackageProvenance {
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
        redistributable: None,
    };

    let content = PackageContent::default();

    reg.install(&manifest, &content).unwrap();
    let list = reg.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "test-pkg");

    let (loaded_m, _loaded_c) = reg.load_package("test-pkg", "1.0.0").unwrap();
    assert_eq!(loaded_m.name, "test-pkg");

    let removed = reg.remove("test-pkg", None).unwrap();
    assert_eq!(removed, 1);
    assert!(reg.list().unwrap().is_empty());
}

/// A version 1.0.0 manifest whose integrity binds exactly `content`.
fn bound_manifest(name: &str, content: &PackageContent) -> PackageManifest {
    let digest = |bytes: &[u8]| {
        let mut hash = Sha256::new();
        hash.update(bytes);
        crate::core::agent_identity::hex_encode(&hash.finalize())
    };
    let compact = serde_json::to_string(content).unwrap();
    let content_hash = digest(compact.as_bytes());
    PackageManifest {
        schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
        conformance_level: None,
        kind: crate::core::context_package::manifest::PackageKind::default(),
        name: name.into(),
        version: "1.0.0".into(),
        description: "test".into(),
        author: None,
        scope: None,
        created_at: Utc::now(),
        updated_at: None,
        layers: vec![super::super::manifest::PackageLayer::Patterns],
        dependencies: vec![],
        tags: vec![],
        visibility: None,
        integrity: super::super::manifest::PackageIntegrity {
            sha256: digest(format!("{name}:1.0.0:{content_hash}").as_bytes()),
            content_hash,
            byte_size: compact.len() as u64,
        },
        provenance: super::super::manifest::PackageProvenance {
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
        redistributable: None,
    }
}

/// Regression (A–Z audit P1, `pack install` of a registry package): the
/// import verified the writer's bytes but stored this engine's
/// re-serialization of them; a member this engine writes differently (an
/// explicit `null` here, another writer's field set in the wild) made
/// every later load fail "content_hash mismatch" while the package stood
/// installed. The written bytes are stored, so it loads.
#[test]
fn a_package_from_another_writer_loads_as_it_was_verified() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let dir = tempfile::tempdir().unwrap();
    let written =
        r#"{"graph":null,"patterns":{"patterns":[],"exported_at":"2026-01-01T00:00:00Z"}}"#;
    let content: PackageContent = serde_json::from_str(written).unwrap();
    let mut manifest = bound_manifest("foreign", &content);
    let digest = |bytes: &[u8]| crate::core::agent_identity::hex_encode(&Sha256::digest(bytes));
    manifest.integrity.content_hash = digest(written.as_bytes());
    manifest.integrity.sha256 =
        digest(format!("foreign:1.0.0:{}", manifest.integrity.content_hash).as_bytes());
    manifest.integrity.byte_size = written.len() as u64;
    let file = dir.path().join("foreign.ctxpkg");
    let manifest_json = serde_json::to_string(&manifest).unwrap();
    std::fs::write(
        &file,
        format!(r#"{{"manifest":{manifest_json},"content":{written}}}"#),
    )
    .unwrap();

    let reg = LocalRegistry::open_at(&dir.path().join("store")).unwrap();
    reg.import_from_file(&file, SignaturePolicy::AllowUnsigned)
        .unwrap();
    reg.load_package("foreign", "1.0.0")
        .expect("an imported package loads as it was verified");
}

/// AP07-G5: installs shared one temp file name and rewrote the index
/// without a lock, so parallel installs dropped each other's entries.
#[test]
fn parallel_installs_keep_every_index_entry() {
    use std::sync::{Arc, Barrier};
    const INSTALLS: usize = 8;
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let barrier = Arc::new(Barrier::new(INSTALLS));
    let installs: Vec<_> = (0..INSTALLS)
        .map(|index| {
            let (barrier, root) = (Arc::clone(&barrier), dir.path().to_path_buf());
            std::thread::spawn(move || {
                let reg = LocalRegistry::open_at(&root).unwrap();
                let content = PackageContent::default();
                let manifest = bound_manifest(&format!("pkg-{index}"), &content);
                barrier.wait();
                reg.install(&manifest, &content).map(|_| ())
            })
        })
        .collect();
    for install in installs {
        install.join().unwrap().unwrap();
    }
    let reg = LocalRegistry::open_at(dir.path()).unwrap();
    assert_eq!(reg.list().unwrap().len(), INSTALLS);
}

fn rebuilt_content() -> PackageContent {
    PackageContent {
        patterns: Some(super::super::content::PatternsLayer {
            patterns: Vec::new(),
            exported_at: Utc::now(),
        }),
        ..PackageContent::default()
    }
}

/// AP07-G5: a received package installed again at the same name@version
/// silently replaced the content installed under it.
#[test]
fn a_received_version_keeps_its_content() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let reg = LocalRegistry::open_at(dir.path()).unwrap();
    let original = PackageContent::default();
    reg.install(&bound_manifest("pkg", &original), &original)
        .unwrap();
    let changed = rebuilt_content();
    let refused = reg
        .install(&bound_manifest("pkg", &changed), &changed)
        .unwrap_err();
    assert!(refused.contains("already installed with different content"));
    let (_, kept) = reg.load_package("pkg", "1.0.0").unwrap();
    assert!(kept.patterns.is_none(), "installed content was replaced");
    // Installing the identical package again stays idempotent.
    reg.install(&bound_manifest("pkg", &original), &original)
        .unwrap();
}

/// Review B1: `pack create` rebuilds the same name@version from changed
/// project sources; the rebuild must replace the earlier local build and
/// keep its auto_load choice.
#[test]
fn a_local_rebuild_replaces_the_previous_build() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let reg = LocalRegistry::open_at(dir.path()).unwrap();
    let first = PackageContent::default();
    reg.install_local_build(&bound_manifest("pkg", &first), &first)
        .unwrap();
    reg.set_auto_load("pkg", "1.0.0", true, "/work/pkg-project")
        .unwrap();
    let second = rebuilt_content();
    reg.install_local_build(&bound_manifest("pkg", &second), &second)
        .unwrap();
    let (_, stored) = reg.load_package("pkg", "1.0.0").unwrap();
    assert!(stored.patterns.is_some(), "the second build wins");
    let listed = reg.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].auto_loads_into("/work/pkg-project"));
}

#[test]
fn export_import_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let reg = LocalRegistry::open_at(dir.path()).unwrap();

    let content = PackageContent::default();
    let content_json = serde_json::to_string(&content).unwrap();
    let mut h = Sha256::new();
    h.update(content_json.as_bytes());
    let content_hash = crate::core::agent_identity::hex_encode(&h.finalize());

    let manifest = PackageManifest {
        schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
        conformance_level: None,
        kind: crate::core::context_package::manifest::PackageKind::default(),
        name: "export-test".into(),
        version: "2.0.0".into(),
        description: "round trip test".into(),
        author: Some("test".into()),
        scope: None,
        created_at: Utc::now(),
        updated_at: None,
        layers: vec![super::super::manifest::PackageLayer::Knowledge],
        dependencies: vec![],
        tags: vec![],
        visibility: None,
        integrity: {
            let composite = format!("export-test:2.0.0:{content_hash}");
            let mut h2 = Sha256::new();
            h2.update(composite.as_bytes());
            super::super::manifest::PackageIntegrity {
                sha256: crate::core::agent_identity::hex_encode(&h2.finalize()),
                content_hash,
                byte_size: content_json.len() as u64,
            }
        },
        provenance: super::super::manifest::PackageProvenance {
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
        redistributable: None,
    };

    reg.install_local_build(&manifest, &content).unwrap();

    // V02: an own build is exported.
    let export_path = dir.path().join("test.ctxpkg");
    let bytes = reg
        .export_to_file("export-test", "2.0.0", &export_path)
        .unwrap();
    assert!(bytes > 0);

    let reg2 = LocalRegistry::open_at(&dir.path().join("other")).unwrap();
    let json = std::fs::read_to_string(&export_path).unwrap();
    // GHSA-rxv5-4c3x-g4q6 (P): the network door never accepts unsigned content.
    assert!(
        parse_context_transfer(&json)
            .unwrap_err()
            .contains("unsigned")
    );
    for key in ["content", "cont\\u0065nt", "manifest"] {
        let duplicate = format!(
            "{},\"{key}\":{{}}}}",
            json.trim_end().strip_suffix('}').unwrap()
        );
        let error = parse_context_transfer(&duplicate).unwrap_err();
        assert!(error.contains("duplicate field"), "{key}: {error}");
    }
    // GHSA-gvg8-285q-m62g: unsigned local import needs the explicit opt-in.
    let err = reg2
        .import_from_file(&export_path, SignaturePolicy::RequireSigned)
        .unwrap_err();
    assert!(err.contains("--allow-unsigned"), "{err}");
    assert!(reg2.list().unwrap().is_empty());
    let imported = reg2
        .import_from_file(&export_path, SignaturePolicy::AllowUnsigned)
        .unwrap();
    assert_eq!(imported.name, "export-test");
    assert_eq!(reg2.list().unwrap().len(), 1);

    // V02: the recipient holds it, but no grant means no passing on —
    // signed or not, and nothing is written.
    let onward = dir.path().join("onward.ctxpkg");
    let key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
    let refused = reg2
        .export_to_file("export-test", "2.0.0", &onward)
        .unwrap_err();
    assert!(refused.contains("grants no redistribution"), "{refused}");
    let refused = reg2
        .export_to_file_signed("export-test", "2.0.0", &onward, &key, false)
        .unwrap_err();
    assert!(refused.contains("grants no redistribution"), "{refused}");
    assert!(!onward.exists());

    // A grant counts only under the full manifest signature: added to a
    // package after it was signed identity-only, it is refused; signed
    // over the whole manifest, the recipient may pass it on.
    let mut granted = manifest;
    granted.version = "3.0.0".into();
    granted.integrity.sha256 = {
        let mut h = Sha256::new();
        h.update(format!("export-test:3.0.0:{}", granted.integrity.content_hash).as_bytes());
        crate::core::agent_identity::hex_encode(&h.finalize())
    };
    granted.redistributable = Some(true);
    super::super::signing::sign_package(&mut granted, &content, &key);
    let mut identity_only = granted.clone();
    identity_only.version = "3.0.1".into();
    identity_only.integrity.sha256 = {
        let mut h = Sha256::new();
        h.update(format!("export-test:3.0.1:{}", granted.integrity.content_hash).as_bytes());
        crate::core::agent_identity::hex_encode(&h.finalize())
    };
    identity_only.signature = None;
    super::super::signing::sign_package(&mut identity_only, &content, &key);
    identity_only.signature.as_mut().unwrap().manifest_value = None;
    for (file, signed, passes) in [("full", granted, true), ("v1", identity_only, false)] {
        let path = dir.path().join(format!("{file}.ctxpkg"));
        let version = signed.version.clone();
        let bundle = ExportBundle {
            manifest: signed,
            content: content.clone(),
        };
        std::fs::write(&path, serde_json::to_string(&bundle).unwrap()).unwrap();
        reg2.import_from_file(&path, SignaturePolicy::AllowUnsigned)
            .unwrap();
        let onward = reg2.export_to_file("export-test", &version, &onward);
        assert_eq!(onward.is_ok(), passes, "{file}: {onward:?}");
    }
}

#[test]
fn import_rejects_tampered_signature() {
    let dir = tempfile::tempdir().unwrap();
    let reg = LocalRegistry::open_at(dir.path()).unwrap();

    let content = PackageContent::default();
    let content_json = serde_json::to_string(&content).unwrap();
    let mut h = Sha256::new();
    h.update(content_json.as_bytes());
    let content_hash = crate::core::agent_identity::hex_encode(&h.finalize());
    let composite = format!("signed-test:1.0.0:{content_hash}");
    let mut h2 = Sha256::new();
    h2.update(composite.as_bytes());

    let mut manifest = PackageManifest {
        schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
        conformance_level: None,
        kind: crate::core::context_package::manifest::PackageKind::default(),
        name: "signed-test".into(),
        version: "1.0.0".into(),
        description: "signature gate test".into(),
        author: None,
        scope: None,
        created_at: Utc::now(),
        updated_at: None,
        layers: vec![super::super::manifest::PackageLayer::Knowledge],
        dependencies: vec![],
        tags: vec![],
        visibility: None,
        integrity: super::super::manifest::PackageIntegrity {
            sha256: crate::core::agent_identity::hex_encode(&h2.finalize()),
            content_hash,
            byte_size: content_json.len() as u64,
        },
        provenance: super::super::manifest::PackageProvenance {
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
        redistributable: None,
    };

    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    super::super::signing::sign_package(&mut manifest, &content, &signing_key);

    // Valid signature imports fine.
    let bundle = ExportBundle {
        manifest: manifest.clone(),
        content: content.clone(),
    };
    let good = dir.path().join("good.ctxpkg");
    std::fs::write(&good, serde_json::to_string(&bundle).unwrap()).unwrap();
    let reg_good = LocalRegistry::open_at(&dir.path().join("good-reg")).unwrap();
    let (_, coverage) = reg_good
        .import_from_file_checked(&good, SignaturePolicy::RequireSigned)
        .unwrap();
    assert_eq!(
        coverage,
        Some(super::super::signing::SignatureCoverage::Manifest)
    );
    assert!(super::super::signing::signature_gap(coverage).is_none());

    // K02: an identity-only (v1) signature still installs locally and is
    // reported as identity-only.
    let mut identity_only = manifest.clone();
    identity_only.signature.as_mut().unwrap().manifest_value = None;
    let v1 = dir.path().join("v1.ctxpkg");
    let bundle = ExportBundle {
        manifest: identity_only,
        content: content.clone(),
    };
    std::fs::write(&v1, serde_json::to_string(&bundle).unwrap()).unwrap();
    let reg_v1 = LocalRegistry::open_at(&dir.path().join("v1-reg")).unwrap();
    let (_, coverage) = reg_v1
        .import_from_file_checked(&v1, SignaturePolicy::RequireSigned)
        .expect("local install");
    assert!(
        super::super::signing::signature_gap(coverage)
            .is_some_and(|gap| gap.starts_with("identity-only")),
        "{coverage:?}"
    );

    // Corrupted signature value must be rejected.
    let mut tampered = manifest.clone();
    if let Some(sig) = tampered.signature.as_mut() {
        sig.value = format!("0000{}", &sig.value[4..]);
    }
    let bundle = ExportBundle {
        manifest: tampered,
        content: content.clone(),
    };
    let bad = dir.path().join("bad.ctxpkg");
    std::fs::write(&bad, serde_json::to_string(&bundle).unwrap()).unwrap();
    let err = reg
        .import_from_file(&bad, SignaturePolicy::AllowUnsigned)
        .unwrap_err();
    assert!(
        err.contains("signature verification failed"),
        "unexpected error: {err}"
    );

    // K02: a dependency added after signing is refused at the local
    // import door, and nothing is stored.
    let mut redirected = manifest;
    redirected
        .dependencies
        .push(super::super::manifest::PackageDependency {
            name: "@evil/payload".into(),
            version_req: "^1".into(),
            optional: false,
        });
    let bundle = ExportBundle {
        manifest: redirected,
        content,
    };
    let deps = dir.path().join("deps.ctxpkg");
    std::fs::write(&deps, serde_json::to_string(&bundle).unwrap()).unwrap();
    let err = reg
        .import_from_file(&deps, SignaturePolicy::AllowUnsigned)
        .unwrap_err();
    assert!(
        err.contains("signature verification failed"),
        "unexpected error: {err}"
    );
    assert!(reg.list().unwrap().is_empty());
}

#[test]
fn legacy_lctxpkg_extension_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let reg = LocalRegistry::open_at(dir.path()).unwrap();

    let content = PackageContent::default();
    let content_json = serde_json::to_string(&content).unwrap();
    let mut h = Sha256::new();
    h.update(content_json.as_bytes());
    let content_hash = crate::core::agent_identity::hex_encode(&h.finalize());
    let composite = format!("legacy-test:1.0.0:{content_hash}");
    let mut h2 = Sha256::new();
    h2.update(composite.as_bytes());

    let manifest = PackageManifest {
        schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
        conformance_level: None,
        kind: crate::core::context_package::manifest::PackageKind::default(),
        name: "legacy-test".into(),
        version: "1.0.0".into(),
        description: "legacy extension test".into(),
        author: None,
        scope: None,
        created_at: Utc::now(),
        updated_at: None,
        layers: vec![super::super::manifest::PackageLayer::Knowledge],
        dependencies: vec![],
        tags: vec![],
        visibility: None,
        integrity: super::super::manifest::PackageIntegrity {
            sha256: crate::core::agent_identity::hex_encode(&h2.finalize()),
            content_hash,
            byte_size: content_json.len() as u64,
        },
        provenance: super::super::manifest::PackageProvenance {
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
        redistributable: None,
    };

    reg.install_local_build(&manifest, &content).unwrap();

    let legacy_path = dir.path().join("test.lctxpkg");
    reg.export_to_file("legacy-test", "1.0.0", &legacy_path)
        .unwrap();

    let reg2 = LocalRegistry::open_at(&dir.path().join("other")).unwrap();
    let imported = reg2
        .import_from_file(&legacy_path, SignaturePolicy::AllowUnsigned)
        .unwrap();
    assert_eq!(imported.name, "legacy-test");
}

#[test]
fn unsupported_extension_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let reg = LocalRegistry::open_at(dir.path()).unwrap();
    let bad_path = dir.path().join("test.json");
    std::fs::write(&bad_path, "{}").unwrap();
    assert!(
        reg.import_from_file(&bad_path, SignaturePolicy::AllowUnsigned)
            .is_err()
    );
}

/// O01 + V03 + K03: a private package is created, signed, exported and
/// verified with no account and every network route closed; imported
/// elsewhere it is applied to no project until it is assigned to one,
/// and then only to that one — a same-named project at another root and
/// the project it came from get nothing. An auto-load entry from before
/// the binding loads nowhere.
#[test]
fn a_private_package_travels_offline_and_loads_only_where_assigned() {
    use crate::core::knowledge::ProjectKnowledge;
    use crate::core::memory_policy::MemoryPolicy;
    use crate::test_env::VarGuard;

    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let closed = "http://127.0.0.1:9";
    let _net: Vec<VarGuard> = ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY", "CTXPKG_REGISTRY"]
        .into_iter()
        .map(|var| VarGuard::set(var, closed))
        .collect();
    assert!(
        std::env::var("CTXPKG_TOKEN").is_err(),
        "no registry account"
    );

    let projects = tempfile::tempdir().expect("projects");
    let root = |name: &str| {
        let dir = projects.path().join(name);
        std::fs::create_dir_all(&dir).expect("project");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"same\"\n").expect("id");
        dir.to_string_lossy().into_owned()
    };
    let (origin, assigned, namesake) = (root("p1"), root("p2"), root("p3"));
    ProjectKnowledge::mutate_locked(&origin, |k| {
        k.remember(
            "decision",
            "parser",
            "the parser is hand-written",
            "fixture",
            0.9,
            &MemoryPolicy::default(),
        );
    })
    .expect("origin fact");

    // Create → sign → export (private) → verify, offline.
    let (manifest, content) = crate::core::context_package::PackageBuilder::new("travel", "1.0.0")
        .private()
        .add_knowledge_from_project(&origin)
        .build()
        .expect("build");
    let sender = LocalRegistry::open_at(&projects.path().join("sender")).expect("sender");
    sender
        .install_local_build(&manifest, &content)
        .expect("create");
    let (key, _) = super::super::keys::load_or_create().expect("signing key");
    let file = projects.path().join("travel.ctxpkg");
    sender
        .export_to_file_signed("travel", "1.0.0", &file, &key, true)
        .expect("export");
    let report = super::super::verify::verify_package_file(&file).expect("verify");
    assert!(report.valid(), "{:?}", report.errors);
    assert_eq!(report.signature.as_str(), "pass");

    // Import: stored, applied nowhere.
    let receiver = LocalRegistry::open().expect("receiver");
    let imported = receiver
        .import_from_file(&file, SignaturePolicy::RequireSigned)
        .expect("import");
    assert_eq!(imported.visibility.as_deref(), Some("private"));
    for project in [&origin, &assigned, &namesake] {
        assert!(crate::core::context_package::auto_load_packages(project).is_empty());
    }

    // A deliberate assignment applies it to that project only.
    receiver
        .set_auto_load("travel", "1.0.0", true, &assigned)
        .expect("assign");
    assert_eq!(
        crate::core::context_package::auto_load_packages(&assigned),
        vec!["travel v1.0.0".to_string()]
    );
    assert!(crate::core::context_package::auto_load_packages(&namesake).is_empty());
    let fact = ProjectKnowledge::load(&assigned)
        .expect("assigned store")
        .facts
        .into_iter()
        .find(|f| f.key == "parser")
        .expect("merged fact");
    assert_eq!(fact.imported_from.as_deref(), Some("travel@1.0.0"));
    assert!(fact.confidence <= 0.8, "an import gains no confidence");
    assert!(ProjectKnowledge::load(&namesake).is_none_or(|k| k.facts.is_empty()));

    // An entry from before the binding loads nowhere.
    let mut index = receiver.load_index().expect("index");
    index.entries[0].auto_load_projects.clear();
    receiver.save_index(&index).expect("legacy index");
    assert!(receiver.list().expect("list")[0].auto_load_unbound());
    assert!(
        receiver
            .auto_load_packages(&assigned)
            .expect("entries")
            .is_empty()
    );
}

/// A signed `kind=context` package file named `name`, signed by `key`.
fn write_signed_package(
    dir: &std::path::Path,
    name: &str,
    key: &ed25519_dalek::SigningKey,
) -> std::path::PathBuf {
    let content = PackageContent::default();
    let content_json = serde_json::to_string(&content).unwrap();
    let content_hash = crate::core::agent_identity::hex_encode(&Sha256::digest(&content_json));
    let composite = format!("{name}:1.0.0:{content_hash}");
    let mut manifest = PackageManifest {
        schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
        conformance_level: None,
        kind: crate::core::context_package::manifest::PackageKind::default(),
        name: name.into(),
        version: "1.0.0".into(),
        description: "publisher pin test".into(),
        author: None,
        scope: None,
        created_at: Utc::now(),
        updated_at: None,
        layers: vec![super::super::manifest::PackageLayer::Knowledge],
        dependencies: vec![],
        tags: vec![],
        visibility: None,
        integrity: super::super::manifest::PackageIntegrity {
            sha256: crate::core::agent_identity::hex_encode(&Sha256::digest(&composite)),
            content_hash,
            byte_size: content_json.len() as u64,
        },
        provenance: super::super::manifest::PackageProvenance {
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
        redistributable: None,
    };
    super::super::signing::sign_package(&mut manifest, &content, key);
    let path = dir.join(format!(
        "{}-{}.ctxpkg",
        name.replace(['@', '/'], "_"),
        crate::core::agent_identity::hex_encode(&key.verifying_key().to_bytes()[..4])
    ));
    let bundle = ExportBundle { manifest, content };
    std::fs::write(&path, serde_json::to_string(&bundle).unwrap()).unwrap();
    path
}

/// GHSA-gvg8-285q-m62g: the TOFU publisher pin is recorded only after the
/// install succeeded; a failed install leaves no trust anchor behind.
#[test]
fn failed_install_leaves_no_publisher_pin() {
    let _lock = crate::core::data_dir::test_env_lock();
    let data = tempfile::tempdir().unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
    let dir = tempfile::tempdir().unwrap();
    let publisher = ed25519_dalek::SigningKey::from_bytes(&[14u8; 32]);
    let pk = crate::core::agent_identity::hex_encode(&publisher.verifying_key().to_bytes());
    let pinned = SignaturePolicy::RequirePinnedPublisher { namespace: "acme" };
    let file = write_signed_package(dir.path(), "@acme/broken", &publisher);

    // A regular file where the package directory must go fails the install.
    let reg = LocalRegistry::open_at(&dir.path().join("reg")).unwrap();
    std::fs::write(dir.path().join("reg").join("@acme"), b"not a dir").unwrap();
    assert!(reg.import_from_file(&file, pinned).is_err());
    assert!(
        !crate::core::signer_trust::is_trusted("@acme", &pk),
        "a failed install must not pin the publisher key"
    );

    // A successful install pins it.
    let reg2 = LocalRegistry::open_at(&dir.path().join("reg2")).unwrap();
    reg2.import_from_file(&file, pinned).unwrap();
    assert!(crate::core::signer_trust::is_trusted("@acme", &pk));

    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

/// GHSA-gvg8-285q-m62g: the MCP door admits only packages whose signer
/// key is anchored for the publisher; any self-made key is refused.
#[test]
fn mcp_imports_require_an_anchored_signer() {
    let _lock = crate::core::data_dir::test_env_lock();
    let data = tempfile::tempdir().unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
    let dir = tempfile::tempdir().unwrap();
    let publisher = ed25519_dalek::SigningKey::from_bytes(&[13u8; 32]);
    let reg = LocalRegistry::open_at(&dir.path().join("reg")).unwrap();
    let file = write_signed_package(dir.path(), "@acme/mcp", &publisher);

    let err = reg
        .import_from_file(&file, SignaturePolicy::RequireTrustedSigner)
        .unwrap_err();
    assert!(err.contains("not pinned"), "{err}");
    assert!(err.contains("mcp_import_allow_unverified"), "{err}");
    assert!(reg.list().unwrap().is_empty());
    assert!(
        reg.import_from_file(&file, SignaturePolicy::RequireSigned)
            .is_ok(),
        "the CLI door still accepts a valid signature"
    );

    let reg2 = LocalRegistry::open_at(&dir.path().join("reg2")).unwrap();
    let pk = crate::core::agent_identity::hex_encode(&publisher.verifying_key().to_bytes());
    crate::core::signer_trust::pin_if_absent("@acme", &pk, "test").unwrap();
    let manifest = reg2
        .import_from_file(&file, SignaturePolicy::RequireTrustedSigner)
        .unwrap();
    assert!(untrusted_package_signer(&manifest).is_none());

    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

/// GHSA-gvg8-285q-m62g: a remote install pins the namespace's publisher
/// key on first use and refuses a re-signed artifact or a foreign
/// namespace afterwards.
#[test]
fn remote_installs_pin_the_publisher_key() {
    let _lock = crate::core::data_dir::test_env_lock();
    let data = tempfile::tempdir().unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
    let dir = tempfile::tempdir().unwrap();
    let publisher = ed25519_dalek::SigningKey::from_bytes(&[11u8; 32]);
    let attacker = ed25519_dalek::SigningKey::from_bytes(&[12u8; 32]);
    let pinned = SignaturePolicy::RequirePinnedPublisher { namespace: "acme" };

    let reg = LocalRegistry::open_at(&dir.path().join("reg")).unwrap();
    let first = write_signed_package(dir.path(), "@acme/ctx", &publisher);
    reg.import_from_file(&first, pinned).unwrap();
    let manifest = reg.list().unwrap();
    assert_eq!(manifest.len(), 1);

    let resigned = write_signed_package(dir.path(), "@acme/ctx", &attacker);
    let err = reg.import_from_file(&resigned, pinned).unwrap_err();
    assert!(err.contains("publisher key mismatch"), "{err}");

    let foreign = write_signed_package(dir.path(), "@other/ctx", &publisher);
    let err = reg.import_from_file(&foreign, pinned).unwrap_err();
    assert!(err.contains("requested namespace"), "{err}");

    // The same publisher key keeps working.
    assert!(reg.import_from_file(&first, pinned).is_ok());

    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}
