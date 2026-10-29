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
const BUSY: &str = "policy refresh is unavailable or busy";
const DEADLINE_EXCEEDED: &str = "policy refresh deadline exceeded";
/// Pause before the one repeat of a refresh the shared worker could not serve.
const SETTLE: Duration = Duration::from_millis(50);

#[derive(Clone)]
pub(super) struct VerifiedPolicy {
    pub(super) policy: Option<Arc<ActivePolicy>>,
    pub(super) fingerprint: blake3::Hash,
    /// Set when the policy rests on a verifier's decision: whoever holds the
    /// policy past this refresh checks the signed horizon before acting on it.
    pub(super) lifetime: Option<org::Lifetime>,
    /// What the verifier's answer stated, when this very refresh asked it:
    /// from the same answer the policy and the lifetime rest on, and handed
    /// out only together with them. `None` without a verifier decision.
    pub(super) observation: Option<org::Observation>,
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
                            cache.refresh_verified_by(&request.root, request.deadline)
                        }))
                        .unwrap_or_else(|_| Err("policy refresh failed".into()));
                        let result = if Instant::now() < request.deadline {
                            result
                        } else {
                            Err(DEADLINE_EXCEEDED.into())
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
        .map_err(|_| BUSY)?;
    let result = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| DEADLINE_EXCEEDED)?;
    if Instant::now() >= deadline {
        return Err(DEADLINE_EXCEEDED.into());
    }
    #[cfg(test)]
    let result = result.map(|mut verified| {
        ANSWER_HOOK.with(|slot| {
            if let Some(hook) = slot.borrow_mut().as_mut() {
                hook(&mut verified.lifetime);
            }
        });
        verified
    });
    // The last handout: both bounds of a verifier's decision once more, on
    // the receiving side. From here on only the signed horizon applies, and
    // the policy carries that itself.
    if let Ok(verified) = &result
        && let Some(lifetime) = &verified.lifetime
    {
        lifetime.check()?;
    }
    result
}

#[cfg(test)]
type AnswerHook = Box<dyn FnMut(&mut Option<org::Lifetime>)>;

#[cfg(test)]
thread_local! {
    /// Tests only: the lifetime of each verified answer this thread receives,
    /// as the verifier would have stated it, set before the receiving side
    /// checks its bounds. Policy, fingerprint and observation stay as the
    /// bytes made them. Never re-enters `current_verified`.
    static ANSWER_HOOK: std::cell::RefCell<Option<AnswerHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Shapes the lifetime of every answer [`current_verified`] hands out on this
/// thread until the guard drops. Tests only. One hook per thread: arming
/// while one is armed panics instead of replacing it.
#[cfg(test)]
#[must_use]
pub(super) fn answer_hook_for_test(
    hook: impl FnMut(&mut Option<org::Lifetime>) + 'static,
) -> AnswerHookGuard {
    ANSWER_HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(
            slot.is_none(),
            "an answer hook is already armed on this thread"
        );
        *slot = Some(Box::new(hook));
    });
    AnswerHookGuard(std::marker::PhantomData)
}

/// Clears the hook of the thread that armed it; neither `Send` nor `Sync`, so
/// it is dropped on that very thread.
#[cfg(test)]
pub(super) struct AnswerHookGuard(std::marker::PhantomData<std::rc::Rc<()>>);

#[cfg(test)]
impl Drop for AnswerHookGuard {
    fn drop(&mut self) {
        ANSWER_HOOK.with(|slot| slot.borrow_mut().take());
    }
}

/// [`current_verified`], asked once more when the one shared worker was only
/// busy or late — parallel tool calls queue behind it. Ops telemetry
/// (3.11.4): `ctx_compose` discarded finished results ("source authority
/// changed or could not be verified") on a re-check that merely timed out.
/// Fail-closed is unchanged: only a verified snapshot is ever returned, and a
/// policy that is invalid or changed fails on the first answer.
pub(super) fn current_verified_settled(root: &Path) -> VerifiedResult {
    settled(|| current_verified(root.to_path_buf()))
}

