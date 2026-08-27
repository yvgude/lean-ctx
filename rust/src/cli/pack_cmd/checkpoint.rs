use std::path::{Path, PathBuf};

use crate::core::context_package::PackageLayer;
use crate::core::context_package::content::CheckpointPackageContentV1;
use sha2::{Digest, Sha256};

const MAX_CHECKPOINT_INPUT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CHECKPOINT_PACKAGE_BYTES: u64 = 16 * 1024 * 1024;

pub(super) fn cmd_pack_checkpoint_seal(args: &[String]) {
    let input = flag(args, "--checkpoint");
    let output = flag(args, "--output");
    let name = flag(args, "--name");
    let version = flag(args, "--version").unwrap_or_else(|| "1.0.0".into());
    let unsigned = args.iter().any(|arg| arg == "--unsigned");
    let (Some(input), Some(output), Some(name)) = (input, output, name) else {
        fail(
            "Usage: lean-ctx pack checkpoint-seal --checkpoint=<payload.json> --output=<file.ctxpkg> --name=<name> [--version=<v>] [--unsigned]",
        );
    };

    require_bounded_regular_file(
        Path::new(&input),
        MAX_CHECKPOINT_INPUT_BYTES,
        "checkpoint payload",
    );
    let raw = std::fs::read_to_string(&input)
        .unwrap_or_else(|error| fail(&format!("read checkpoint payload: {error}")));
    let checkpoint: CheckpointPackageContentV1 = serde_json::from_str(&raw)
        .unwrap_or_else(|error| fail(&format!("parse checkpoint payload: {error}")));
    let (manifest, content) = crate::core::context_package::PackageBuilder::new(&name, &version)
        .description("Portable ContextCheckpointV2")
        .checkpoint(checkpoint)
        .build()
        .unwrap_or_else(|error| fail(&format!("build checkpoint package: {error}")));

    let signing_key = if unsigned {
        None
    } else {
        Some(
            crate::core::context_package::keys::load_or_create()
                .unwrap_or_else(|error| fail(&format!("signing key: {error}")))
                .0,
        )
    };
    let manifest = crate::core::context_package::registry::write_checkpoint_bundle(
        manifest,
        content,
        Path::new(&output),
        signing_key.as_ref(),
    )
    .unwrap_or_else(|error| fail(&format!("seal checkpoint package: {error}")));
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "schema_version": "leanctx.ctxpkg-checkpoint-seal/v1",
            "path": PathBuf::from(output),
            "name": manifest.name,
            "version": manifest.version,
            "package_digest": format!("sha256:{}", manifest.integrity.sha256),
            "content_hash": format!("sha256:{}", manifest.integrity.content_hash),
            "signature_state": if manifest.signature.is_some() { "signed_valid" } else { "unsigned" },
        }))
        .expect("seal result serializes")
    );
}

pub(super) fn cmd_pack_checkpoint_inspect(args: &[String]) {
    let file = args
        .iter()
        .find(|arg| !arg.starts_with("--") && arg.as_str() != "checkpoint-inspect")
        .unwrap_or_else(|| fail("Usage: lean-ctx pack checkpoint-inspect <file.ctxpkg>"));
    require_bounded_regular_file(
        Path::new(file),
        MAX_CHECKPOINT_PACKAGE_BYTES,
        "checkpoint package",
    );
    let (manifest, checkpoint) =
        crate::core::context_package::registry::read_checkpoint_bundle(Path::new(file))
            .unwrap_or_else(|error| fail(&format!("inspect checkpoint package: {error}")));
    let signature_state = if manifest.signature.is_some() {
        "signed_valid"
    } else {
        "unsigned"
    };
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "schema_version": "leanctx.ctxpkg-checkpoint-inspect/v1",
            "package": {
                "schema_version": manifest.schema_version,
                "kind": manifest.kind.as_str(),
                "layers": manifest.layers.iter().map(PackageLayer::as_str).collect::<Vec<_>>(),
                "name": manifest.name,
                "version": manifest.version,
                "package_digest": format!("sha256:{}", manifest.integrity.sha256),
                "content_hash": format!("sha256:{}", manifest.integrity.content_hash),
                "signature_state": signature_state,
                "signer_public_key": manifest.signature.as_ref().map(|signature| &signature.public_key),
            },
            "checkpoint": checkpoint,
        }))
        .expect("inspect result serializes")
    );
}

