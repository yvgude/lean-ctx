use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(test)]
use fs2::FileExt;
use serde::{Deserialize, Serialize};

#[path = "updater/flow.rs"]
mod flow;
#[path = "updater/transaction.rs"]
mod transaction;

use transaction::{
    acquire_update_lock, execute_prepared_transaction, prepare_update_transaction,
    recover_pending_transaction, rollback_to_previous,
};
#[cfg(test)]
use transaction::{cleanup_orphaned_prepared_files, orphan_prepared_paths, write_update_receipt};

mod platform;
use platform::{gpu_next_steps, gpu_platform_asset_name, platform_asset_name};

const GITHUB_API_RELEASES: &str = "https://api.github.com/repos/yvgude/lean-ctx/releases/latest";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const UPDATE_RECEIPT_SCHEMA: &str = "leanctx.update-receipt/v2";
const LEGACY_UPDATE_RECEIPT_SCHEMA: &str = "leanctx.update-receipt/v1";
const UPDATE_RECEIPT_FILE: &str = "update-receipt.json";
const UPDATE_TRANSACTION_SCHEMA: &str = "leanctx.update-transaction/v1";
const UPDATE_TRANSACTION_FILE: &str = "update-transaction.json";
const UPDATE_LOCK_FILE: &str = "update.lock";

pub(crate) fn run(args: &[String]) {
    run_with_mode(args, UpdateMode::Normal);
}

pub(crate) fn enable_gpu(args: &[String]) {
    run_with_mode(args, UpdateMode::EnableGpu);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateMode {
    Normal,
    EnableGpu,
}

/// Flags understood by the update flow. Reject typos before the flow can
/// change the scheduler, config, or installed binary.
const KNOWN_FLAGS: &[&str] = &[
    "--check",
    "--quiet",
    "--skip-rules",
    "--scheduled",
    "--schedule",
    "--insecure", // Preserve the flow's explicit refusal for this removed option.
    "--status",
    "--rollback",
    "--unpin",
    "--pin",
    "--recover-update",
    "--lock-held",
    // Accepted for compatibility with older scripts and docs.
    "--force",
    "--rewire",
];

#[derive(Debug, PartialEq, Eq)]
enum FlagCheck<'a> {
    Run,
    Help,
    Unknown(&'a str),
}

fn check_flags(args: &[String]) -> FlagCheck<'_> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return FlagCheck::Help;
    }

    args.iter()
        .map(String::as_str)
        .find(|arg| arg.starts_with('-') && !KNOWN_FLAGS.contains(arg))
        .map_or(FlagCheck::Run, FlagCheck::Unknown)
}

fn command_name(mode: UpdateMode) -> &'static str {
    match mode {
        UpdateMode::Normal => "update",
        UpdateMode::EnableGpu => "enable-gpu",
    }
}

fn print_help(mode: UpdateMode) {
    let command = command_name(mode);
    println!("Usage: lean-ctx {command} [VERSION] [OPTIONS]");
    println!();
    println!("  VERSION                install this release instead of the latest");
    println!("  --check                only report whether an update is available");
    println!("  --quiet                suppress output unless something changes");
    println!("  --skip-rules           do not refresh agent rules after updating");
    println!("  --pin VERSION          pin and install this release");
    println!("  --unpin                clear the release pin");
    println!("  --status               show the pin and retained binary receipt");
    println!("  --rollback             restore the retained verified binary");
    println!("  --schedule [N|Nh]      install automatic updates every N hours (default 6)");
    println!("  --schedule notify      only notify about updates");
    println!("  --schedule status      show the update schedule");
    println!("  --schedule off|disable disable automatic updates");
    println!("  -h, --help             show this help");
}

fn run_with_mode(args: &[String], mode: UpdateMode) {
    match check_flags(args) {
        FlagCheck::Run => {}
        FlagCheck::Help => {
            print_help(mode);
            return;
        }
        FlagCheck::Unknown(flag) => {
            eprintln!("  \x1b[31m✗\x1b[0m Unknown option: {flag}");
            eprintln!(
                "  \x1b[2mSee: lean-ctx {} --help\x1b[0m",
                command_name(mode)
            );
            std::process::exit(2);
        }
    }

    flow::run_with_mode(args, mode);
}

#[cfg(test)]
mod argument_guard_tests {
    use super::{FlagCheck, UpdateMode, check_flags, command_name};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn help_never_falls_through_to_an_update() {
        assert_eq!(check_flags(&args(&["--help"])), FlagCheck::Help);
        assert_eq!(check_flags(&args(&["-h"])), FlagCheck::Help);
        assert_eq!(
            check_flags(&args(&["--schedule", "off", "--broken", "--help"])),
            FlagCheck::Help
        );
        assert_eq!(command_name(UpdateMode::Normal), "update");
        assert_eq!(command_name(UpdateMode::EnableGpu), "enable-gpu");
    }

    #[test]
    fn unknown_flags_are_refused() {
        assert_eq!(
            check_flags(&args(&["--chek"])),
            FlagCheck::Unknown("--chek")
        );
        assert_eq!(
            check_flags(&args(&["3.10.3", "--forse"])),
            FlagCheck::Unknown("--forse")
        );
    }

    #[test]
    fn scheduler_and_known_invocations_still_run() {
        for ok in [
            &[][..],
            &["--quiet", "--scheduled"][..],
            &["--check"][..],
            &["3.10.3", "--skip-rules"][..],
            &["--schedule", "12h"][..],
            &["--schedule", "off"][..],
            &["--schedule", "disable"][..],
            &["--status"][..],
            &["--rollback"][..],
            &["--pin", "3.10.3"][..],
            &["--unpin"][..],
            &["--recover-update", "--lock-held"][..],
            &["--insecure"][..],
            &["--force"][..],
            &["--rewire"][..],
        ] {
            assert_eq!(check_flags(&args(ok)), FlagCheck::Run, "{ok:?}");
        }
    }
}

/// Outcome of the config gate applied to automatic (scheduled) update runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoUpdateGate {
    /// Install normally.
    Proceed,
    /// Auto-update disabled in config — skip and clean up the scheduler.
    Skip,
    /// Notify-only — check for a newer version but never install.
    NotifyOnly,
}