/// Run `check`, and once more after [`SETTLE`] when it failed only because
/// the worker was busy or late. Any other answer is final.
pub(super) fn settled(mut check: impl FnMut() -> VerifiedResult) -> VerifiedResult {
    match check() {
        Err(error) if error == BUSY || error == DEADLINE_EXCEEDED => {
            std::thread::sleep(SETTLE);
            check()
        }
        answer => answer,
    }
}

struct Snapshot {
    fingerprint: blake3::Hash,
    /// Everything of the fingerprint before the marker text, which is hashed
    /// last: what the fingerprint would be with another marker is computed
    /// from here, without reading anything again.
    before_marker: blake3::Hasher,
    local: Option<String>,
    organization: Option<String>,
    trust: Option<String>,
    /// The installed Hive filter set, as one captured text.
    hive_filters: Option<String>,
    /// The Hive effective policy (2.x): floor, requirement and the vault
    /// whose entry applies to this project ([`crate::core::hive::effective_floor`]).
    hive_policy: HivePolicy,
    /// The required-organization marker. Part of the fingerprint: a marker
    /// change is a new snapshot, so no compiled policy outlives it.
    marker: Option<String>,
}

/// See [`org::is_v2`]: the admission snapshot and the proxy's captured
/// authority must agree on what a version-two artifact is.
fn is_org_policy_v2(text: &str) -> bool {
    org::is_v2(text)
}