fn require_bounded_regular_file(path: &Path, max_bytes: u64, label: &str) {
    let metadata = std::fs::symlink_metadata(path)
        .unwrap_or_else(|error| fail(&format!("stat {label}: {error}")));
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        fail(&format!(
            "{label} must be a bounded regular non-symlink file"
        ));
    }
}

pub(super) fn cmd_pack_snapshot_v1_inspect(args: &[String]) {
    use crate::core::context_snapshot::types::{
        MAX_SNAPSHOT_LEDGER_ITEMS, MAX_SNAPSHOT_LINEAGE_ITEMS, MAX_SNAPSHOT_SESSION_LIST,
    };

    let file = args
        .iter()
        .find(|arg| !arg.starts_with("--") && arg.as_str() != "snapshot-v1-inspect")
        .unwrap_or_else(|| fail("Usage: lean-ctx pack snapshot-v1-inspect <file.json>"));
    let path = Path::new(file);
    let metadata = std::fs::symlink_metadata(path)
        .unwrap_or_else(|error| fail(&format!("stat SnapshotV1: {error}")));
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 8 * 1024 * 1024
    {
        fail("SnapshotV1 must be a bounded regular non-symlink file");
    }
    let raw =
        std::fs::read(path).unwrap_or_else(|error| fail(&format!("read SnapshotV1: {error}")));
    let snapshot: crate::core::context_snapshot::ContextSnapshotV1 = serde_json::from_slice(&raw)
        .unwrap_or_else(|error| fail(&format!("parse SnapshotV1: {error}")));
    if snapshot.schema_version != crate::core::contracts::CONTEXT_SNAPSHOT_V1_SCHEMA_VERSION
        || snapshot.lineage.items.len() > MAX_SNAPSHOT_LINEAGE_ITEMS
        || snapshot.ledger.items.len() > MAX_SNAPSHOT_LEDGER_ITEMS
        || snapshot.session.as_ref().is_some_and(|session| {
            session.decisions.len() > MAX_SNAPSHOT_SESSION_LIST
                || session.files_touched.len() > MAX_SNAPSHOT_SESSION_LIST
        })
    {
        fail("SnapshotV1 schema or bounds are invalid");
    }
    if !crate::core::context_snapshot::verify_snapshot(&snapshot)
        .unwrap_or_else(|error| fail(&format!("verify SnapshotV1: {error}")))
    {
        fail("SnapshotV1 signature or canonical identity is invalid");
    }
    if snapshot.git.commit.as_ref().is_some_and(|commit| {
        !(7..=64).contains(&commit.len()) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) || snapshot.git.branch.as_ref().is_some_and(|branch| {
        branch.is_empty()
            || branch.len() > 255
            || branch.contains("..")
            || branch.starts_with('/')
            || branch.ends_with('/')
            || branch.chars().any(char::is_control)
    }) {
        fail("SnapshotV1 git anchor is invalid");
    }
    let mut hasher = Sha256::new();
    hasher.update(&raw);
    let artifact_digest = crate::core::agent_identity::hex_encode(&hasher.finalize());
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "schema_version": "leanctx.snapshot-v1-inspect/v1",
            "snapshot_id": snapshot.snapshot_id,
            "artifact_digest": format!("sha256:{artifact_digest}"),
            "signature_state": "signed_valid",
            "signer_public_key": snapshot.signature.as_ref().map(|signature| &signature.public_key),
        }))
        .expect("SnapshotV1 inspect result serializes")
    );
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    args.iter()
        .find_map(|arg| arg.strip_prefix(&prefix).map(str::to_string))
}

fn fail(message: &str) -> ! {
    eprintln!("ERROR: {message}");
    std::process::exit(1)
}
