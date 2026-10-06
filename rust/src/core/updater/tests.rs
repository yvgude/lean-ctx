// SPDX-License-Identifier: Apache-2.0
use std::fs::File;

use super::*;

#[test]
fn auto_update_disabled_skips_and_cleans_up() {
    // #335: auto_update=false → scheduled run must not install.
    assert_eq!(automatic_update_gate(false, false), AutoUpdateGate::Skip);
    // auto_update=false wins even if notify_only is also set.
    assert_eq!(automatic_update_gate(false, true), AutoUpdateGate::Skip);
}

#[test]
fn notify_only_downgrades_to_check() {
    assert_eq!(
        automatic_update_gate(true, true),
        AutoUpdateGate::NotifyOnly
    );
}

#[test]
fn auto_update_enabled_proceeds() {
    assert_eq!(automatic_update_gate(true, false), AutoUpdateGate::Proceed);
}

#[test]
fn bat_script_has_timeout_guard() {
    let script = generate_deferred_bat_script(
        r"C:\bin\lean-ctx.exe",
        r"C:\bin\lean-ctx-pending.exe",
        r"C:\bin\lean-ctx.old.exe",
        r"C:\state\update-transaction.json",
        r"C:\state\update.lock",
        60,
    );
    assert!(script.contains("set \"MAX_RETRIES=60\""));
    assert!(script.contains(":timeout"), "must have timeout label");
    assert!(
        script.contains("timed out after"),
        "must show timeout message"
    );
}

#[test]
fn bat_script_shows_blocking_processes() {
    let script = generate_deferred_bat_script("t", "p", "o", "tx", "lock", 30);
    assert!(script.contains("tasklist"), "must list blocking processes");
    assert!(
        script.contains("lean-ctx stop"),
        "must suggest lean-ctx stop"
    );
}

#[test]
fn bat_script_has_progress_indicators() {
    let script = generate_deferred_bat_script("t", "p", "o", "tx", "lock", 60);
    assert!(script.contains("Still waiting"));
    assert!(script.contains("RETRIES"));
}

#[test]
fn bat_script_provides_manual_recovery() {
    let script = generate_deferred_bat_script(
        r"C:\bin\lean-ctx.exe",
        r"C:\bin\lean-ctx-pending.exe",
        r"C:\bin\lean-ctx.old.exe",
        r"C:\state\update-transaction.json",
        r"C:\state\update.lock",
        60,
    );
    assert!(script.contains("Move-Item"));
    assert!(
        script.contains("lean-ctx-pending.exe"),
        "must show where the pending binary is"
    );
    assert!(
        script.contains("lean-ctx update"),
        "must suggest re-running update"
    );
}

#[test]
fn bat_script_no_infinite_loop() {
    let script = generate_deferred_bat_script("t", "p", "o", "tx", "lock", 10);
    assert!(script.contains("if %RETRIES% GEQ %MAX_RETRIES% goto timeout"));
    assert!(
        !script.contains(":retry\ntimeout"),
        "must not be an infinite loop"
    );
}

#[test]
fn bat_script_serializes_swap_and_recovery_under_updater_lock() {
    let script = generate_deferred_bat_script(
        r"C:\bin\lean-ctx.exe",
        r"C:\bin\lean-ctx-pending.exe",
        r"C:\bin\lean-ctx.old.exe",
        r"C:\state\update-transaction.json",
        r"C:\state\update.lock",
        60,
    );
    assert!(script.contains("LEANCTX_UPDATE_LOCK"));
    assert!(script.contains("[System.IO.FileShare]::None"));
    assert!(script.contains("--recover-update --lock-held"));
    assert!(
        !script.contains("move /Y"),
        "swap must stay inside the lock-held PowerShell command"
    );
}

