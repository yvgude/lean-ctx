//! Contained, immutable storage for local Engine Interface artifacts.

use std::path::Path;

#[cfg(test)]
use std::cell::Cell;

use sha2::{Digest, Sha256};

use crate::core::data_dir;

const ARTIFACT_BOUNDARY_REJECTED: &str = "engine_artifact_boundary_rejected";
const ARTIFACT_BOUNDARY_UNSUPPORTED: &str = "engine_artifact_boundary_unsupported";
const ARTIFACT_DIRECTORY_OPEN_FAILED: &str = "engine_artifact_directory_open_failed";
#[cfg(unix)]
const ARTIFACT_DIRECTORY_CREATE_FAILED: &str = "engine_artifact_directory_create_failed";
const ARTIFACT_TEMP_CREATE_FAILED: &str = "engine_artifact_temp_create_failed";
const ARTIFACT_WRITE_FAILED: &str = "engine_artifact_write_failed";
#[cfg(unix)]
const ARTIFACT_PERMISSIONS_FAILED: &str = "engine_artifact_permissions_failed";
const ARTIFACT_SYNC_FAILED: &str = "engine_artifact_sync_failed";
const ARTIFACT_PUBLISH_FAILED: &str = "engine_artifact_publish_failed";
const ARTIFACT_PUBLISH_UNSUPPORTED: &str = "engine_artifact_publish_unsupported";
const ARTIFACT_LEAF_UNTRUSTED: &str = "engine_artifact_leaf_untrusted";
const ARTIFACT_DIGEST_MISMATCH: &str = "engine_artifact_digest_mismatch";
const ARTIFACT_CLEANUP_FAILED: &str = "engine_artifact_cleanup_failed";
const ARTIFACT_SIZE_LIMIT_EXCEEDED: &str = "engine_artifact_size_limit_exceeded";