/// Decide what a scheduled `update` run should do based on config. Pure helper
/// so the precedence (`auto_update` wins over `notify_only`) is unit-testable.
fn automatic_update_gate(auto_update: bool, notify_only: bool) -> AutoUpdateGate {
    if !auto_update {
        AutoUpdateGate::Skip
    } else if notify_only {
        AutoUpdateGate::NotifyOnly
    } else {
        AutoUpdateGate::Proceed
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct UpdateReceipt {
    schema_version: String,
    active: BinaryReceipt,
    previous: BinaryReceipt,
    #[serde(default)]
    receipt_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BinaryReceipt {
    version: String,
    asset: String,
    sha256: String,
    size: u64,
    path: String,
    manifest_sha256: Option<String>,
    archive_sha256: Option<String>,
    #[serde(default)]
    release_commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedArtifact {
    archive_sha256: String,
    manifest_sha256: String,
    release_commit: String,
    payload_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PreparedTransaction {
    schema_version: String,
    operation: String,
    current_path: String,
    previous_path: String,
    staged_path: String,
    backup_path: String,
    old_active: BinaryReceipt,
    target_active: BinaryReceipt,
    target_previous: BinaryReceipt,
    transaction_sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReleaseManifest {
    schema_version: String,
    tag: String,
    commit: String,
    artifacts: std::collections::HashMap<String, ManifestArtifact>,
    sbom_sha256: String,
    checksums_sha256: String,
}

#[derive(Debug, Deserialize)]
struct ManifestArtifact {
    sha256: String,
    size: u64,
    #[serde(default)]
    payload_sha256: Option<String>,
}

fn verify_download_integrity(
    release: &serde_json::Value,
    asset_name: &str,
    bytes: &[u8],
) -> Result<VerifiedArtifact, String> {
    #[cfg(not(feature = "secure-update"))]
    {
        let _ = (release, asset_name, bytes);
        return Err("secure-update feature disabled (sha256 verification unavailable)".to_string());
    }

    #[cfg(feature = "secure-update")]
    {
        let release_tag = release["tag_name"]
            .as_str()
            .ok_or_else(|| "release metadata has no tag_name".to_string())?;
        let manifest_url = find_asset_url(release, "release-manifest.json")
            .ok_or_else(|| "release-manifest.json is required for binary updates".to_string())?;
        let manifest_bytes = download_bytes(&manifest_url)?;
        let manifest_signature_url = find_asset_url(release, "release-manifest.json.sig")
            .ok_or_else(|| {
                "release-manifest.json.sig is required for binary updates".to_string()
            })?;
        let manifest_certificate_url = find_asset_url(release, "release-manifest.json.pem")
            .ok_or_else(|| {
                "release-manifest.json.pem is required for binary updates".to_string()
            })?;
        verify_cosign_signature(
            &manifest_bytes,
            &download_bytes(&manifest_signature_url)?,
            &download_bytes(&manifest_certificate_url)?,
            release_tag,
        )?;
        let manifest_sha256 = sha256_hex(&manifest_bytes);
        let manifest: ReleaseManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| format!("invalid release-manifest.json: {e}"))?;
        validate_manifest(&manifest, release_tag, asset_name)?;
        let artifact = manifest
            .artifacts
            .get(asset_name)
            .ok_or_else(|| format!("manifest has no artifact entry for {asset_name}"))?;
        let computed = sha256_hex(bytes);
        if bytes.len() as u64 != artifact.size {
            return Err(format!(
                "size mismatch for {asset_name}: expected {}, got {}",
                artifact.size,
                bytes.len()
            ));
        }
        if !constant_time_eq(computed.as_bytes(), artifact.sha256.as_bytes()) {
            return Err(format!(
                "sha256 mismatch for {asset_name}: expected {}, got {computed}",
                artifact.sha256
            ));
        }

        let checksum_url = find_asset_url(release, "SHA256SUMS")
            .ok_or_else(|| "SHA256SUMS is required for binary updates".to_string())?;
        let checksum_bytes = download_bytes(&checksum_url)?;
        let signature_url = find_asset_url(release, "SHA256SUMS.sig")
            .ok_or_else(|| "SHA256SUMS.sig is required for binary updates".to_string())?;
        let signature_bytes = download_bytes(&signature_url)?;
        let certificate_url = find_asset_url(release, "SHA256SUMS.pem")
            .ok_or_else(|| "SHA256SUMS.pem is required for binary updates".to_string())?;
        let certificate_bytes = download_bytes(&certificate_url)?;
        verify_cosign_signature(
            &checksum_bytes,
            &signature_bytes,
            &certificate_bytes,
            release_tag,
        )?;
        verify_release_commit(release_tag, &manifest.commit)?;
        let checksum_sha256 = sha256_hex(&checksum_bytes);
        if !constant_time_eq(
            checksum_sha256.as_bytes(),
            manifest.checksums_sha256.to_ascii_lowercase().as_bytes(),
        ) {
            return Err("SHA256SUMS digest does not match release manifest".to_string());
        }
        let sbom_url = find_asset_url(release, "SBOM.cdx.json")
            .ok_or_else(|| "SBOM.cdx.json is required for binary updates".to_string())?;
        let sbom_sha256 = sha256_hex(&download_bytes(&sbom_url)?);
        if !constant_time_eq(
            sbom_sha256.as_bytes(),
            manifest.sbom_sha256.to_ascii_lowercase().as_bytes(),
        ) {
            return Err("SBOM digest does not match release manifest".to_string());
        }
        let checksum_text = String::from_utf8(checksum_bytes)
            .map_err(|_| "SHA256SUMS is not valid UTF-8".to_string())?;
        let expected = parse_sha256sums(&checksum_text, asset_name)
            .ok_or_else(|| format!("SHA256SUMS has no unique entry for {asset_name}"))?;
        if !constant_time_eq(expected.as_bytes(), computed.as_bytes())
            || !constant_time_eq(expected.as_bytes(), artifact.sha256.as_bytes())
        {
            return Err(format!("SHA256SUMS digest mismatch for {asset_name}"));
        }
        Ok(VerifiedArtifact {
            archive_sha256: computed,
            manifest_sha256,
            release_commit: manifest.commit,
            payload_sha256: artifact.payload_sha256.clone().ok_or_else(|| {
                format!("release manifest omits extracted payload digest for {asset_name}")
            })?,
        })
    }
}

fn cosign_identity_for_tag(release_tag: &str) -> String {
    let escaped_tag = regex::escape(release_tag);
    format!(
        "^https://github\\.com/yvgude/lean-ctx/\\.github/workflows/release\\.yml@refs/tags/{escaped_tag}$"
    )
}

fn verify_cosign_signature(
    checksums: &[u8],
    signature: &[u8],
    certificate: &[u8],
    release_tag: &str,
) -> Result<(), String> {
    let directory =
        tempfile::tempdir().map_err(|e| format!("cannot create signature workspace: {e}"))?;
    let checksum_path = directory.path().join("SHA256SUMS");
    let signature_path = directory.path().join("SHA256SUMS.sig");
    let certificate_path = directory.path().join("SHA256SUMS.pem");
    std::fs::write(&checksum_path, checksums).map_err(|e| e.to_string())?;
    std::fs::write(&signature_path, signature).map_err(|e| e.to_string())?;
    std::fs::write(&certificate_path, certificate).map_err(|e| e.to_string())?;
    let output = std::process::Command::new("cosign")
        .args([
            "verify-blob",
            "--signature",
            signature_path.to_str().unwrap_or(""),
            "--certificate",
            certificate_path.to_str().unwrap_or(""),
            "--certificate-identity-regexp",
            &cosign_identity_for_tag(release_tag),
            "--certificate-oidc-issuer",
            "https://token.actions.githubusercontent.com",
            checksum_path.to_str().unwrap_or(""),
        ])
        .output()
        .map_err(|e| format!("cosign is unavailable; refusing unsigned release: {e}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "cosign release signature verification failed: {detail}"
        ));
    }
    Ok(())
}

fn fetch_api_json(url: &str) -> Result<serde_json::Value, String> {
    const API_PREFIX: &str = "https://api.github.com/repos/yvgude/lean-ctx/";
    if !url.starts_with(API_PREFIX) {
        return Err(format!(
            "refusing GitHub API URL outside canonical repository: {url}"
        ));
    }
    let response = https_agent()
        .get(url)
        .header("User-Agent", &format!("lean-ctx/{CURRENT_VERSION}"))
        .header("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| e.to_string())?;
    response
        .into_body()
        .read_to_string()
        .map_err(|e| e.to_string())
        .and_then(|body| serde_json::from_str(&body).map_err(|e| e.to_string()))
}

fn verify_release_commit(release_tag: &str, expected_commit: &str) -> Result<(), String> {
    let tag = release_tag.trim_start_matches('v');
    let reference = fetch_api_json(&format!(
        "https://api.github.com/repos/yvgude/lean-ctx/git/ref/tags/v{tag}"
    ))?;
    let object = reference
        .get("object")
        .ok_or_else(|| "GitHub tag reference has no object".to_string())?;
    let object_sha = object
        .get("sha")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "GitHub tag reference has no object SHA".to_string())?;
    let commit = match object.get("type").and_then(serde_json::Value::as_str) {
        Some("commit") => object_sha.to_string(),
        Some("tag") => {
            let tag_object = fetch_api_json(&format!(
                "https://api.github.com/repos/yvgude/lean-ctx/git/tags/{object_sha}"
            ))?;
            tag_object["object"]["sha"]
                .as_str()
                .ok_or_else(|| "annotated GitHub tag has no commit object".to_string())?
                .to_string()
        }
        Some(other) => return Err(format!("unsupported GitHub tag object type `{other}`")),
        None => return Err("GitHub tag reference has no object type".to_string()),
    };
    if commit.len() != 40
        || !commit.chars().all(|c| c.is_ascii_hexdigit())
        || !constant_time_eq(
            commit.to_ascii_lowercase().as_bytes(),
            expected_commit.to_ascii_lowercase().as_bytes(),
        )
    {
        return Err("release manifest commit does not match the signed GitHub tag".to_string());
    }
    Ok(())
}

fn validate_manifest(
    manifest: &ReleaseManifest,
    release_tag: &str,
    asset_name: &str,
) -> Result<(), String> {
    if manifest.schema_version != "leanctx.release-manifest/v1" {
        return Err(format!(
            "unsupported release manifest schema `{}`",
            manifest.schema_version
        ));
    }
    if manifest.tag.trim_start_matches('v') != release_tag.trim_start_matches('v') {
        return Err(format!(
            "manifest tag `{}` does not match release tag `{release_tag}`",
            manifest.tag
        ));
    }
    if manifest.commit.len() != 40 || !manifest.commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("release manifest commit is not a 40-character hexadecimal SHA".to_string());
    }
    if manifest.checksums_sha256.len() != 64
        || !manifest
            .checksums_sha256
            .chars()
            .all(|c| c.is_ascii_hexdigit())
    {
        return Err("release manifest has an invalid checksums_sha256 digest".to_string());
    }
    if manifest.sbom_sha256.len() != 64
        || !manifest.sbom_sha256.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err("release manifest has an invalid sbom_sha256 digest".to_string());
    }
    let artifact = manifest
        .artifacts
        .get(asset_name)
        .ok_or_else(|| format!("release manifest omits {asset_name}"))?;
    if artifact.sha256.len() != 64 || !artifact.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "release manifest has invalid digest for {asset_name}"
        ));
    }
    let payload_sha256 = artifact.payload_sha256.as_ref().ok_or_else(|| {
        format!("release manifest omits extracted payload digest for {asset_name}")
    })?;
    if payload_sha256.len() != 64 || !payload_sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "release manifest has invalid extracted payload digest for {asset_name}"
        ));
    }
    Ok(())
}