#[test]
fn orphan_cleanup_removes_verified_state_without_touching_active() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = directory.path();
    std::fs::create_dir_all(state.join("updates/staged")).expect("staged directory");
    let active_path = state.join("active.bin");
    std::fs::write(&active_path, b"active").expect("active binary");

    for path in orphan_prepared_paths(state) {
        let is_backup = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains("-backup.bin"));
        let bytes: &[u8] = if is_backup { b"active" } else { b"prepared" };
        std::fs::write(path, bytes).expect("orphan state");
    }

    assert!(cleanup_orphaned_prepared_files(state, b"active").expect("cleanup"));
    assert_eq!(
        std::fs::read(&active_path).expect("active bytes"),
        b"active"
    );
    assert!(
        orphan_prepared_paths(state)
            .iter()
            .all(|path| !path.exists()),
        "cleanup must remove only prepared state"
    );
}

#[test]
fn orphan_cleanup_rejects_mismatched_backup_without_mutation() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = directory.path();
    std::fs::create_dir_all(state.join("updates/staged")).expect("staged directory");
    let active_path = state.join("active.bin");
    std::fs::write(&active_path, b"active").expect("active binary");
    let target = state.join("updates/staged/update-target.bin");
    let backup = state.join("updates/staged/update-backup.bin");
    std::fs::write(&target, b"prepared").expect("target");
    std::fs::write(&backup, b"wrong-active").expect("backup");

    assert!(cleanup_orphaned_prepared_files(state, b"active").is_err());
    assert_eq!(
        std::fs::read(&active_path).expect("active bytes"),
        b"active"
    );
    assert!(target.exists(), "mismatch must preserve target");
    assert!(backup.exists(), "mismatch must preserve backup");
}

#[cfg(unix)]
#[test]
fn updater_state_rejects_in_root_symlink_before_canonicalization() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path();
    let target = root.join("updates/previous");
    std::fs::create_dir_all(&target).expect("target directory");
    let alias = root.join("previous-alias");
    symlink(&target, &alias).expect("in-root symlink");

    let path = alias.join("lean-ctx.bin");
    let error = ensure_no_symlink_under(root, &path).expect_err("symlink must fail closed");
    assert!(error.contains("is a symlink"), "unexpected error: {error}");
}

#[test]
fn release_url_latest_when_no_version() {
    // #447: no pin → the canonical "latest" endpoint.
    assert_eq!(release_api_url(None), GITHUB_API_RELEASES);
}

#[test]
fn release_url_pins_specific_tag() {
    // #447: a bare version pins the `v`-prefixed tag …
    assert_eq!(
        release_api_url(Some("3.8.5")),
        "https://api.github.com/repos/yvgude/lean-ctx/releases/tags/v3.8.5"
    );
    // … and an already-`v`-prefixed version is normalised, not doubled.
    assert_eq!(
        release_api_url(Some("v3.8.5")),
        "https://api.github.com/repos/yvgude/lean-ctx/releases/tags/v3.8.5"
    );
}

#[test]
fn parse_target_version_peels_positional_only() {
    let flags_only = [String::from("--check"), String::from("--quiet")];
    assert_eq!(parse_target_version(&flags_only), None);

    let with_version = [String::from("3.8.5"), String::from("--check")];
    assert_eq!(parse_target_version(&with_version), Some("3.8.5"));

    // Order-independent: the positional is found after leading flags.
    let flag_then_version = [String::from("--insecure"), String::from("v3.8.5")];
    assert_eq!(parse_target_version(&flag_then_version), Some("v3.8.5"));
}

#[test]
fn looks_like_version_accepts_releases_rejects_typos() {
    assert!(looks_like_version("3.8.5"));
    assert!(looks_like_version("v3.8.5"));
    assert!(looks_like_version("3.8.5-rc1"));
    // Not versions: flags, words, and bare majors (too ambiguous to pin).
    assert!(!looks_like_version("--check"));
    assert!(!looks_like_version("latest"));
    assert!(!looks_like_version("3"));
}