impl Snapshot {
    /// A version-two organization policy is valid only as currently verified:
    /// its permit depends on time, on the verifier and on current
    /// authorization, none of which is in the snapshot bytes. Such a snapshot
    /// is compiled on every refresh and never answered from the cache.
    fn needs_current_verification(&self) -> bool {
        self.organization.as_deref().is_some_and(is_org_policy_v2)
    }

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
        // The artifact alone is read at its own, larger bound; the same
        // checks apply, and what is not version two stays at the usual one.
        let organization =
            files::read_org_artifact(&org_path, org_required, required_root.is_some())?;
        let trust = read(&trust_path, false)?;
        field(&mut hash, organization.as_deref().map(str::as_bytes));
        field(&mut hash, trust.as_deref().map(str::as_bytes));
        // The Hive filter set is written atomically as one file; a changed
        // set is a new snapshot, in every process that reads it.
        let filters_path = crate::core::hive::policy::filter_set_path();
        field(
            &mut hash,
            filters_path
                .as_ref()
                .map(|path| path.as_os_str().as_encoded_bytes()),
        );
        let hive_filters = match &filters_path {
            Some(path) => read(path, false)?,
            None => None,
        };
        field(&mut hash, hive_filters.as_deref().map(str::as_bytes));
        let hive_policy = HivePolicy::capture(&root, read, &mut hash)?;
        let marker_path = org::marker::marker_path_read_only()?;
        field(&mut hash, Some(marker_path.as_os_str().as_encoded_bytes()));
        let marker = read(&marker_path, false)?;
        let before_marker = hash.clone();
        field(&mut hash, marker.as_deref().map(str::as_bytes));
        Ok(Self {
            fingerprint: hash.finalize(),
            before_marker,
            local,
            organization,
            trust,
            hive_filters,
            hive_policy,
            marker,
        })
    }

    /// The compiled policy and the fingerprint it is valid under.
    ///
    /// That is this snapshot's own fingerprint unless the verification itself
    /// appended to the marker: then it is the fingerprint of the same captured
    /// inputs with exactly that new marker text. A caller comparing against a
    /// later snapshot thus accepts its own recorded decision and still
    /// refuses every other change to artifact, trust store or marker.
    fn compile(&self, deadline: Instant) -> Result<Compiled, String> {
        let mut fingerprint = self.fingerprint;
        let mut lifetime = None;
        let mut observation = None;
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
        // A damaged marker is an error, never "no requirement".
        let required = self
            .marker
            .as_deref()
            .map(org::marker::parse)
            .transpose()?
            .is_some_and(|marker| marker.is_required());
        let organization = match self.organization.as_deref() {
            Some(text) if is_org_policy_v2(text) => {
                // Version two is decided by the authenticated verifier, for
                // this refresh only. Whatever goes wrong, the policy is not
                // established: deny, never fall back to version-one rules or
                // to the local policy alone.
                let verified = org::verify_v2(
                    text,
                    self.trust.as_deref(),
                    self.marker.as_deref(),
                    deadline,
                )?;
                // The packs the verified policy requires must be the ones
                // installed, from this same snapshot (HIVE-RT-D5). A lifted
                // requirement (no floor) requires nothing.
                if verified.floor.is_some() {
                    crate::core::hive::policy::required_packs_installed(
                        text,
                        self.hive_filters.as_deref(),
                    )?;
                }
                if self.marker.as_deref() != Some(verified.marker.as_str()) {
                    let mut hash = self.before_marker.clone();
                    field(&mut hash, Some(verified.marker.as_bytes()));
                    fingerprint = hash.finalize();
                }
                lifetime = Some(verified.lifetime);
                // Kept whether or not there is a floor: a lifted requirement
                // is an observed organization decision too.
                observation = Some(verified.observation);
                verified.floor
            }
            Some(text) if text.len() as u64 > files::MAX_BYTES => {
                // Only a version-two artifact may be this large.
                return Err("policy input exceeds byte limit".into());
            }
            Some(_) if required => {
                // Under a recorded requirement only a verified version-two
                // policy counts; a version-one artifact is as good as absent.
                return Err("required organization policy is not verifiable".into());
            }
            Some(text) => {
                let artifact =
                    org::OrgPolicyV1::from_json(text).map_err(|_| "org policy input is invalid")?;
                org::resolve_checked(&artifact, &trust)
                    .map_err(|_| "org policy verification failed")?
            }
            None if required => {
                // Artifact gone (with or without the trust store): the marker
                // still says an organization policy is required.
                return Err("required organization policy is missing".into());
            }
            None if !trust.keys.is_empty() => {
                return Err("configured organization has no signed policy".into());
            }
            None => None,
        };
        // The compiled policy carries the signed end of its floor, so that a
        // policy kept beyond this refresh ends with it.
        let hive = crate::core::hive::policy::filter_rules(self.hive_filters.as_deref());
        if hive.dropped > 0 {
            tracing::warn!(
                dropped = hive.dropped,
                "hive filter set: rules that did not validate are not applied"
            );
        }
        // A missing or damaged Hive policy in force fails the refresh
        // closed; it never means "no Hive rules".
        let hive_floor = self.hive_policy.resolve()?;
        let policy = super::apply_org_floor(
            local,
            Ok(organization),
            lifetime.as_ref().and_then(org::Lifetime::horizon),
            &hive,
            hive_floor.as_ref(),
        );
        if policy.as_ref().is_some_and(|policy| !policy.content_valid) {
            return Err("policy compilation failed".into());
        }
        Ok(Compiled {
            policy,
            fingerprint,
            lifetime,
            observation,
        })
    }
}

struct Compiled {
    policy: Option<Arc<ActivePolicy>>,
    fingerprint: blake3::Hash,
    lifetime: Option<org::Lifetime>,
    observation: Option<org::Observation>,
}

/// The captured Hive effective policy of one snapshot.
#[derive(Default)]
struct HivePolicy {
    floor: Option<String>,
    required: Option<String>,
    vault: Option<String>,
}

