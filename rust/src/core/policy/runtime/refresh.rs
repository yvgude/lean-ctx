// SPDX-License-Identifier: Apache-2.0
//! Serialize current-byte policy snapshots under one bounded worker. A stalled
//! filesystem cannot create an unbounded set of timed-out reader threads.

use super::{ActivePolicy, PROJECT_PACK_PATH};
use crate::core::policy::{files, org, parse, resolve};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

type PolicyResult = Result<Option<Arc<ActivePolicy>>, String>;
type VerifiedResult = Result<VerifiedPolicy, String>;
const DEADLINE: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub(super) struct VerifiedPolicy {
    pub(super) policy: Option<Arc<ActivePolicy>>,
    pub(super) fingerprint: blake3::Hash,
}

struct Request {
    root: PathBuf,
    deadline: Instant,
    reply: mpsc::Sender<VerifiedResult>,
}

fn worker() -> Option<&'static mpsc::SyncSender<Request>> {
    static WORKER: OnceLock<Option<mpsc::SyncSender<Request>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel::<Request>(16);
            std::thread::Builder::new()
                .name("policy-refresh".into())
                .spawn(move || {
                    let mut cache = RefreshCache::default();
                    while let Ok(request) = rx.recv() {
                        if Instant::now() >= request.deadline {
                            continue;
                        }
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            cache.refresh_verified(&request.root)
                        }))
                        .unwrap_or_else(|_| Err("policy refresh failed".into()));
                        let result = if Instant::now() < request.deadline {
                            result
                        } else {
                            Err("policy refresh deadline exceeded".into())
                        };
                        let _ = request.reply.send(result);
                    }
                })
                .ok()?;
            Some(tx)
        })
        .as_ref()
}

pub(super) fn current(root: PathBuf) -> PolicyResult {
    current_verified(root).map(|verified| verified.policy)
}

pub(super) fn current_verified(root: PathBuf) -> VerifiedResult {
    let deadline = Instant::now() + DEADLINE;
    let (tx, rx) = mpsc::channel();
    worker()
        .ok_or("policy refresh worker unavailable")?
        .try_send(Request {
            root,
            deadline,
            reply: tx,
        })
        .map_err(|_| "policy refresh is unavailable or busy")?;
    let result = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| "policy refresh deadline exceeded")?;
    if Instant::now() >= deadline {
        return Err("policy refresh deadline exceeded".into());
    }
    result
}

struct Snapshot {
    fingerprint: blake3::Hash,
    local: Option<String>,
    organization: Option<String>,
    trust: Option<String>,
}

impl Snapshot {
    fn capture(project: &Path) -> Result<Self, String> {
        let required_root = std::env::var_os(super::REQUIRED_POLICY_ROOT_ENV).map(PathBuf::from);
        let snapshot = Self::capture_for(project, required_root.as_deref())?;
        let expected = std::env::var_os(super::REQUIRED_POLICY_DIGEST_ENV);
        verify_launch_digest(
            snapshot.fingerprint,
            required_root.is_some(),
            expected.as_deref().and_then(std::ffi::OsStr::to_str),
        )?;
        Ok(snapshot)
    }

    fn capture_for(project: &Path, required_root: Option<&Path>) -> Result<Self, String> {
        let root = canonical_project_root(project)?;
        let required_root = required_root
            .map(|required| {
                if required.is_absolute() {
                    canonical_project_root(required)
                } else {
                    Ok(required.to_path_buf())
                }
            })
            .transpose()?;
        let mut hash = blake3::Hasher::new();
        field(&mut hash, Some(root.as_os_str().as_encoded_bytes()));
        let local = local_snapshot(&root, required_root.as_deref(), &mut hash)?;
        if required_root.is_some() {
            protected_authority(&root, &mut hash)?;
        }
        let (org_path, org_required) = org::store::selected_source()?;
        let trust_path = org::trust::trust_path_read_only()?;
        field(&mut hash, Some(org_path.as_os_str().as_encoded_bytes()));
        field(&mut hash, Some(trust_path.as_os_str().as_encoded_bytes()));
        let read = if required_root.is_some() {
            files::read_protected
        } else {
            files::read
        };
        let organization = read(&org_path, org_required)?;
        let trust = read(&trust_path, false)?;
        field(&mut hash, organization.as_deref().map(str::as_bytes));
        field(&mut hash, trust.as_deref().map(str::as_bytes));
        Ok(Self {
            fingerprint: hash.finalize(),
            local,
            organization,
            trust,
        })
    }

