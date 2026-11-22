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

#[cfg(feature = "secure-update")]
#[test]
fn checksum_parser_rejects_duplicate_asset_entries() {
    let digest = "a".repeat(64);
    let sums = format!("{digest}  lean-ctx-linux.tar.gz\n{digest}  lean-ctx-linux.tar.gz\n");
    assert_eq!(parse_sha256sums(&sums, "lean-ctx-linux.tar.gz"), None);
}

#[cfg(feature = "secure-update")]
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

#[cfg(feature = "secure-update")]
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

#[cfg(feature = "secure-update")]
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

/// Regression (3.11.0/3.11.1): `prepare_update_transaction` returned the
/// unsealed transaction, so `execute_prepared_transaction` always failed with
/// "prepared transaction is missing its integrity digest" and `lean-ctx update`
/// could never install a release. Runs the real prepare → execute path on a
/// stand-in binary inside an isolated state directory.
#[test]
fn prepared_update_executes_and_records_the_receipt() {
    let sandbox = UpdaterSandbox::new(b"#!/bin/sh\necho old\n");
    let exe = sandbox.exe.clone();
    let target = b"#!/bin/sh\necho new\n";

    let transaction = sandbox.prepare("9.9.9", target);
    assert!(
        transaction.transaction_sha256.is_some(),
        "the caller must receive the sealed transaction"
    );
    execute_prepared_transaction(&transaction, &exe).expect("execute");

    assert_eq!(std::fs::read(&exe).expect("installed binary"), target);
    let receipt = load_update_receipt_for(&exe)
        .expect("receipt loads")
        .expect("receipt written");
    assert_eq!(receipt.active.version, "9.9.9");
    assert_eq!(receipt.active.sha256, sha256_hex(target));
    assert!(
        !update_transaction_path()
            .expect("transaction path")
            .exists(),
        "a committed transaction is cleaned up"
    );
    // A second run finds nothing pending.
    assert!(!recover_pending_transaction(&exe).expect("recovery"));

    // `lean-ctx update --rollback` restores the previous binary through the
    // same sealed-transaction path and records the swapped receipt.
    rollback_installation(&exe).expect("rollback");
    assert_eq!(
        std::fs::read(&exe).expect("restored binary"),
        b"#!/bin/sh\necho old\n"
    );
    let receipt = load_update_receipt_for(&exe)
        .expect("receipt loads")
        .expect("receipt written");
    assert_eq!(receipt.previous.version, "9.9.9");
    assert!(!recover_pending_transaction(&exe).expect("recovery after rollback"));
}

/// When signing the staged binary stops because a signing tool could not be
/// confirmed ended, that tool may still be writing the file: the update is
/// refused before the file is read, and nothing is sealed, recorded or swapped.
#[test]
#[cfg(target_os = "macos")]
fn an_update_is_neither_sealed_nor_swapped_when_signing_could_not_be_confirmed_over() {
    let sandbox = UpdaterSandbox::new(b"#!/bin/sh\necho old\n");
    let exe = sandbox.exe.clone();
    let tools = tempfile::tempdir().expect("signing tools dir");
    let _stopped = crate::core::codesign::test_scope::unconfirmable(tools.path());
    let target = b"#!/bin/sh\necho new\n";
    let verified = VerifiedArtifact {
        archive_sha256: "a".repeat(64),
        manifest_sha256: "b".repeat(64),
        release_commit: "c".repeat(40),
        payload_sha256: sha256_hex(target),
    };

    let refused = prepare_update_transaction(
        &exe,
        "lean-ctx-aarch64-apple-darwin.tar.gz",
        "9.9.9",
        &verified,
        target,
    );
    assert!(
        refused
            .err()
            .is_some_and(|error| error.contains("code signing stopped")),
        "the update was prepared although signing was not confirmed over"
    );
    assert_eq!(
        std::fs::read(&exe).expect("installed binary"),
        b"#!/bin/sh\necho old\n"
    );
    assert!(
        load_update_receipt_for(&exe)
            .expect("receipt loads")
            .is_none(),
        "a receipt was written"
    );
    assert!(
        !update_transaction_path()
            .expect("transaction path")
            .exists(),
        "a transaction was sealed"
    );
    assert!(!recover_pending_transaction(&exe).expect("recovery"));
}