#[test]
fn checksum_parser_rejects_duplicate_asset_entries() {
    let digest = "a".repeat(64);
    let sums = format!("{digest}  lean-ctx-linux.tar.gz\n{digest}  lean-ctx-linux.tar.gz\n");
    assert_eq!(parse_sha256sums(&sums, "lean-ctx-linux.tar.gz"), None);
}

#[test]
fn manifest_validation_is_fail_closed() {
    let asset = "lean-ctx-linux.tar.gz";
    let mut artifacts = std::collections::HashMap::new();
    artifacts.insert(
        asset.to_string(),
        ManifestArtifact {
            sha256: "a".repeat(64),
            size: 7,
            payload_sha256: Some("e".repeat(64)),
        },
    );
    let good = ReleaseManifest {
        schema_version: "leanctx.release-manifest/v1".to_string(),
        tag: "v3.9.20".to_string(),
        commit: "b".repeat(40),
        artifacts,
        sbom_sha256: "d".repeat(64),
        checksums_sha256: "c".repeat(64),
    };
    assert!(validate_manifest(&good, "v3.9.20", asset).is_ok());
    assert!(validate_manifest(&good, "v3.9.21", asset).is_err());
    assert!(validate_manifest(&good, "v3.9.20", "other.tar.gz").is_err());
    let mut missing_payload = good;
    missing_payload
        .artifacts
        .get_mut(asset)
        .expect("artifact")
        .payload_sha256 = None;
    assert!(validate_manifest(&missing_payload, "v3.9.20", asset).is_err());
}

#[test]
fn manifest_json_accepts_additive_inventory_without_weakening_payload_checks() {
    let asset = "lean-ctx-linux.tar.gz";
    let mut document = serde_json::json!({
        "schema_version": "leanctx.release-manifest/v1",
        "tag": "v4.0.0",
        "commit": "b".repeat(40),
        "checksums_sha256": "c".repeat(64),
        "sbom_sha256": "d".repeat(64),
        "artifacts": {
            (asset): {"sha256": "a".repeat(64), "size": 7,
                      "payload_sha256": "e".repeat(64)}
        }
    });
    let legacy: ReleaseManifest = serde_json::from_value(document.clone()).unwrap();
    assert!(validate_manifest(&legacy, "v4.0.0", asset).is_ok());

    document["artifacts"][asset]["kind"] = "binary".into();
    document["artifacts"]["SBOM.cdx.json"] = serde_json::json!({
        "kind": "supplemental", "sha256": "d".repeat(64), "size": 19,
        "source_path": "SBOM.cdx.json"
    });
    let additive: ReleaseManifest = serde_json::from_value(document.clone()).unwrap();
    assert!(validate_manifest(&additive, "v4.0.0", asset).is_ok());
    assert!(validate_manifest(&additive, "v4.0.0", "SBOM.cdx.json").is_err());

    // Metadata cannot turn a requested executable into a payload-exempt artifact.
    document["artifacts"][asset]["kind"] = "supplemental".into();
    document["artifacts"][asset]
        .as_object_mut()
        .unwrap()
        .remove("payload_sha256");
    let missing: ReleaseManifest = serde_json::from_value(document).unwrap();
    assert!(validate_manifest(&missing, "v4.0.0", asset).is_err());
}

#[test]
fn receipt_validation_rejects_tampered_digest() {
    let binary = BinaryReceipt {
        version: "3.9.20".to_string(),
        asset: "lean-ctx-linux.tar.gz".to_string(),
        sha256: "a".repeat(64),
        size: 1,
        path: "/tmp/lean-ctx".to_string(),
        manifest_sha256: Some("b".repeat(64)),
        archive_sha256: Some("c".repeat(64)),
        release_commit: None,
    };
    let mut receipt = UpdateReceipt {
        schema_version: UPDATE_RECEIPT_SCHEMA.to_string(),
        active: binary.clone(),
        previous: binary,
        receipt_sha256: None,
    };
    assert!(validate_receipt(&receipt).is_ok());
    receipt.active.sha256 = "tampered".to_string();
    assert!(validate_receipt(&receipt).is_err());
}