    fn compile(&self) -> PolicyResult {
        let local = self
            .local
            .as_deref()
            .map(|text| parse(text).and_then(|pack| resolve(&pack)))
            .transpose()
            .map_err(|_| "project policy is invalid")?;
        let trust: org::TrustStore = match self.trust.as_deref() {
            Some(text) => toml::from_str(text).map_err(|_| "org trust input is invalid")?,
            None => org::TrustStore::default(),
        };
        let organization = match self.organization.as_deref() {
            Some(text) => {
                let artifact =
                    org::OrgPolicyV1::from_json(text).map_err(|_| "org policy input is invalid")?;
                org::resolve_checked(&artifact, &trust)
                    .map_err(|_| "org policy verification failed")?
            }
            None if !trust.keys.is_empty() => {
                return Err("configured organization has no signed policy".into());
            }
            None => None,
        };
        let policy = super::apply_org_floor(local, Ok(organization));
        if policy.as_ref().is_some_and(|policy| !policy.content_valid) {
            return Err("policy compilation failed".into());
        }
        Ok(policy)
    }
}

/// The launcher and admission worker use exactly the same bounded snapshot.
/// A deliberate authority update requires a new protected launch; changing
/// files within the old session never silently grants the new authority.
pub(crate) fn protected_policy_digest(project: &Path) -> Result<String, String> {
    let root = canonical_project_root(project)?;
    let snapshot = Snapshot::capture_for(&root, Some(&root))?;
    snapshot.compile()?.ok_or("required policy is missing")?;
    Ok(snapshot.fingerprint.to_hex().to_string())
}

/// Resolve policy roots through the security canonicalizer so project roots,
/// required-root bindings, and Engine boundaries share the same Windows form.
/// `canonicalize_secure` strips `\\?\` on output; retrying its stripped input
/// handles callers that supplied a verbatim root that a host API rejects.
fn canonical_project_root(path: &Path) -> Result<PathBuf, String> {
    let canonical = crate::core::pathutil::canonicalize_secure(path)
        .or_else(|error| {
            let simplified = crate::core::pathutil::strip_verbatim(path.to_path_buf());
            if simplified.as_path() == path {
                Err(error)
            } else {
                crate::core::pathutil::canonicalize_secure(&simplified)
            }
        })
        .map_err(|_| "policy project root unavailable")?;
    if !canonical.is_dir() {
        return Err("policy project root is not a directory".into());
    }
    Ok(canonical)
}

fn verify_launch_digest(
    actual: blake3::Hash,
    protected: bool,
    expected: Option<&str>,
) -> Result<(), String> {
    if !protected && expected.is_none() {
        return Ok(());
    }
    let expected = expected
        .and_then(|value| blake3::Hash::from_hex(value).ok())
        .ok_or("protected policy launch digest is missing or invalid")?;
    if !protected || actual != expected {
        return Err("protected policy authority changed; restart codex-protected after reviewing the changes".into());
    }
    Ok(())
}

fn protected_authority(root: &Path, hash: &mut blake3::Hasher) -> Result<(), String> {
    field(hash, Some(b"protected-launch-authority-v1"));
    root_identity(root, hash)?;
    for path in protected_config_paths(root)? {
        field(hash, Some(path.as_os_str().as_encoded_bytes()));
        let text = files::read_config(&path)?;
        field(hash, text.as_deref().map(str::as_bytes));
    }
    Ok(())
}

fn protected_config_paths(root: &Path) -> Result<Vec<PathBuf>, String> {
    Ok(vec![
        crate::core::config::Config::path().ok_or("configuration path unavailable")?,
        crate::core::config::Config::local_path(root.to_str().ok_or("policy root is not Unicode")?),
        crate::core::paths::config_dir_read_only()?.join("workspace-trust.toml"),
    ])
}

/// The launcher's kernel write boundary covers the same file authorities as
/// the bounded admission snapshot, including absent optional inputs.
pub(crate) fn protected_authority_paths(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = protected_config_paths(root)?;
    paths.push(root.join(PROJECT_PACK_PATH));
    paths.push(org::store::selected_source()?.0);
    paths.push(org::trust::trust_path_read_only()?);
    Ok(paths)
}

#[cfg(unix)]
fn root_identity(root: &Path, hash: &mut blake3::Hasher) -> Result<(), String> {
    // A replacement directory with the same path and bytes is a new authority.
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(root).map_err(|_| "policy root metadata unavailable")?;
    field(hash, Some(&metadata.dev().to_le_bytes()));
    field(hash, Some(&metadata.ino().to_le_bytes()));
    Ok(())
}