/// Stand-in install for transaction tests: isolated state dir plus a binary.
struct UpdaterSandbox {
    /// Signing during an update uses stand-in tools of this sandbox: these
    /// tests show the transaction around the signing step, never the user's
    /// keychain or a real signature. Installed on the thread that prepares
    /// the update, before it does, and put back when the sandbox goes.
    #[cfg(target_os = "macos")]
    _signing: (crate::core::codesign::test_scope::Scoped, tempfile::TempDir),
    _env_lock: crate::core::data_dir::TestEnvGuard,
    _state: tempfile::TempDir,
    _install: tempfile::TempDir,
    exe: std::path::PathBuf,
}

impl UpdaterSandbox {
    fn new(binary: &[u8]) -> Self {
        let env_lock = crate::core::data_dir::test_env_lock();
        let state = tempfile::tempdir().expect("state dir");
        crate::test_env::set_var("LEAN_CTX_STATE_DIR", state.path());
        let install = tempfile::tempdir().expect("install dir");
        let exe = install.path().join("lean-ctx");
        std::fs::write(&exe, binary).expect("current binary");
        #[cfg(target_os = "macos")]
        let signing = {
            let tools = tempfile::tempdir().expect("signing tools dir");
            (
                crate::core::codesign::test_scope::without_identity(tools.path()),
                tools,
            )
        };
        Self {
            #[cfg(target_os = "macos")]
            _signing: signing,
            _env_lock: env_lock,
            _state: state,
            _install: install,
            exe,
        }
    }

    fn prepare(&self, version: &str, target: &[u8]) -> PreparedTransaction {
        let verified = VerifiedArtifact {
            archive_sha256: "a".repeat(64),
            manifest_sha256: "b".repeat(64),
            release_commit: "c".repeat(40),
            payload_sha256: sha256_hex(target),
        };
        prepare_update_transaction(
            &self.exe,
            "lean-ctx-aarch64-apple-darwin.tar.gz",
            version,
            &verified,
            target,
        )
        .expect("prepare")
    }
}

impl Drop for UpdaterSandbox {
    fn drop(&mut self) {
        crate::test_env::remove_var("LEAN_CTX_STATE_DIR");
    }
}

/// Users stranded on 3.11.0/3.11.1 keep a sealed transaction on disk from a
/// failed run. After they reinstall a newer build by script or package
/// manager, recovery must drop that obsolete transaction instead of refusing
/// every later update with "matches neither prepared state".
#[test]
fn recovery_drops_a_transaction_an_external_install_superseded() {
    let sandbox = UpdaterSandbox::new(b"#!/bin/sh\necho stranded\n");
    let transaction = sandbox.prepare(CURRENT_VERSION, b"#!/bin/sh\necho target\n");
    std::fs::write(&sandbox.exe, b"#!/bin/sh\necho reinstalled\n").expect("reinstall");

    assert!(!recover_pending_transaction(&sandbox.exe).expect("recovery"));
    assert_eq!(
        std::fs::read(&sandbox.exe).expect("binary"),
        b"#!/bin/sh\necho reinstalled\n",
        "recovery never touches the reinstalled binary"
    );
    for path in [
        update_transaction_path().expect("transaction path"),
        std::path::PathBuf::from(&transaction.staged_path),
        std::path::PathBuf::from(&transaction.backup_path),
    ] {
        assert!(!path.exists(), "{} removed", path.display());
    }
}

/// A transaction toward a version newer than the running build is not
/// superseded: an unknown binary there still refuses recovery.
#[test]
fn recovery_still_refuses_an_unknown_binary_below_the_target() {
    let sandbox = UpdaterSandbox::new(b"#!/bin/sh\necho old\n");
    sandbox.prepare("999.0.0", b"#!/bin/sh\necho future\n");
    std::fs::write(&sandbox.exe, b"#!/bin/sh\necho tampered\n").expect("swap");
    let error = recover_pending_transaction(&sandbox.exe).expect_err("refuses");
    assert!(error.contains("matches neither prepared state"), "{error}");
}