fn parse_sha256sums(text: &str, asset_name: &str) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let mut parts = l.split_whitespace();
        let hash = parts.next().unwrap_or("");
        let file = parts.next().unwrap_or("");
        if file == asset_name {
            if found.is_some() || hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            found = Some(hash.to_ascii_lowercase());
        }
    }
    found
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    hex_lower(&out)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn canonical_state_dir() -> Result<PathBuf, String> {
    let state = crate::core::paths::state_dir()?;
    std::fs::create_dir_all(&state).map_err(|e| {
        format!(
            "cannot create update state directory {}: {e}",
            state.display()
        )
    })?;
    reject_symlink(&state)?;
    std::fs::canonicalize(&state).map_err(|e| format!("cannot resolve state directory: {e}"))
}

fn update_layout(state_dir: &Path) -> Result<(), String> {
    let updates = state_dir.join("updates");
    let previous = updates.join("previous");
    let staged = updates.join("staged");
    for directory in [&updates, &previous, &staged] {
        std::fs::create_dir_all(directory).map_err(|e| {
            format!(
                "cannot create update directory {}: {e}",
                directory.display()
            )
        })?;
        reject_symlink(directory)?;
    }
    Ok(())
}

fn update_transaction_path() -> Result<PathBuf, String> {
    let state = canonical_state_dir()?;
    update_layout(&state)?;
    let path = state.join(UPDATE_TRANSACTION_FILE);
    reject_symlink_if_present(&path)?;
    Ok(path)
}