#[cfg(test)]
thread_local! {
    static TEST_FAIL_BEFORE_PUBLISH: Cell<bool> = const { Cell::new(false) };
    static TEST_FAIL_CAPABILITY_PREFLIGHT: Cell<bool> = const { Cell::new(false) };
    #[cfg(windows)]
    static TEST_FAIL_TEMP_VALIDATION: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
pub(super) fn inject_test_pre_publish_failure() {
    TEST_FAIL_BEFORE_PUBLISH.with(|failpoint| failpoint.set(true));
}

#[cfg(test)]
#[allow(dead_code)]
pub(super) fn inject_test_capability_preflight_failure() {
    TEST_FAIL_CAPABILITY_PREFLIGHT.with(|failpoint| failpoint.set(true));
}

#[cfg(all(test, windows))]
pub(super) fn inject_test_temp_validation_failure() {
    TEST_FAIL_TEMP_VALIDATION.with(|failpoint| failpoint.set(true));
}

fn test_pre_publish_failure() -> bool {
    #[cfg(test)]
    {
        TEST_FAIL_BEFORE_PUBLISH.with(|failpoint| failpoint.replace(false))
    }
    #[cfg(not(test))]
    {
        false
    }
}

fn test_capability_preflight_failure() -> bool {
    #[cfg(test)]
    {
        TEST_FAIL_CAPABILITY_PREFLIGHT.with(|failpoint| failpoint.replace(false))
    }
    #[cfg(not(test))]
    {
        false
    }
}

#[cfg(windows)]
fn test_temp_validation_failure() -> bool {
    #[cfg(test)]
    {
        TEST_FAIL_TEMP_VALIDATION.with(|failpoint| failpoint.replace(false))
    }
    #[cfg(not(test))]
    {
        false
    }
}

pub(super) fn persist_content(
    directory: &str,
    digest: &str,
    extension: &str,
    bytes: &[u8],
) -> Result<std::fs::File, String> {
    persist_content_checked(directory, digest, extension, bytes, None, None)
}

/// Read one integrity-addressed artifact through descriptor/handle-relative,
/// no-follow traversal of the configured Engine data root.
pub(super) fn read_content(
    directory: &str,
    digest: &str,
    extension: &str,
) -> Result<Vec<u8>, String> {
    validate_artifact_name(digest, extension)?;
    let configured_root =
        data_dir::lean_ctx_data_dir().map_err(|_| ARTIFACT_BOUNDARY_REJECTED.to_owned())?;
    let bytes = read_bounded_content(&configured_root, directory, digest, extension, 1024 * 1024)?;
    if hex_sha256(&bytes) != digest {
        return Err(ARTIFACT_DIGEST_MISMATCH.to_owned());
    }
    Ok(bytes)
}

pub(super) fn read_bounded_content(
    configured_root: &Path,
    directory: &str,
    digest: &str,
    extension: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    validate_artifact_name(digest, extension)?;
    if max_bytes == 0 {
        return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
    }

    #[cfg(unix)]
    {
        unix::read_bounded_content(configured_root, directory, digest, extension, max_bytes)
    }

    #[cfg(windows)]
    {
        windows::read_bounded_content(configured_root, directory, digest, extension, max_bytes)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (configured_root, directory, digest, extension, max_bytes);
        Err(ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())
    }
}

#[cfg(all(test, any(unix, windows)))]
pub(super) fn persist_content_with_test_barrier(
    directory: &str,
    digest: &str,
    extension: &str,
    bytes: &[u8],
    barrier: Box<dyn FnOnce()>,
) -> Result<std::fs::File, String> {
    persist_content_checked(directory, digest, extension, bytes, Some(barrier), None)
}

#[cfg(all(test, unix))]
pub(super) fn persist_content_with_test_publish_barrier(
    directory: &str,
    digest: &str,
    extension: &str,
    bytes: &[u8],
    barrier: Box<dyn FnOnce()>,
) -> Result<std::fs::File, String> {
    persist_content_checked(directory, digest, extension, bytes, None, Some(barrier))
}

fn persist_content_checked(
    directory: &str,
    digest: &str,
    extension: &str,
    bytes: &[u8],
    bind_barrier: Option<Box<dyn FnOnce()>>,
    publish_barrier: Option<Box<dyn FnOnce()>>,
) -> Result<std::fs::File, String> {
    validate_artifact_name(digest, extension)?;
    if hex_sha256(bytes) != digest {
        return Err(ARTIFACT_DIGEST_MISMATCH.to_owned());
    }

    let configured_root =
        data_dir::lean_ctx_data_dir().map_err(|_| ARTIFACT_BOUNDARY_REJECTED.to_owned())?;

    #[cfg(unix)]
    {
        unix::persist_content(
            &configured_root,
            directory,
            digest,
            extension,
            bytes,
            bind_barrier,
            publish_barrier,
        )
    }

    #[cfg(windows)]
    {
        windows::persist_content(
            &configured_root,
            directory,
            digest,
            extension,
            bytes,
            bind_barrier,
            publish_barrier,
        )
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (
            configured_root,
            directory,
            digest,
            extension,
            bytes,
            bind_barrier,
            publish_barrier,
        );
        Err(ARTIFACT_BOUNDARY_UNSUPPORTED.to_owned())
    }
}

/// Open (creating if absent) the mutable `<digest>.lock` sidecar beside an
/// artifact directory, through the same handle-relative, no-follow traversal
/// as artifact publication. Windows byte-range locks are mandatory, so a lock
/// taken on the immutable artifact itself would block the next opener from
/// verifying its bytes; the sidecar carries only the lock.
#[cfg(windows)]
pub(super) fn open_lock(directory: &str, digest: &str) -> Result<std::fs::File, String> {
    validate_artifact_name(digest, "json")?;
    let configured_root =
        data_dir::lean_ctx_data_dir().map_err(|_| ARTIFACT_BOUNDARY_REJECTED.to_owned())?;
    windows::open_lock(&configured_root, directory, &format!("{digest}.lock"))
}

fn validate_artifact_name(digest: &str, extension: &str) -> Result<(), String> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
    }
    if !matches!(extension, "json" | "txt") {
        return Err(ARTIFACT_BOUNDARY_REJECTED.to_owned());
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    crate::core::agent_identity::hex_encode(&Sha256::digest(bytes))
}

#[cfg(unix)]
#[allow(clippy::wildcard_imports)]
mod unix;

#[cfg(windows)]
mod windows;