fn receipt_for(
    sandbox: &UpdaterSandbox,
    version: &str,
    bytes: &[u8],
) -> (std::path::PathBuf, UpdateReceipt) {
    let (_state, receipt_path, previous) = canonical_update_paths(&sandbox.exe).expect("paths");
    let binary = |path: String| BinaryReceipt {
        version: version.to_string(),
        asset: "lean-ctx".to_string(),
        sha256: sha256_hex(bytes),
        size: bytes.len() as u64,
        path,
        manifest_sha256: None,
        archive_sha256: None,
        release_commit: None,
    };
    let current = canonical_current_exe(&sandbox.exe).expect("canonical exe");
    let receipt = UpdateReceipt {
        schema_version: UPDATE_RECEIPT_SCHEMA.to_string(),
        active: binary(current.to_string_lossy().into_owned()),
        previous: binary(previous.to_string_lossy().into_owned()),
        receipt_sha256: None,
    };
    (receipt_path, receipt)
}

/// A receipt from an earlier updater run that a reinstall of another version
/// made stale no longer blocks updates; the chain restarts from the running
/// binary.
#[test]
fn update_restarts_the_receipt_chain_after_an_external_reinstall() {
    let sandbox = UpdaterSandbox::new(b"#!/bin/sh\necho reinstalled\n");
    let (path, receipt) = receipt_for(&sandbox, "3.0.0", b"#!/bin/sh\necho earlier\n");
    write_update_receipt(&path, &receipt).expect("stale receipt");

    let target = b"#!/bin/sh\necho next\n";
    let transaction = sandbox.prepare("999.0.0", target);
    assert_eq!(
        transaction.old_active.sha256,
        sha256_hex(b"#!/bin/sh\necho reinstalled\n")
    );
    execute_prepared_transaction(&transaction, &sandbox.exe).expect("execute");
    assert_eq!(std::fs::read(&sandbox.exe).expect("installed"), target);
}

/// Same version, different bytes is not a reinstall but an unexplained
/// change: the updater keeps refusing.
#[test]
fn update_refuses_a_same_version_binary_that_differs_from_its_receipt() {
    let sandbox = UpdaterSandbox::new(b"#!/bin/sh\necho modified\n");
    let (path, receipt) = receipt_for(&sandbox, CURRENT_VERSION, b"#!/bin/sh\necho recorded\n");
    write_update_receipt(&path, &receipt).expect("receipt");
    let verified = VerifiedArtifact {
        archive_sha256: "a".repeat(64),
        manifest_sha256: "b".repeat(64),
        release_commit: "c".repeat(40),
        payload_sha256: sha256_hex(b"x"),
    };
    let error = prepare_update_transaction(&sandbox.exe, "lean-ctx", "999.0.0", &verified, b"x")
        .expect_err("refuses");
    assert!(error.contains("differs from the active receipt"), "{error}");
}

/// Only a newer release is an update unless a version was chosen on purpose:
/// a build ahead of GitHub's "latest" is never downgraded on a schedule.
#[test]
fn only_newer_releases_are_offered_without_a_pin() {
    assert!(offers_update("3.11.2", "3.11.1", false));
    assert!(offers_update("v3.12.0", "3.11.2", false));
    assert!(!offers_update("3.11.1", "3.11.2", false));
    assert!(!offers_update("3.11.2", "3.11.2", false));
    assert!(
        offers_update("3.11.0", "3.11.2", true),
        "explicit downgrade"
    );
    assert!(!offers_update("v3.11.2", "3.11.2", true));
}

/// #2037: an exhausted GitHub quota names the cause, the reset time and the
/// fix instead of a bare "http status: 403".
#[test]
fn rate_limit_message_names_reset_and_token() {
    let now = 1_791_550_000;
    let anonymous = rate_limit_message(Some(now + 600), false, now);
    assert!(anonymous.contains("60 requests per hour"), "{anonymous}");
    assert!(anonymous.contains("in 10 min"), "{anonymous}");
    assert!(
        anonymous.contains("GITHUB_TOKEN or GH_TOKEN"),
        "{anonymous}"
    );
    let with_token = rate_limit_message(None, true, now);
    assert!(with_token.contains("for your token"), "{with_token}");
    assert!(!with_token.contains("resets"), "{with_token}");
}