fn update_lock_path() -> Result<PathBuf, String> {
    let state = canonical_state_dir()?;
    update_layout(&state)?;
    let path = state.join(UPDATE_LOCK_FILE);
    reject_symlink_if_present(&path)?;
    Ok(path)
}

fn staged_update_path(operation: &str, backup: bool) -> Result<PathBuf, String> {
    if operation != "update" && operation != "rollback" {
        return Err(format!("unsupported update operation `{operation}`"));
    }
    let state = canonical_state_dir()?;
    update_layout(&state)?;
    let suffix = if backup { "backup" } else { "target" };
    Ok(state
        .join("updates")
        .join("staged")
        .join(format!("{operation}-{suffix}.bin")))
}

fn previous_binary_path(state_path: &Path, current_exe: &Path) -> Result<PathBuf, String> {
    let name = current_exe
        .file_name()
        .ok_or_else(|| "current executable has no file name".to_string())?;
    Ok(state_path.join("updates").join("previous").join(name))
}

fn canonical_current_exe(current_exe: &Path) -> Result<PathBuf, String> {
    let metadata = std::fs::symlink_metadata(current_exe).map_err(|e| {
        format!(
            "cannot inspect current executable {}: {e}",
            current_exe.display()
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err("current executable is a symlink; refusing update".to_string());
    }
    std::fs::canonicalize(current_exe).map_err(|e| {
        format!(
            "cannot resolve current executable {}: {e}",
            current_exe.display()
        )
    })
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| format!("cannot inspect update path {}: {e}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!("update path is a symlink: {}", path.display()));
    }
    Ok(())
}

fn reject_symlink_if_present(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("update path is a symlink: {}", path.display()))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "cannot inspect update path {}: {e}",
            path.display()
        )),
    }
}

fn ensure_no_symlink_under(root: &Path, path: &Path) -> Result<(), String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| format!("update path escapes state directory: {}", path.display()))?;
    let root = std::fs::canonicalize(root)
        .map_err(|e| format!("cannot resolve update root {}: {e}", root.display()))?;
    let mut cursor = root;
    for component in relative.components() {
        match component {
            std::path::Component::Normal(component) => cursor.push(component),
            std::path::Component::CurDir => continue,
            _ => {
                return Err(format!(
                    "update path contains unsafe component: {}",
                    path.display()
                ));
            }
        }
        reject_symlink_if_present(&cursor)?;
    }
    Ok(())
}

fn canonical_update_paths(current_exe: &Path) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let state = canonical_state_dir()?;
    update_layout(&state)?;
    let current = canonical_current_exe(current_exe)?;
    let receipt = state.join(UPDATE_RECEIPT_FILE);
    let previous = previous_binary_path(&state, &current)?;
    ensure_no_symlink_under(&state, &receipt)?;
    ensure_no_symlink_under(&state, &previous)?;
    Ok((state, receipt, previous))
}

fn validate_receipt_paths(
    receipt: &UpdateReceipt,
    state_dir: &Path,
    current_exe: &Path,
) -> Result<(), String> {
    let current = canonical_current_exe(current_exe)?;
    let previous = previous_binary_path(state_dir, &current)?;
    if Path::new(&receipt.active.path) != current {
        return Err("active receipt path is not the canonical executable".to_string());
    }
    if Path::new(&receipt.previous.path) != previous {
        return Err("previous receipt path is not the canonical retained binary".to_string());
    }
    ensure_no_symlink_under(state_dir, &previous)?;
    Ok(())
}

fn load_update_receipt() -> Result<Option<UpdateReceipt>, String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    load_update_receipt_for(&current)
}

fn load_update_receipt_for(current_exe: &Path) -> Result<Option<UpdateReceipt>, String> {
    let (state, path, _) = canonical_update_paths(current_exe)?;
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let receipt: UpdateReceipt =
        serde_json::from_str(&raw).map_err(|e| format!("invalid {}: {e}", path.display()))?;
    validate_receipt(&receipt)?;
    validate_receipt_integrity(&receipt)?;
    validate_receipt_paths(&receipt, &state, current_exe)?;
    Ok(Some(receipt))
}

fn validate_receipt(receipt: &UpdateReceipt) -> Result<(), String> {
    if receipt.schema_version != UPDATE_RECEIPT_SCHEMA
        && receipt.schema_version != LEGACY_UPDATE_RECEIPT_SCHEMA
    {
        return Err(format!(
            "unsupported update receipt schema `{}`",
            receipt.schema_version
        ));
    }
    for (label, binary) in [("active", &receipt.active), ("previous", &receipt.previous)] {
        if binary.sha256.len() != 64 || !binary.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("{label} receipt has an invalid binary digest"));
        }
        if binary.size == 0 || binary.path.trim().is_empty() || binary.version.trim().is_empty() {
            return Err(format!("{label} receipt has missing identity or size"));
        }
        if let Some(digest) = &binary.manifest_sha256
            && (digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(format!("{label} receipt has an invalid manifest digest"));
        }
        if let Some(digest) = &binary.archive_sha256
            && (digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(format!("{label} receipt has an invalid archive digest"));
        }
        if let Some(commit) = &binary.release_commit
            && (commit.len() != 40 || !commit.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(format!("{label} receipt has an invalid release commit"));
        }
    }
    if let Some(digest) = &receipt.receipt_sha256
        && (digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return Err("update receipt has an invalid integrity digest".to_string());
    }
    Ok(())
}

