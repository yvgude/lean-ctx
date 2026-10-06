// SPDX-License-Identifier: Apache-2.0

use lean_ctx_protocol::ExecutionReceiptV1;

const V3_RECEIPT: &[u8] = include_bytes!("fixtures/v3/execution-receipt-v1.json");

#[test]
fn v3_task_cli_migration_is_idempotent_and_rollback_preserves_source_bytes() {
    let sandbox = tempfile::tempdir().expect("isolated migration workspace");
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../_archive/benchmarks/efficiency/task-spine-v1/tasks/task-001-python-bug");
    let task = sandbox.path().join("task");
    std::fs::create_dir(&task).unwrap();
    let names = [
        "baseline.json",
        "context_plan.json",
        "execution_receipt.json",
        "outcome.json",
        "task_envelope.json",
    ];
    let originals: Vec<_> = names
        .iter()
        .map(|name| {
            let bytes = std::fs::read(source.join(name)).unwrap();
            std::fs::write(task.join(name), &bytes).unwrap();
            bytes
        })
        .collect();
    let run = |extra: &[&str]| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
            .args(["migrate", "task-receipt"])
            .arg(&task)
            .args(extra)
            .current_dir(sandbox.path())
            .env("LEAN_CTX_DATA_DIR", sandbox.path().join("data"))
            .env("LEAN_CTX_CONFIG_DIR", sandbox.path().join("config"))
            .env("DO_NOT_TRACK", "1")
            .output()
            .expect("execute migration CLI");
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let record = task.join("leanctx-v4-task-receipt-migration.json");
    assert!(run(&["--dry-run"]).contains("dry-run"));
    assert!(!record.exists());
    assert!(run(&[]).contains("non-authoritative"));
    let first = std::fs::read(&record).unwrap();
    let metadata: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(metadata["receipt_authority"], "legacy_non_authoritative");
    assert_eq!(metadata["source_artifacts"].as_array().unwrap().len(), 5);
    assert!(run(&[]).contains("already migrated"));
    assert_eq!(std::fs::read(&record).unwrap(), first);
    let denied = migration_command(sandbox.path(), "task-receipt", &task)
        .args(["--rollback", "--dry-run"])
        .output()
        .unwrap();
    assert!(!denied.status.success(), "{denied:?}");
    assert!(String::from_utf8_lossy(&denied.stderr).contains("cannot be combined"));
    assert_eq!(std::fs::read(&record).unwrap(), first);
    for (name, expected) in names.iter().zip(&originals) {
        assert_eq!(&std::fs::read(task.join(name)).unwrap(), expected);
    }
    assert!(run(&["--rollback"]).contains("legacy sources retained"));
    assert!(!record.exists());
    for (name, expected) in names.iter().zip(&originals) {
        assert_eq!(&std::fs::read(task.join(name)).unwrap(), expected);
    }
}

fn migration_command(
    sandbox: &std::path::Path,
    source: &str,
    input: &std::path::Path,
) -> std::process::Command {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    command
        .args(["migrate", source])
        .arg(input)
        .current_dir(sandbox)
        .env("LEAN_CTX_DATA_DIR", sandbox.join("data"))
        .env("LEAN_CTX_CONFIG_DIR", sandbox.join("config"))
        .env("DO_NOT_TRACK", "1");
    command
}

#[test]
fn config_rollback_dry_run_rejects_without_changing_migration_artifacts() {
    let sandbox = tempfile::tempdir().unwrap();
    let path = sandbox.path().join("legacy.toml");
    let original = "# retain exactly\nterse_agent = \"ultra\"\n";
    let migrated = "compression_level = \"max\"\n";
    std::fs::write(&path, original).unwrap();
    lean_ctx::config_io::write_atomic_config_migration(&path, migrated).unwrap();
    let backup = sandbox.path().join("legacy.toml.v4-migration.bak");
    let receipt = sandbox.path().join("legacy.toml.v4-migration.json");
    let paths = [&path, &backup, &receipt];
    let before: Vec<_> = paths.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let denied = migration_command(sandbox.path(), "config", &path)
        .args(["--dry-run", "--rollback"])
        .output()
        .unwrap();
    assert!(!denied.status.success(), "{denied:?}");
    assert!(String::from_utf8_lossy(&denied.stderr).contains("cannot be combined"));
    for (path, expected) in paths.iter().zip(before) {
        assert_eq!(std::fs::read(path).unwrap(), expected);
    }
    let rollback = migration_command(sandbox.path(), "config", &path)
        .arg("--rollback")
        .output()
        .unwrap();
    assert!(rollback.status.success(), "{rollback:?}");
    assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
    assert!(!backup.exists());
    assert!(!receipt.exists());
}

#[test]
fn supported_v3_receipt_fixture_remains_readable_but_non_authoritative() {
    let receipt: ExecutionReceiptV1 =
        serde_json::from_slice(V3_RECEIPT).expect("supported v3 receipt fixture must decode");
    receipt
        .validate()
        .expect("supported v3 receipt fixture must remain structurally valid");

    assert_eq!(receipt.receipt_id.as_str(), "receipt-v3-fixture");
    assert_eq!(receipt.signature, "legacy-compatibility-only");
    assert!(receipt.outcome_ref.is_none());
    assert!(
        receipt
            .evidence_refs
            .iter()
            .all(|reference| reference.signature_status
                != lean_ctx_protocol::SignatureStatus::Verified),
        "legacy fixture must not acquire canonical signature authority"
    );
}

#[test]
fn v3_receipt_fixture_migration_fails_closed_on_schema_or_unknown_fields() {
    let mut wrong_schema: serde_json::Value =
        serde_json::from_slice(V3_RECEIPT).expect("fixture JSON");
    wrong_schema["schema_version"] = serde_json::json!(2);
    assert!(serde_json::from_value::<ExecutionReceiptV1>(wrong_schema).is_err());

    let mut unknown_field: serde_json::Value =
        serde_json::from_slice(V3_RECEIPT).expect("fixture JSON");
    unknown_field["canonical_receipt"] = serde_json::json!(true);
    let receipt = serde_json::from_value::<ExecutionReceiptV1>(unknown_field)
        .expect("forward-compatible receipt must deserialize");
    assert!(
        receipt.validate().is_err(),
        "reserved ingress field must fail closed"
    );
}