/// #2037: the updater reads GITHUB_TOKEN, then GH_TOKEN, then
/// LEAN_CTX_GITHUB_TOKEN, ignoring empty values.
#[test]
fn github_token_prefers_github_token_and_skips_empty_values() {
    let _env_lock = crate::core::data_dir::test_env_lock();
    let saved: Vec<_> = ["GITHUB_TOKEN", "GH_TOKEN", "LEAN_CTX_GITHUB_TOKEN"]
        .iter()
        .map(|name| (*name, std::env::var_os(name)))
        .collect();
    crate::test_env::set_var("GITHUB_TOKEN", "  ");
    crate::test_env::set_var("GH_TOKEN", "gh-token");
    crate::test_env::set_var("LEAN_CTX_GITHUB_TOKEN", "lean-token");
    assert_eq!(github_token().as_deref(), Some("gh-token"));
    crate::test_env::set_var("GITHUB_TOKEN", "github-token");
    assert_eq!(github_token().as_deref(), Some("github-token"));
    for (name, value) in saved {
        match value {
            Some(value) => crate::test_env::set_var(name, value),
            None => crate::test_env::remove_var(name),
        }
    }
}

/// The token is only ever sent to api.github.com.
#[test]
fn github_api_json_refuses_other_hosts() {
    let error = github_api_json("https://example.com/repos/yvgude/lean-ctx/releases/latest")
        .expect_err("refused");
    assert!(error.contains("refusing non-GitHub API URL"), "{error}");
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

/// A daemon double for the post-update restart on a fake clock: records
/// restarts, pauses and probe timeouts, and answers with the given versions in
/// turn (the last one repeats). A probe takes `probe_cost`; when that exceeds
/// the probe's timeout, the probe gives up at the timeout without an answer.
struct FakeDaemon {
    supervised: bool,
    restart_result: Result<(), String>,
    answers: Vec<Option<&'static str>>,
    probe_cost: std::time::Duration,
    epoch: std::time::Instant,
    elapsed: std::time::Duration,
    restarts: usize,
    checks: usize,
    probe_timeouts: Vec<std::time::Duration>,
    pauses: Vec<std::time::Duration>,
}

impl FakeDaemon {
    fn answering(answers: Vec<Option<&'static str>>) -> Self {
        Self {
            supervised: false,
            restart_result: Ok(()),
            answers,
            probe_cost: std::time::Duration::ZERO,
            epoch: std::time::Instant::now(),
            elapsed: std::time::Duration::ZERO,
            restarts: 0,
            checks: 0,
            probe_timeouts: Vec::new(),
            pauses: Vec::new(),
        }
    }
}

impl rewire::DaemonControl for FakeDaemon {
    fn supervised(&self) -> bool {
        self.supervised
    }

    fn restart(&mut self) -> Result<(), String> {
        self.restarts += 1;
        self.restart_result.clone()
    }

    fn reported_version(&mut self, timeout: std::time::Duration) -> Option<String> {
        self.probe_timeouts.push(timeout);
        let index = self.checks.min(self.answers.len().saturating_sub(1));
        self.checks += 1;
        if self.probe_cost > timeout {
            self.elapsed += timeout;
            return None;
        }
        self.elapsed += self.probe_cost;
        self.answers
            .get(index)
            .copied()
            .flatten()
            .map(str::to_string)
    }

    fn now(&self) -> std::time::Instant {
        self.epoch + self.elapsed
    }

    fn pause(&mut self, duration: std::time::Duration) {
        self.elapsed += duration;
        self.pauses.push(duration);
    }
}

/// A daemon that ran before an attended update whose new binary is in place.
fn ran_before() -> rewire::RestartContext {
    rewire::RestartContext {
        was_running: true,
        swap_landed: true,
        ..rewire::RestartContext::default()
    }
}

/// A daemon that ran before the update is restarted once and confirmed on
/// the new version, also when it needs a few checks to come back.
#[test]
fn update_restarts_a_running_daemon_and_verifies_the_new_version() {
    let mut daemon = FakeDaemon::answering(vec![None, None, Some("v3.12.7")]);
    let outcome = rewire::restart_and_verify(
        &mut daemon,
        ran_before(),
        "3.12.7",
        std::time::Duration::from_secs(10),
    );
    assert_eq!(
        outcome,
        rewire::DaemonRestart::Verified("3.12.7".to_string())
    );
    assert_eq!(daemon.restarts, 1);
    assert_eq!(daemon.checks, 3);
    assert_eq!(daemon.pauses, vec![rewire::DAEMON_POLL; 2]);
    let (warning, lines) = rewire::daemon_restart_report(&outcome).expect("a line");
    assert!(!warning);
    assert!(lines[0].contains("Daemon running on the new binary (v3.12.7)"));
}

/// A daemon that did not run before the update stays stopped.
#[test]
fn update_leaves_a_stopped_daemon_stopped() {
    let mut daemon = FakeDaemon::answering(vec![Some("3.12.7")]);
    let outcome = rewire::restart_and_verify(
        &mut daemon,
        rewire::RestartContext {
            was_running: false,
            ..ran_before()
        },
        "3.12.7",
        std::time::Duration::from_secs(10),
    );
    assert_eq!(outcome, rewire::DaemonRestart::NotRunningBefore);
    assert_eq!(daemon.restarts, 0);
    assert_eq!(daemon.checks, 0);
    assert!(rewire::daemon_restart_report(&outcome).is_none());
}

/// A daemon that still reports the old version after the bounded wait is
/// reported as a warning naming both versions; the rollback stays available.
#[test]
fn update_reports_a_daemon_on_the_wrong_version_after_restart() {
    let mut daemon = FakeDaemon::answering(vec![Some("3.12.6")]);
    let outcome = rewire::restart_and_verify(
        &mut daemon,
        ran_before(),
        "v3.12.7",
        std::time::Duration::from_secs(2),
    );
    assert_eq!(
        outcome,
        rewire::DaemonRestart::VersionMismatch {
            expected: "3.12.7".to_string(),
            reported: Some("3.12.6".to_string()),
            waited_secs: 2,
        }
    );
    assert_eq!(daemon.restarts, 1);
    // Bounded: one check right after the restart, then one per poll interval.
    assert_eq!(daemon.pauses.len(), 4);
    assert_eq!(daemon.checks, 5);
    let (warning, lines) = rewire::daemon_restart_report(&outcome).expect("a warning");
    assert!(warning);
    let text = lines.join("\n");
    assert!(text.contains("reports v3.12.6, not v3.12.7"), "{text}");
    assert!(text.contains("lean-ctx daemon status"), "{text}");
    assert!(text.contains("lean-ctx update --rollback"), "{text}");
}

/// A daemon that never answers after the restart is reported with the wait.
#[test]
fn update_reports_a_daemon_that_does_not_come_back() {
    let mut daemon = FakeDaemon::answering(vec![None]);
    let outcome = rewire::restart_and_verify(
        &mut daemon,
        ran_before(),
        "3.12.7",
        std::time::Duration::from_secs(1),
    );
    assert_eq!(
        outcome,
        rewire::DaemonRestart::VersionMismatch {
            expected: "3.12.7".to_string(),
            reported: None,
            waited_secs: 1,
        }
    );
    let (warning, lines) = rewire::daemon_restart_report(&outcome).expect("a warning");
    assert!(warning);
    assert!(lines[0].contains("did not come back on v3.12.7 within 1s"));
}

/// A deferred swap (Windows, locked binary) and a failed restart never poll.
#[test]
fn update_skips_the_wait_when_the_swap_is_deferred_or_the_restart_fails() {
    let mut daemon = FakeDaemon::answering(vec![Some("3.12.7")]);
    let wait = std::time::Duration::from_secs(10);
    let deferred = rewire::RestartContext {
        swap_landed: false,
        ..ran_before()
    };
    let outcome = rewire::restart_and_verify(&mut daemon, deferred, "3.12.7", wait);
    assert_eq!(outcome, rewire::DaemonRestart::SwapDeferred);
    assert_eq!((daemon.restarts, daemon.checks), (0, 0));
    assert!(
        rewire::daemon_restart_report(&outcome)
            .expect("a warning")
            .0
    );

    let mut daemon = FakeDaemon::answering(vec![Some("3.12.7")]);
    daemon.restart_result = Err("start: spawn failed".to_string());
    let outcome = rewire::restart_and_verify(&mut daemon, ran_before(), "3.12.7", wait);
    assert_eq!(
        outcome,
        rewire::DaemonRestart::RestartFailed("start: spawn failed".to_string())
    );
    assert_eq!((daemon.restarts, daemon.checks), (1, 0));
    let (warning, lines) = rewire::daemon_restart_report(&outcome).expect("a warning");
    assert!(warning);
    assert!(lines[0].contains("start: spawn failed"));
}

/// A daemon that accepts the connection but answers slower than the time
/// left (index still loading) never stretches the wait: every probe is cut
/// to what remains before the deadline, so the whole wait ends by it.
#[test]
fn update_wait_ends_by_the_deadline_when_the_daemon_answers_slowly() {
    use std::time::Duration;
    let wait = Duration::from_secs(10);
    let mut daemon = FakeDaemon::answering(vec![Some("3.12.7")]);
    daemon.probe_cost = Duration::from_secs(3);
    let outcome = rewire::restart_and_verify(&mut daemon, ran_before(), "3.12.7", wait);
    assert_eq!(
        outcome,
        rewire::DaemonRestart::VersionMismatch {
            expected: "3.12.7".to_string(),
            reported: None,
            waited_secs: 10,
        }
    );
    assert!(
        daemon.elapsed <= wait + rewire::DAEMON_POLL,
        "waited {:?} for a {wait:?} budget",
        daemon.elapsed
    );
    assert!(
        daemon
            .probe_timeouts
            .iter()
            .all(|timeout| *timeout <= rewire::DAEMON_PROBE_TIMEOUT),
        "{:?}",
        daemon.probe_timeouts
    );

    // A wait of 0 s still asks once, within one poll interval.
    let mut daemon = FakeDaemon::answering(vec![Some("3.12.7")]);
    daemon.probe_cost = Duration::from_secs(3);
    rewire::restart_and_verify(&mut daemon, ran_before(), "3.12.7", Duration::ZERO);
    assert_eq!(daemon.checks, 1);
    assert!(
        daemon.elapsed <= rewire::DAEMON_POLL,
        "{:?}",
        daemon.elapsed
    );
}

/// macOS seatbelt guard: an unattended update (scheduled, or launchd-
/// standalone) without an autostart unit never stops or starts the daemon —
/// a directly started daemon would inherit the deny-~/Documents sandbox —
/// and tells the user to restart it. With a unit it restarts through it.
#[test]
fn unattended_update_without_a_unit_leaves_the_restart_to_the_user() {
    let unattended = rewire::RestartContext {
        unattended: true,
        ..ran_before()
    };
    let wait = std::time::Duration::from_secs(10);
    let mut daemon = FakeDaemon::answering(vec![Some("3.12.7")]);
    let outcome = rewire::restart_and_verify(&mut daemon, unattended, "3.12.7", wait);
    assert_eq!(outcome, rewire::DaemonRestart::ManualRestartNeeded);
    assert_eq!((daemon.restarts, daemon.checks), (0, 0));
    let (warning, lines) = rewire::daemon_restart_report(&outcome).expect("a warning");
    assert!(warning);
    assert!(
        lines.join("\n").contains("run: lean-ctx daemon restart"),
        "{lines:?}"
    );

    let mut daemon = FakeDaemon::answering(vec![Some("3.12.7")]);
    daemon.supervised = true;
    let outcome = rewire::restart_and_verify(&mut daemon, unattended, "3.12.7", wait);
    assert_eq!(
        outcome,
        rewire::DaemonRestart::Verified("3.12.7".to_string())
    );
    assert_eq!(daemon.restarts, 1);
}

/// When the setup refresh already restarted the daemon through its unit
/// (macOS LaunchAgent re-wrap), it is not restarted a second time; the
/// version is still verified.
#[test]
fn update_does_not_restart_a_daemon_the_refresh_already_restarted() {
    let mut daemon = FakeDaemon::answering(vec![None, Some("3.12.7")]);
    daemon.supervised = true;
    let context = rewire::RestartContext {
        already_restarted: true,
        unattended: true,
        ..ran_before()
    };
    let outcome = rewire::restart_and_verify(
        &mut daemon,
        context,
        "3.12.7",
        std::time::Duration::from_secs(10),
    );
    assert_eq!(
        outcome,
        rewire::DaemonRestart::Verified("3.12.7".to_string())
    );
    assert_eq!((daemon.restarts, daemon.checks), (0, 2));
}

/// Linux legacy schedulers (timer units and cron entries written before
/// #335) run `update --quiet` without `--scheduled`; such a run is
/// unattended too, or a hand-started daemon would be spawned inside the
/// oneshot timer unit and killed with its cgroup. Windows has no unit.
#[test]
fn quiet_update_from_a_legacy_scheduler_counts_as_unattended() {
    assert!(rewire::unattended_run("linux", false, true, false));
    assert!(rewire::unattended_run("linux", true, false, false));
    assert!(rewire::unattended_run("macos", false, false, true));
    assert!(rewire::unattended_run("macos", false, true, false));
    assert!(!rewire::unattended_run("linux", false, false, false));
    assert!(!rewire::unattended_run("windows", true, true, false));

    let context = rewire::RestartContext {
        unattended: rewire::unattended_run("linux", false, true, false),
        ..ran_before()
    };
    let mut daemon = FakeDaemon::answering(vec![Some("3.12.7")]);
    let outcome = rewire::restart_and_verify(
        &mut daemon,
        context,
        "3.12.7",
        std::time::Duration::from_secs(10),
    );
    assert_eq!(outcome, rewire::DaemonRestart::ManualRestartNeeded);
    assert_eq!(daemon.restarts, 0);
    assert!(!outcome.waited(), "no wait-value warning without a wait");
}

/// macOS: the LaunchAgent re-wrap restarts only the daemon launchd runs. A
/// daemon started by hand next to an installed plist (another PID, or no
/// launchd job) is still stopped and restarted explicitly.
#[test]
fn a_hand_started_daemon_next_to_the_launch_agent_is_restarted() {
    assert!(rewire::restarted_by_rewrap(
        "macos",
        true,
        Some(42),
        Some(42)
    ));
    assert!(!rewire::restarted_by_rewrap(
        "macos",
        true,
        Some(42),
        Some(7)
    ));
    assert!(!rewire::restarted_by_rewrap("macos", true, Some(42), None));
    assert!(!rewire::restarted_by_rewrap("macos", true, None, None));
    assert!(!rewire::restarted_by_rewrap(
        "macos",
        false,
        Some(42),
        Some(42)
    ));
    assert!(!rewire::restarted_by_rewrap(
        "linux",
        true,
        Some(42),
        Some(42)
    ));

    let mut daemon = FakeDaemon::answering(vec![Some("3.12.7")]);
    daemon.supervised = true;
    let context = rewire::RestartContext {
        already_restarted: rewire::restarted_by_rewrap("macos", true, Some(42), Some(7)),
        ..ran_before()
    };
    let outcome = rewire::restart_and_verify(
        &mut daemon,
        context,
        "3.12.7",
        std::time::Duration::from_secs(10),
    );
    assert_eq!(
        outcome,
        rewire::DaemonRestart::Verified("3.12.7".to_string())
    );
    assert_eq!(daemon.restarts, 1);
    assert!(outcome.waited());
}

/// `LEAN_CTX_DAEMON_WAIT_SECS` as the installers read it: whole seconds,
/// default 10, bounded; an invalid or capped value comes with a warning.
#[test]
fn daemon_wait_follows_the_installer_variable() {
    use std::time::Duration;
    let secs = |raw: Option<&str>| rewire::daemon_wait(raw).0;
    let warns = |raw: Option<&str>| rewire::daemon_wait(raw).1.is_some();
    assert_eq!(secs(None), Duration::from_secs(10));
    assert_eq!(secs(Some("")), Duration::from_secs(10));
    assert_eq!(secs(Some("abc")), Duration::from_secs(10));
    assert_eq!(secs(Some("-3")), Duration::from_secs(10));
    assert_eq!(secs(Some("1.5")), Duration::from_secs(10));
    assert_eq!(secs(Some("08")), Duration::from_secs(8));
    assert_eq!(secs(Some("0")), Duration::from_secs(0));
    assert_eq!(secs(Some("300")), Duration::from_secs(300));
    assert_eq!(secs(Some("301")), Duration::from_secs(300));
    assert_eq!(
        secs(Some("99999999999999999999999")),
        Duration::from_secs(300)
    );
    assert!(!warns(None) && !warns(Some(" ")) && !warns(Some("08")) && !warns(Some("300")));
    assert!(warns(Some("abc")) && warns(Some("1.5")) && warns(Some("-3")));
    assert!(warns(Some("301")) && warns(Some("99999999999999999999999")));
    let text = rewire::daemon_wait(Some("1.5")).1.unwrap_or_default();
    assert!(text.contains("LEAN_CTX_DAEMON_WAIT_SECS=\"1.5\""), "{text}");
}

/// The daemon's version comes from its capabilities document.
#[test]
fn daemon_version_is_read_from_the_capabilities_document() {
    assert_eq!(
        rewire::version_from_capabilities(
            r#"{"contract":"local-read/1","kind":"capabilities","version":"3.12.7"}"#
        )
        .as_deref(),
        Some("3.12.7")
    );
    assert_eq!(
        rewire::version_from_capabilities(r#"{"kind":"error"}"#),
        None
    );
    assert_eq!(rewire::version_from_capabilities("ok\n"), None);
}