fn receipt_digest(receipt: &UpdateReceipt) -> Result<String, String> {
    let mut unsigned = receipt.clone();
    unsigned.receipt_sha256 = None;
    let bytes = serde_json::to_vec(&unsigned).map_err(|e| e.to_string())?;
    Ok(sha256_hex(&bytes))
}

fn validate_receipt_integrity(receipt: &UpdateReceipt) -> Result<(), String> {
    if receipt.schema_version == LEGACY_UPDATE_RECEIPT_SCHEMA {
        return Ok(());
    }
    let actual = receipt
        .receipt_sha256
        .as_deref()
        .ok_or_else(|| "v2 update receipt is missing its integrity digest".to_string())?;
    let expected = receipt_digest(receipt)?;
    if !constant_time_eq(actual.as_bytes(), expected.as_bytes()) {
        return Err("update receipt integrity digest mismatch".to_string());
    }
    Ok(())
}

pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    reject_symlink_if_present(parent)?;
    reject_symlink_if_present(path)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} has no valid file name", path.display()))?;
    let tmp = parent.join(format!(".{name}.tmp"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    let result = (|| {
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(|e| format!("{}: {e}", path.display()))
}

/// #828: One-time migration — enable `shadow_mode` for users who never
/// explicitly set it. Without shadow mode, most harnesses silently prefer
/// native tools, negating lean-ctx's token savings. Idempotent: a state-dir
/// marker prevents repeated flips if the user disables it after migration.
fn migrate_shadow_mode_default() {
    let Ok(state) = crate::core::paths::state_dir() else {
        return;
    };
    let marker = state.join("shadow_mode_migrated");
    if marker.exists() {
        return;
    }

    // Read raw TOML to distinguish "explicitly set to false" from "never set".
    let global = crate::core::config::Config::load_global();
    let raw_toml = crate::core::config::Config::path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();

    let explicitly_set = raw_toml.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("shadow_mode") && trimmed.contains('=')
    });

    if !explicitly_set && !global.shadow_mode {
        // User never touched shadow_mode — the old default was false.
        // Set it to true in the global config so it persists.
        match crate::core::config::Config::update_global(|c| c.shadow_mode = true) {
            Ok(_) => {
                eprintln!("  \u{2139} shadow_mode enabled (recommended default since v3.9.9).");
                eprintln!("    Disable: lean-ctx config set shadow_mode false");
            }
            Err(e) => tracing::warn!("could not enable shadow_mode during update: {e}"),
        }
    } else if explicitly_set && !global.shadow_mode {
        eprintln!("  \u{2139} shadow_mode is false in your config. Since v3.9.9, true is the");
        eprintln!("    recommended default (60%+ more token savings). Enable:");
        eprintln!("    lean-ctx config set shadow_mode true");
    }

    // Write marker so this migration runs only once.
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&marker, "migrated");
}

/// Public entry point for the shadow_mode migration, callable from
/// `cmd_dev_install` and other lifecycle commands.
pub(crate) fn migrate_shadow_mode_default_public() {
    migrate_shadow_mode_default();
}

fn post_update_rewire(skip_rules: bool) {
    // #356: regenerate installed LaunchAgent plists so they adopt the new
    // deny-~/Documents seatbelt wrapper. A plist is only (re)written on install,
    // so without this an upgrade keeps the old unwrapped plist — and the TCC
    // prompt — until the next manual enable. Idempotent: install rewrites the
    // plist and re-bootstraps it.
    #[cfg(target_os = "macos")]
    rewrap_launchagents_for_tcc();

    // The persist decision reads the GLOBAL file only and writes via
    // update_global, so a project-local override is never leaked into the
    // global config (#443).
    if crate::core::config::Config::load_global()
        .proxy_enabled
        .is_none()
        && crate::proxy_autostart::is_installed()
    {
        match crate::core::config::Config::update_global(|c| c.proxy_enabled = Some(true)) {
            Ok(_) => {
                eprintln!("  \u{2139} Proxy was already active \u{2014} keeping enabled.");
                eprintln!("    Disable anytime: lean-ctx proxy disable");
            }
            Err(e) => tracing::warn!("could not persist proxy_enabled during update: {e}"),
        }
    }

    // #828: shadow_mode default changed to true. Migrate existing configs
    // that never explicitly set it (still at old default false).
    migrate_shadow_mode_default();

    // Runtime decisions use the effective (global + project-local) config.
    let cfg = crate::core::config::Config::load();
    let proxy_active = cfg.proxy_enabled == Some(true);

    // Determine whether rules should be injected during rewire.
    // CLI --skip-rules always wins. Otherwise, respect the config setting.
    let effective_skip_rules = if skip_rules {
        true
    } else {
        !cfg.setup.should_inject_rules()
    };

    // PHASE 1: Restart proxy BEFORE writing env vars.
    if proxy_active {
        restart_proxy_if_running();
        wait_for_proxy_health(crate::proxy_setup::default_port());
    }

    // PHASE 2: Run setup which writes MCP configs (always) and rules (if opted in).
    // #1026: shell hooks are refreshed here — but install_all_with_style
    // respects shell_hook_disabled, so users who opt out keep control.
    if !cfg.shell_hook_disabled_effective() {
        eprintln!("  \u{2139} Refreshing shell hooks in ~/.zshenv / ~/.bashenv.");
        eprintln!("    Set shell_hook_disabled=true to prevent this in future updates.");
    }
    let opts = crate::setup::SetupOptions {
        non_interactive: true,
        yes: true,
        fix: true,
        skip_proxy: !proxy_active,
        skip_rules: effective_skip_rules,
        ..Default::default()
    };
    if let Err(e) = crate::setup::run_setup_with_options(opts) {
        tracing::error!("Setup refresh error: {e}");
    }
}