impl HivePolicy {
    /// Read the installed floor and requirement (and, for a floor with
    /// per-vault entries, the vault linked to `root`). Hashed only when one
    /// of them exists, so a snapshot without a Hive policy keeps the
    /// fingerprint it had before Release B.
    fn capture(
        root: &Path,
        read: fn(&Path, bool) -> Result<Option<String>, String>,
        hash: &mut blake3::Hasher,
    ) -> Result<Self, String> {
        use crate::core::hive::effective_floor::{FLOOR_FILE, REQUIRED_FILE, floor_dir, vault_for};
        let Some(dir) = floor_dir() else {
            return Ok(Self::default());
        };
        let floor = read(&dir.join(FLOOR_FILE), false)?;
        let required = read(&dir.join(REQUIRED_FILE), false)?;
        if floor.is_none() && required.is_none() {
            return Ok(Self::default());
        }
        let vault = floor.as_deref().and_then(|text| {
            let hive_data = dir.parent()?.parent()?;
            vault_for(text, hive_data, root)
        });
        field(hash, Some(b"hive-effective-policy-v1"));
        field(hash, Some(dir.as_os_str().as_encoded_bytes()));
        field(hash, floor.as_deref().map(str::as_bytes));
        field(hash, required.as_deref().map(str::as_bytes));
        field(hash, vault.as_deref().map(str::as_bytes));
        Ok(Self {
            floor,
            required,
            vault,
        })
    }

    fn resolve(&self) -> Result<Option<super::ResolvedPolicy>, String> {
        crate::core::hive::effective_floor::resolve(
            self.floor.as_deref(),
            self.required.as_deref(),
            self.vault.as_deref(),
        )
    }
}

/// The launcher and admission worker use exactly the same bounded snapshot.
/// A deliberate authority update requires a new protected launch; changing
/// files within the old session never silently grants the new authority.
pub(crate) fn protected_policy_digest(project: &Path) -> Result<String, String> {
    let root = canonical_project_root(project)?;
    let snapshot = Snapshot::capture_for(&root, Some(&root))?;
    // The launch digest is the authority as captured: a marker this very
    // verification appended is a new authority and needs a new launch.
    let compiled = snapshot.compile(Instant::now() + DEADLINE)?;
    compiled.policy.ok_or("required policy is missing")?;
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
    paths.push(org::marker::marker_path_read_only()?);
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
    /// The compiled policy only (tests drive the cache through this).
    #[cfg(test)]
    pub(super) fn refresh(&mut self, root: &Path) -> PolicyResult {
        self.refresh_verified_by(root, Instant::now() + DEADLINE)
            .map(|verified| verified.policy)
    }

    /// One refresh under the request's own monotonic bound: a verification
    /// it needs gets only the time left and records nothing at or after it.
    fn refresh_verified_by(&mut self, root: &Path, deadline: Instant) -> VerifiedResult {
        let snapshot = Snapshot::capture(root)?;
        // Identical bytes prove nothing about a policy whose validity depends
        // on the present moment: such a snapshot is never served from here.
        if !snapshot.needs_current_verification()
            && let Some((fingerprint, compiled)) = &self.compiled
            && *fingerprint == snapshot.fingerprint
        {
            return Ok(VerifiedPolicy {
                policy: compiled.clone(),
                fingerprint: *fingerprint,
                lifetime: None,
                observation: None,
            });
        }
        let Compiled {
            policy,
            fingerprint,
            lifetime,
            observation,
        } = snapshot.compile(deadline)?;
        // A verification waited for an answer. Before its result is handed
        // out, everything is read again and must be the captured authority
        // with at most the marker this decision itself left stored: any other
        // change to project policy, artifact, trust store or marker refuses.
        if snapshot.needs_current_verification()
            && Snapshot::capture(root)?.fingerprint != fingerprint
        {
            return Err("policy authority changed during verification".into());
        }
        // That read took time too: the signed horizon and the request's
        // deadline are checked once more, last, whether the decision
        // enforces a policy or lifts the requirement.
        if let Some(lifetime) = &lifetime {
            lifetime.check()?;
        }
        self.compiled = Some((fingerprint, policy.clone()));
        // After the recapture and both bounds: the observation leaves with
        // the policy it belongs to, or not at all.
        Ok(VerifiedPolicy {
            policy,
            fingerprint,
            lifetime,
            observation,
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