#[test]
fn atomic_receipt_write_roundtrips_and_leaves_no_temp_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("receipt.json");
    let binary = BinaryReceipt {
        version: "3.9.20".to_string(),
        asset: "lean-ctx-linux.tar.gz".to_string(),
        sha256: "a".repeat(64),
        size: 1,
        path: "/tmp/lean-ctx".to_string(),
        manifest_sha256: None,
        archive_sha256: None,
        release_commit: None,
    };
    let receipt = UpdateReceipt {
        schema_version: UPDATE_RECEIPT_SCHEMA.to_string(),
        active: binary.clone(),
        previous: binary,
        receipt_sha256: None,
    };
    write_update_receipt(&path, &receipt).expect("atomic receipt");
    let loaded: UpdateReceipt =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("receipt bytes"))
            .expect("receipt json");
    let mut expected = receipt;
    expected.receipt_sha256 = Some(receipt_digest(&expected).expect("receipt digest"));
    assert_eq!(loaded, expected);
    assert!(!path.with_file_name(".receipt.json.tmp").exists());
}

#[test]
fn receipt_integrity_digest_is_tamper_evident() {
    let binary = BinaryReceipt {
        version: "3.9.20".to_string(),
        asset: "lean-ctx-linux.tar.gz".to_string(),
        sha256: "a".repeat(64),
        size: 1,
        path: "/tmp/lean-ctx".to_string(),
        manifest_sha256: None,
        archive_sha256: None,
        release_commit: None,
    };
    let mut receipt = UpdateReceipt {
        schema_version: UPDATE_RECEIPT_SCHEMA.to_string(),
        active: binary.clone(),
        previous: binary,
        receipt_sha256: None,
    };
    receipt.receipt_sha256 = Some(receipt_digest(&receipt).expect("digest"));
    assert!(validate_receipt_integrity(&receipt).is_ok());
    receipt.previous.size = 2;
    assert!(validate_receipt_integrity(&receipt).is_err());
}

#[test]
fn cosign_identity_is_pinned_to_release_workflow_and_tag() {
    assert_eq!(
        cosign_identity_for_tag("v3.9.20"),
        r"^https://github\.com/yvgude/lean-ctx/\.github/workflows/release\.yml@refs/tags/v3\.9\.20$"
    );
    let identity = regex::Regex::new(&cosign_identity_for_tag("v3.9.20")).unwrap();
    assert!(identity.is_match(
        "https://github.com/yvgude/lean-ctx/.github/workflows/release.yml@refs/tags/v3.9.20"
    ));
    assert!(!identity.is_match(
        "https://github.com/yvgude/lean-ctx/.github/workflows/release.yml@refs/tags/v3x9x20"
    ));
}

#[test]
fn manifest_rejects_malformed_payload_digest() {
    let asset = "lean-ctx-linux.tar.gz";
    let mut artifacts = std::collections::HashMap::new();
    artifacts.insert(
        asset.to_string(),
        ManifestArtifact {
            sha256: "a".repeat(64),
            size: 7,
            payload_sha256: Some("bad".to_string()),
        },
    );
    let manifest = ReleaseManifest {
        schema_version: "leanctx.release-manifest/v1".to_string(),
        tag: "v3.9.20".to_string(),
        commit: "b".repeat(40),
        artifacts,
        sbom_sha256: "d".repeat(64),
        checksums_sha256: "c".repeat(64),
    };
    assert!(validate_manifest(&manifest, "v3.9.20", asset).is_err());
}

#[test]
fn filesystem_lock_rejects_concurrent_writer() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("update.lock");
    let first = File::create(&path).expect("lock file");
    first.try_lock_exclusive().expect("first lock");
    let second = File::open(&path).expect("second handle");
    assert!(second.try_lock_exclusive().is_err());
    first.unlock().expect("unlock");
    second.try_lock_exclusive().expect("lock after release");
}