/// #356: rewrite every installed LaunchAgent plist (daemon, proxy, auto-updater)
/// so an upgrade re-emits them with the deny-~/Documents seatbelt wrapper.
/// plists are only generated on install, so an existing install would otherwise
/// keep the pre-wrapper plist — and the prompt — until the next manual `enable`.
/// Each `install` / `install_schedule` rewrites the plist and re-bootstraps it.
#[cfg(target_os = "macos")]
fn rewrap_launchagents_for_tcc() {
    if crate::proxy_autostart::is_installed() {
        crate::proxy_autostart::install(crate::proxy_setup::default_port(), true);
    }
    if crate::daemon_autostart::is_installed() {
        crate::daemon_autostart::install(true);
    }
    if crate::core::update_scheduler::schedule_status().enabled {
        let hours = crate::core::config::Config::load()
            .updates
            .check_interval_hours;
        if let Err(e) = crate::core::update_scheduler::install_schedule(hours) {
            tracing::warn!("#356 re-wrap of auto-update LaunchAgent failed: {e}");
        }
    }
}

fn wait_for_proxy_health(port: u16) {
    let max_attempts = 20;
    for i in 0..max_attempts {
        if is_proxy_reachable(port) {
            println!("  \x1b[32m✓\x1b[0m Proxy healthy on port {port}");
            return;
        }
        if i == 0 {
            print!("  \x1b[2mWaiting for proxy to become healthy");
        }
        print!(".");
        use std::io::Write;
        std::io::stdout().flush().ok();
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    println!();
    eprintln!(
        "  \x1b[33m⚠\x1b[0m Proxy did not respond within {}s — writing env vars anyway",
        max_attempts / 2
    );
    eprintln!("    If Claude Code shows connection errors, run: lean-ctx proxy start");
}

fn restart_proxy_if_running() {
    let port = crate::proxy_setup::default_port();

    if restart_managed_proxy() {
        return;
    }

    if is_proxy_reachable(port) {
        println!(
            "  \x1b[33m⟳\x1b[0m Proxy running on port {port} — restart it to use the new binary:"
        );
        println!("    \x1b[1mlean-ctx proxy start --port={port}\x1b[0m");
    }
}

/// Restart proxy managed by launchd (macOS) or systemd (Linux).
/// Returns `true` if a managed service was found and restarted.
fn restart_managed_proxy() -> bool {
    #[cfg(target_os = "macos")]
    {
        let plist_path = dirs::home_dir()
            .unwrap_or_default()
            .join("Library/LaunchAgents/com.leanctx.proxy.plist");
        if plist_path.exists() {
            if crate::core::launchd::bootstrap("com.leanctx.proxy", &plist_path) {
                println!("  \x1b[32m✓\x1b[0m Proxy restarted (LaunchAgent)");
            } else {
                println!("  \x1b[33m⚠\x1b[0m Could not restart proxy LaunchAgent");
            }
            return true;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let service_path = dirs::home_dir()
            .unwrap_or_default()
            .join(".config/systemd/user/lean-ctx-proxy.service");
        if service_path.exists() {
            let result = std::process::Command::new("systemctl")
                .args(["--user", "restart", "lean-ctx-proxy"])
                .output();
            match result {
                Ok(o) if o.status.success() => {
                    println!("  \x1b[32m✓\x1b[0m Proxy restarted (systemd)");
                }
                _ => {
                    println!("  \x1b[33m⚠\x1b[0m Could not restart proxy systemd service");
                }
            }
            return true;
        }
    }

    false
}

fn is_proxy_reachable(port: u16) -> bool {
    ureq::get(&format!("http://127.0.0.1:{port}/health"))
        .call()
        .is_ok()
}

/// Builds the GitHub Releases API URL: the latest release when `version` is
/// `None`, or a specific tag (`v{version}`) when pinned (#447). The leading
/// `v` is normalised so both `3.8.5` and `v3.8.5` resolve to the `v3.8.5` tag.
fn release_api_url(version: Option<&str>) -> String {
    match version {
        None => GITHUB_API_RELEASES.to_string(),
        Some(v) => {
            let core = v.trim_start_matches('v');
            format!("https://api.github.com/repos/yvgude/lean-ctx/releases/tags/v{core}")
        }
    }
}

/// ureq agent that trusts the OS store (incl. corporate proxy CAs) via the
/// platform verifier. ureq's default `RootCerts::WebPki` ignores the system
/// store, so updates fail with `UnknownIssuer` behind TLS-intercepting proxies.
fn https_agent() -> ureq::Agent {
    // Bound the connection-setup phases so a dead network, a stuck DNS
    // resolver or an unresponsive server can never make `lean-ctx update`
    // hang indefinitely (the reported "stuck updating"). These cap DNS,
    // TCP connect and time-to-first-byte only — a large but *progressing*
    // binary download is intentionally NOT limited (no global / recv-body
    // cap on this shared agent), so slow links still complete.
    crate::core::http_client::ureq_agent_with_timeouts(
        Some(std::time::Duration::from_secs(15)),
        Some(std::time::Duration::from_secs(20)),
        Some(std::time::Duration::from_secs(30)),
    )
}

/// Fetches release metadata from GitHub. `version = None` returns the latest
/// release; `Some(v)` returns the specific tagged release for version pinning.
fn fetch_release(version: Option<&str>) -> Result<serde_json::Value, String> {
    let response = https_agent()
        .get(&release_api_url(version))
        .header("User-Agent", &format!("lean-ctx/{CURRENT_VERSION}"))
        .header("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| e.to_string())?;

    response
        .into_body()
        .read_to_string()
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
}

/// Extracts an explicit version argument from `update` args, if present.
/// Returns the first positional token (one that is not a `--flag`); the
/// `--schedule` subcommand is consumed earlier so it never reaches here.
fn parse_target_version(args: &[String]) -> Option<&str> {
    args.iter()
        .map(String::as_str)
        .find(|a| !a.starts_with('-'))
}

/// True if `s` looks like a release version (optionally `v`-prefixed, e.g.
/// `3.8.5` / `v3.8.5` / `3.8.5-rc1`), so typos are rejected before the API call.
fn looks_like_version(s: &str) -> bool {
    let core = s.strip_prefix('v').unwrap_or(s);
    core.contains('.')
        && core.starts_with(|c: char| c.is_ascii_digit())
        && core
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c.is_ascii_alphabetic())
}

fn find_asset_url(release: &serde_json::Value, asset_name: &str) -> Option<String> {
    release["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str() == Some(asset_name))
        .and_then(|a| a["browser_download_url"].as_str())
        .map(std::string::ToString::to_string)
}

fn download_bytes(url: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://github.com/yvgude/lean-ctx/") {
        return Err(format!("refusing release asset from unexpected URL: {url}"));
    }
    let response = https_agent()
        .get(url)
        .header("User-Agent", &format!("lean-ctx/{CURRENT_VERSION}"))
        .call()
        .map_err(|e| e.to_string())?;

    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

fn extract_binary(archive_bytes: &[u8], asset_name: &str) -> Result<Vec<u8>, String> {
    if Path::new(asset_name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
        extract_from_zip(archive_bytes)
    } else {
        extract_from_tar_gz(archive_bytes)
    }
}

fn replace_staged_binary(
    staged_path: &std::path::Path,
    current_exe: &std::path::Path,
) -> Result<(), String> {
    reject_symlink_if_present(staged_path)?;
    let staged = std::fs::metadata(staged_path).map_err(|e| {
        format!(
            "cannot inspect staged binary {}: {e}",
            staged_path.display()
        )
    })?;
    if staged.len() == 0 {
        return Err("refusing to install an empty staged binary".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(staged_path, std::fs::Permissions::from_mode(0o755));
    }

    // On Windows, a running executable can be renamed but not overwritten.
    // Move the current binary out of the way first, then move the new one in.
    // If the file is locked (MCP server running), try stopping managed processes
    // first, then schedule a deferred update as last resort.
    #[cfg(windows)]
    {
        let old_path = current_exe.with_extension("old.exe");
        if old_path.exists() {
            return Err(format!(
                "stale Windows rollback binary exists: {}",
                old_path.display()
            ));
        }

        if let Ok(()) = std::fs::rename(current_exe, &old_path) {
            if let Err(e) = std::fs::rename(staged_path, current_exe) {
                let _ = std::fs::rename(&old_path, current_exe);
                return Err(format!("Cannot place new binary: {e}"));
            }
            Ok(())
        } else {
            // Binary is locked. Try to stop managed processes first.
            eprintln!("\nBinary is locked. Stopping managed lean-ctx processes...");
            stop_managed_windows_processes();

            // Brief wait for processes to release file handles.
            std::thread::sleep(std::time::Duration::from_millis(1500));

            // Retry after stopping.
            if let Ok(()) = std::fs::rename(current_exe, &old_path) {
                if let Err(e) = std::fs::rename(staged_path, current_exe) {
                    let _ = std::fs::rename(&old_path, current_exe);
                    return Err(format!("Cannot place new binary: {e}"));
                }
                Ok(())
            } else {
                // Still locked (likely MCP server held by editor).
                print_blocking_processes(current_exe);
                deferred_windows_update(staged_path, current_exe)
            }
        }
    }

    #[cfg(not(windows))]
    {
        // Same-filesystem rename is atomic and retains the old inode on
        // Unix/macOS, so a failed swap cannot leave the install path absent.
        std::fs::rename(staged_path, current_exe).map_err(|e| {
            let _ = std::fs::remove_file(staged_path);
            format!("Cannot replace binary (permission denied?): {e}")
        })?;

        // #356: re-sign with the persistent identity when available so the
        // macOS TCC grant survives the update; ad-hoc fallback keeps it runnable.
        #[cfg(target_os = "macos")]
        {
            let _ = crate::core::codesign::sign_binary(current_exe);
        }

        Ok(())
    }
}

/// Try to stop managed lean-ctx processes (proxy, serve, daemon) on Windows
/// before attempting a deferred update.
#[cfg(windows)]
fn stop_managed_windows_processes() {
    // Try `lean-ctx stop` first — it's the cleanest shutdown path.
    let stop_result = std::process::Command::new("lean-ctx").arg("stop").output();

    match stop_result {
        Ok(out) if out.status.success() => {
            eprintln!("  Managed processes stopped.");
        }
        _ => {
            // Fallback: taskkill for known process types (proxy, serve).
            // MCP servers managed by editors can't be killed safely.
            for pattern in &["proxy start", "serve "] {
                let _ = std::process::Command::new("taskkill")
                    .args([
                        "/F",
                        "/FI",
                        &format!("WINDOWTITLE eq *{pattern}*"),
                        "/IM",
                        "lean-ctx.exe",
                    ])
                    .output();
            }
            eprintln!("  Attempted to stop lean-ctx processes via taskkill.");
        }
    }
}

/// Print which lean-ctx.exe processes are blocking the update on Windows.
#[cfg(windows)]
fn print_blocking_processes(target_exe: &std::path::Path) {
    let target_name = target_exe
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("lean-ctx.exe");

    let output = std::process::Command::new("tasklist")
        .args([
            "/FI",
            &format!("IMAGENAME eq {target_name}"),
            "/V",
            "/FO",
            "CSV",
        ])
        .output();

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().skip(1).collect(); // skip CSV header
        if !lines.is_empty() {
            eprintln!("\n  Blocking lean-ctx processes:");
            for line in &lines {
                // CSV: "Image Name","PID","Session Name","Session#","Mem Usage","Status","User Name","CPU Time","Window Title"
                let fields: Vec<&str> = line.split(',').collect();
                if fields.len() >= 2 {
                    let pid = fields[1].trim_matches('"');
                    eprintln!("    PID {pid}");
                }
            }
            eprintln!("\n  To stop manually: taskkill /F /PID <pid>  (or close your editor)");
        }
    }
}

/// On Windows, when the binary is locked by an MCP server, we can't rename it.
/// Instead, stage the new binary and spawn a background cmd process that waits
/// for the lock to be released (with a timeout), then performs the swap.
#[cfg(windows)]
fn deferred_windows_update(
    staged_path: &std::path::Path,
    target_exe: &std::path::Path,
) -> Result<(), String> {
    let target_str = target_exe.display().to_string();
    let staged_str = staged_path.display().to_string();
    let old_str = target_exe.with_extension("old.exe").display().to_string();
    let transaction_str = update_transaction_path()?.display().to_string();
    let lock_str = update_lock_path()?.display().to_string();
    let old_path = target_exe.with_extension("old.exe");
    reject_symlink_if_present(&old_path)?;
    let max_retries = 60;

    let script = generate_deferred_bat_script(
        &target_str,
        &staged_str,
        &old_str,
        &transaction_str,
        &lock_str,
        max_retries,
    );

    let script_path = target_exe.with_file_name("lean-ctx-update.bat");
    reject_symlink_if_present(&script_path)?;
    std::fs::write(&script_path, &script)
        .map_err(|e| format!("Cannot write update script: {e}"))?;

    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "/MIN", &script_path.display().to_string()])
        .spawn();

    println!("\nThe binary is still in use (likely by your editor's MCP server).");
    println!("A background update has been scheduled (timeout: {max_retries}s).");
    println!("Close your editor and the update will complete automatically.");
    println!("\nIf it times out, run: lean-ctx update");
    println!("Update script: {}", script_path.display());

    // The helper runs after this process exits; do not commit a receipt that
    // claims the staged file is active before the helper has completed the swap.
    Err("update deferred until the running Windows binary is released".to_string())
}

fn extract_from_tar_gz(data: &[u8]) -> Result<Vec<u8>, String> {
    use flate2::read::GzDecoder;

    let gz = GzDecoder::new(data);
    let mut archive = tar::Archive::new(gz);

    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if name == "lean-ctx" || name == "lean-ctx.exe" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            return Ok(bytes);
        }
    }
    Err("lean-ctx binary not found inside archive".to_string())
}