#[cfg(windows)]
fn root_identity(root: &Path, hash: &mut blake3::Hasher) -> Result<(), String> {
    // Windows counterpart of the Unix (dev, ino) pair: volume serial number and
    // file index of the opened directory. A replacement directory at the same
    // path gets a new index, so it is a new authority.
    use std::mem::MaybeUninit;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, GetFileInformationByHandle, OPEN_EXISTING, SYNCHRONIZE,
    };

    let name = root
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    const TRAVERSE_DIRECTORY_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
    // SAFETY: `name` is NUL-terminated and lives through the call; the
    // security-attributes and template-file pointers are null.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            TRAVERSE_DIRECTORY_ACCESS,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err("protected root identity unavailable".into());
    }
    // SAFETY: `handle` is successful and transferred immediately to `File`.
    let _directory = unsafe { std::fs::File::from_raw_handle(handle) };
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: `_directory` owns the live handle and the output pointer is
    // writable for this synchronous Win32 call.
    if unsafe { GetFileInformationByHandle(handle, information.as_mut_ptr()) } == 0 {
        return Err("protected root identity unavailable".into());
    }
    // SAFETY: GetFileInformationByHandle succeeded and initialized the value.
    let information = unsafe { information.assume_init() };
    if information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err("protected root identity unavailable".into());
    }
    let mut identity = [0_u8; 12];
    identity[..4].copy_from_slice(&information.dwVolumeSerialNumber.to_le_bytes());
    identity[4..8].copy_from_slice(&information.nFileIndexHigh.to_le_bytes());
    identity[8..].copy_from_slice(&information.nFileIndexLow.to_le_bytes());
    field(hash, Some(&identity));
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn root_identity(_root: &Path, _hash: &mut blake3::Hasher) -> Result<(), String> {
    Err("protected root identity validation is unavailable on this platform".into())
}

// Explicit protected launchers pin the canonical source root for the lifetime
// of the MCP process. Policy removal must not turn that process into Community
// mode or silently select a weaker ancestor policy on the next request.
fn local_snapshot(
    root: &Path,
    required: Option<&Path>,
    hash: &mut blake3::Hasher,
) -> Result<Option<String>, String> {
    if required.is_some_and(|expected| !expected.is_absolute() || expected != root) {
        return Err("required policy project binding mismatch".into());
    }
    if let Some(required) = required {
        field(hash, Some(b"required-project-policy-v1"));
        field(hash, Some(required.as_os_str().as_encoded_bytes()));
    }
    for ancestor in root.ancestors() {
        let path = ancestor.join(PROJECT_PACK_PATH);
        field(hash, Some(path.as_os_str().as_encoded_bytes()));
        let local = if required.is_some() {
            files::read_protected(&path, true)?
        } else {
            files::read(&path, false)?
        };
        field(hash, local.as_deref().map(str::as_bytes));
        if local.is_some() {
            return Ok(local);
        }
    }
    Ok(None)
}