fn extract_from_zip(data: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Cursor;

    let cursor = Cursor::new(data);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| e.to_string())?;

    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = file.name().to_string();
        if name == "lean-ctx.exe" || name == "lean-ctx" {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            return Ok(bytes);
        }
    }
    Err("lean-ctx binary not found inside zip archive".to_string())
}

/// Generate the deferred update batch script content (extracted for testability).
#[cfg(any(windows, test))]
fn generate_deferred_bat_script(
    target: &str,
    staged: &str,
    old: &str,
    transaction: &str,
    lock: &str,
    max_retries: u32,
) -> String {
    format!(
        r#"@echo off
setlocal
set "RETRIES=0"
set "MAX_RETRIES={max_retries}"
set "LEANCTX_UPDATE_LOCK={lock}"
set "LEANCTX_UPDATE_TARGET={target}"
set "LEANCTX_UPDATE_STAGED={staged}"
set "LEANCTX_UPDATE_OLD={old}"
set "LEANCTX_UPDATE_TRANSACTION={transaction}"

echo lean-ctx update: waiting for binary to be released (timeout: %MAX_RETRIES%s)...
echo.
echo Blocking processes:
tasklist /FI "IMAGENAME eq lean-ctx.exe" /V /NH 2>nul
echo.
echo Close your editor (Cursor, VS Code, etc.) to release the binary,
echo or stop manually:  lean-ctx stop
echo.

:retry
if %RETRIES% GEQ %MAX_RETRIES% goto timeout
set /a RETRIES+=1
timeout /t 1 /nobreak >nul
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command ^
  "$ErrorActionPreference = 'Stop'; $lock = $null; $targetMoved = $false; $stagedMoved = $false; try {{ $lock = [System.IO.File]::Open($env:LEANCTX_UPDATE_LOCK, [System.IO.FileMode]::OpenOrCreate, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None); Move-Item -LiteralPath $env:LEANCTX_UPDATE_TARGET -Destination $env:LEANCTX_UPDATE_OLD -Force; $targetMoved = $true; Move-Item -LiteralPath $env:LEANCTX_UPDATE_STAGED -Destination $env:LEANCTX_UPDATE_TARGET -Force; $stagedMoved = $true; & $env:LEANCTX_UPDATE_TARGET update --recover-update --lock-held; if ($LASTEXITCODE -ne 0) {{ throw 'receipt recovery failed' }}; if (Test-Path -LiteralPath $env:LEANCTX_UPDATE_OLD) {{ Remove-Item -LiteralPath $env:LEANCTX_UPDATE_OLD -Force -ErrorAction SilentlyContinue }} }} catch {{ if ($stagedMoved -and (Test-Path -LiteralPath $env:LEANCTX_UPDATE_TARGET) -and -not (Test-Path -LiteralPath $env:LEANCTX_UPDATE_STAGED)) {{ Move-Item -LiteralPath $env:LEANCTX_UPDATE_TARGET -Destination $env:LEANCTX_UPDATE_STAGED -Force }}; if ($targetMoved -and (Test-Path -LiteralPath $env:LEANCTX_UPDATE_OLD) -and -not (Test-Path -LiteralPath $env:LEANCTX_UPDATE_TARGET)) {{ Move-Item -LiteralPath $env:LEANCTX_UPDATE_OLD -Destination $env:LEANCTX_UPDATE_TARGET -Force }}; exit 1 }} finally {{ if ($null -ne $lock) {{ $lock.Dispose() }} }}"
if errorlevel 1 (
    if %RETRIES% EQU 10 echo   Still waiting... (%RETRIES%/%MAX_RETRIES%s)
    if %RETRIES% EQU 30 echo   Still waiting... (%RETRIES%/%MAX_RETRIES%s) — try closing your editor
    if %RETRIES% EQU 50 echo   Still waiting... (%RETRIES%/%MAX_RETRIES%s) — timeout approaching
    goto retry
)

echo.
echo Updated successfully!
goto cleanup

:timeout
echo.
echo Update timed out after %MAX_RETRIES% seconds.
echo The new binary is staged at: {staged}
echo.
echo To complete the update manually:
echo   1. Close your editor (Cursor, VS Code, etc.)
echo   2. Run: lean-ctx update
echo.
echo Or run: lean-ctx update --force
echo.
pause
exit /b 1

:cleanup
del "%~f0" >nul 2>&1
"#
    )
}

#[cfg(test)]
#[path = "updater/tests.rs"]
mod tests;