fn field(hash: &mut blake3::Hasher, bytes: Option<&[u8]>) {
    match bytes {
        Some(bytes) => {
            hash.update(&[1]);
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        None => {
            hash.update(&[0]);
        }
    }
}

#[derive(Default)]
pub(super) struct RefreshCache {
    // A single entry bounds retained compiled state; it never authorizes a
    // request until fresh bytes (including org trust) have matched again.
    compiled: Option<(blake3::Hash, Option<Arc<ActivePolicy>>)>,
}

impl RefreshCache {
    /// Preserve the original cache API for policy callers that need only the
    /// compiled policy; the worker uses `refresh_verified` to retain provenance.
    pub(super) fn refresh(&mut self, root: &Path) -> PolicyResult {
        self.refresh_verified(root).map(|verified| verified.policy)
    }

    pub(super) fn refresh_verified(&mut self, root: &Path) -> VerifiedResult {
        let snapshot = Snapshot::capture(root)?;
        if let Some((fingerprint, compiled)) = &self.compiled
            && *fingerprint == snapshot.fingerprint
        {
            return Ok(VerifiedPolicy {
                policy: compiled.clone(),
                fingerprint: *fingerprint,
            });
        }
        let policy = snapshot.compile()?;
        let fingerprint = snapshot.fingerprint;
        self.compiled = Some((fingerprint, policy.clone()));
        Ok(VerifiedPolicy {
            policy,
            fingerprint,
        })
    }
}

#[cfg(test)]
mod required_policy_tests {
    use super::*;

    fn snapshot(root: &Path, required: Option<&Path>) -> Result<Option<String>, String> {
        local_snapshot(root, required, &mut blake3::Hasher::new())
    }

    #[test]
    fn launch_digest_requires_both_bindings_and_exact_bytes() {
        let expected = blake3::hash(b"original");
        let digest = expected.to_hex().to_string();
        assert!(verify_launch_digest(expected, true, Some(&digest)).is_ok());
        assert!(verify_launch_digest(expected, true, None).is_err());
        assert!(verify_launch_digest(expected, true, Some("invalid")).is_err());
        assert!(verify_launch_digest(expected, false, Some(&digest)).is_err());
        assert!(verify_launch_digest(blake3::hash(b"changed"), true, Some(&digest)).is_err());
        assert!(verify_launch_digest(expected, false, None).is_ok());
    }

    #[test]
    fn launch_snapshot_refuses_changed_authority_but_allows_code_edits() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join(".lean-ctx")).unwrap();
        let policy = root.join(PROJECT_PACK_PATH);
        let original = "name='launch'\nversion='1.0.0'\ndescription='fixture'\n";
        std::fs::write(&policy, original).unwrap();
        let digest = protected_policy_digest(&root).unwrap();
        let check = || {
            let snapshot = Snapshot::capture_for(&root, Some(&root)).unwrap();
            verify_launch_digest(snapshot.fingerprint, true, Some(&digest))
        };
        assert!(check().is_ok());
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        assert!(check().is_ok());
        std::fs::write(&policy, original.replace("launch", "changed")).unwrap();
        assert!(check().is_err());
        std::fs::write(&policy, original).unwrap();
        assert!(check().is_ok());
        std::fs::write(root.join(".lean-ctx.toml"), "shell_security='off'\n").unwrap();
        assert!(check().is_err());
        assert_ne!(protected_policy_digest(&root).unwrap(), digest);
    }

    #[test]
    fn replacement_root_changes_authority_even_with_identical_policy_bytes() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("project");
        let original = "name='launch'\nversion='1.0.0'\ndescription='fixture'\n";
        std::fs::create_dir_all(root.join(".lean-ctx")).unwrap();
        std::fs::write(root.join(PROJECT_PACK_PATH), original).unwrap();
        let first = protected_policy_digest(&root).unwrap();
        std::fs::rename(&root, root.with_file_name("original")).unwrap();
        std::fs::create_dir_all(root.join(".lean-ctx")).unwrap();
        std::fs::write(root.join(PROJECT_PACK_PATH), original).unwrap();
        assert_ne!(protected_policy_digest(&root).unwrap(), first);
    }

    #[test]
    fn mandatory_policy_cannot_fall_back_to_parent_after_removal() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        let root = parent.join("project");
        std::fs::create_dir_all(parent.join(".lean-ctx")).unwrap();
        std::fs::create_dir_all(root.join(".lean-ctx")).unwrap();
        std::fs::write(parent.join(PROJECT_PACK_PATH), "parent policy").unwrap();
        let path = root.join(PROJECT_PACK_PATH);
        std::fs::write(&path, "original policy").unwrap();
        assert_eq!(
            snapshot(&root, Some(&root)).unwrap().as_deref(),
            Some("original policy")
        );
        std::fs::remove_file(&path).unwrap();
        assert!(snapshot(&root, Some(&root)).is_err());
        assert_eq!(
            snapshot(&root, None).unwrap().as_deref(),
            Some("parent policy")
        );
        std::fs::write(&path, "repaired policy").unwrap();
        assert_eq!(
            snapshot(&root, Some(&root)).unwrap().as_deref(),
            Some("repaired policy")
        );
    }

    #[test]
    fn required_policy_rejects_a_different_or_relative_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for required in [root.join("other"), PathBuf::from("."), PathBuf::new()] {
            assert!(snapshot(&root, Some(&required)).is_err());
        }
    }

    #[test]
    fn project_and_required_policy_roots_use_the_same_canonical_form() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let temp = tempfile::tempdir().unwrap();
        let verbatim_root = temp.path().canonicalize().unwrap();
        let required_root = crate::core::pathutil::strip_verbatim(verbatim_root.clone());
        std::fs::create_dir_all(required_root.join(".lean-ctx")).unwrap();
        std::fs::write(required_root.join(PROJECT_PACK_PATH), "same policy").unwrap();

        let result = Snapshot::capture_for(&verbatim_root, Some(&required_root));
        assert!(
            result.is_ok(),
            "equivalent canonical project and required roots must be accepted: {:?}",
            result.as_ref().err()
        );
    }

    #[test]
    fn required_policy_rejects_missing_or_nonregular_input() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        assert!(snapshot(&root, Some(&root)).is_err());
        std::fs::create_dir_all(root.join(PROJECT_PACK_PATH)).unwrap();
        assert!(snapshot(&root, Some(&root)).is_err());
    }

    #[test]
    fn required_policy_is_distinct_in_the_snapshot_fingerprint() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join(".lean-ctx")).unwrap();
        std::fs::write(root.join(PROJECT_PACK_PATH), "same policy").unwrap();
        let mut community = blake3::Hasher::new();
        let mut protected = blake3::Hasher::new();
        assert_eq!(
            local_snapshot(&root, None, &mut community).unwrap(),
            local_snapshot(&root, Some(&root), &mut protected).unwrap()
        );
        assert_ne!(community.finalize(), protected.finalize());
    }
}
