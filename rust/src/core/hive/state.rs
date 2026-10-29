// SPDX-License-Identifier: Apache-2.0
//! Durable Hive state on this machine.
//!
//! `<config_dir>/hive/` (directory 0700, files 0600, written atomically):
//! `instance.key`, `kx.key`, `instance.json`, `lease.json` (re-verified on
//! every load), `keys.json`, `token.json` (never logged).
//! `<data_dir>/hive/`: `activity.json` and, with later packages, project
//! maps, cursors and the outbox.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::HiveError;

pub(crate) const INSTANCE_KEY_FILE: &str = "instance.key";
pub(crate) const KX_KEY_FILE: &str = "kx.key";
pub(crate) const INSTANCE_FILE: &str = "instance.json";
pub(crate) const LEASE_FILE: &str = "lease.json";
pub(crate) const KEYS_FILE: &str = "keys.json";
pub(crate) const TOKEN_FILE: &str = "token.json";
pub(crate) const ACTIVITY_FILE: &str = "activity.json";
/// The person's sharing answer at connect (`super::connect`).
pub(crate) const SHARING_FILE: &str = "sharing.json";
/// When the enrolled Hive last answered that it has no match route
/// (`super::mapping`).
pub(crate) const MATCH_FILE: &str = "vault-match.json";

/// The enrolled instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceRecord {
    pub instance_id: String,
    pub org_id: String,
    pub org_name: String,
    pub label: String,
    /// Origin the instance is enrolled with.
    pub url: String,
    pub org_salt_id: String,
    pub enrolled_at: String,
    /// Origin of the Hive web app, when enrollment named one (1.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url: Option<String>,
    /// `personal` | `team`, when enrollment named it (Hive C line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_kind: Option<String>,
    /// The company the team belongs to, when enrollment named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_name: Option<String>,
}

impl InstanceRecord {
    /// Where the instance is enrolled, as people read it (owner decision
    /// 26.10.: Personal, Team, Company): `Company Acme · Team Platform`,
    /// `Team Platform`, `Personal (Ada)`; an older server's record keeps
    /// the plain name.
    pub fn workspace_label(&self) -> String {
        let name = &self.org_name;
        match (self.company_name.as_deref(), self.org_kind.as_deref()) {
            (Some(company), _) => format!("Company {company} · Team {name}"),
            (None, Some("team")) => format!("Team {name}"),
            (None, Some("personal")) => format!("Personal ({name})"),
            _ => name.clone(),
        }
    }
}

/// The last verified lease, as received, plus the service clock offset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaseRecord {
    pub envelope: Value,
    /// `server_time - local time` when the lease arrived, in seconds.
    pub offset_seconds: i64,
    pub received_at: String,
}

/// A cached access token. `Debug` never shows the token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRecord {
    pub access_token: String,
    /// Unix seconds (service clock).
    pub expires_at: i64,
}

impl fmt::Debug for TokenRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenRecord")
            .field("access_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// When Hive last answered, per channel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityRecord {
    pub last_seen: Option<String>,
    #[serde(default)]
    pub last_success: BTreeMap<String, String>,
    pub last_error: Option<String>,
    /// Why Hive last refused this workspace as locked (`trial_ended`,
    /// `plan_ended`); cleared by the next lease Hive issues.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<String>,
}

/// Paths of the Hive state; [`HiveStore::open`] for this machine,
/// [`HiveStore::at`] for tests and embedders.
#[derive(Debug, Clone)]
pub struct HiveStore {
    config: PathBuf,
    data: PathBuf,
}

impl HiveStore {
    /// `<config_dir>/hive` and `<data_dir>/hive` of this installation. Opening
    /// only resolves the paths: a read-only config directory keeps its mode (the
    /// Hive's own directories are made private when something is written there).
    pub fn open() -> Result<Self, HiveError> {
        let config = crate::core::paths::config_dir_read_only().map_err(HiveError::Local)?;
        let data = crate::core::paths::data_dir().map_err(HiveError::Local)?;
        Ok(Self::at(config.join("hive"), data.join("hive")))
    }

    /// State rooted at explicit directories.
    pub fn at(config_dir: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            config: config_dir.into(),
            data: data_dir.into(),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    pub(crate) fn read_config_bytes(&self, name: &str) -> Result<Option<Vec<u8>>, HiveError> {
        read_optional(&self.config.join(name))
    }

    pub(crate) fn write_config_bytes(&self, name: &str, bytes: &[u8]) -> Result<(), HiveError> {
        write_private(&self.config, name, bytes)
    }

    pub(crate) fn read_config<T: DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<Option<T>, HiveError> {
        read_json(&self.config.join(name))
    }

    pub(crate) fn write_config<T: Serialize>(
        &self,
        name: &str,
        value: &T,
    ) -> Result<(), HiveError> {
        write_private(&self.config, name, &to_json(value)?)
    }

    pub(crate) fn remove_config(&self, name: &str) -> Result<(), HiveError> {
        remove_optional(&self.config.join(name))
    }

    pub fn instance(&self) -> Result<Option<InstanceRecord>, HiveError> {
        self.read_config(INSTANCE_FILE)
    }

    pub fn activity(&self) -> ActivityRecord {
        read_json(&self.data.join(ACTIVITY_FILE))
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Record a successful exchange on `channel` at `when` (RFC 3339).
    pub(crate) fn record_success(&self, channel: &str, when: &str) {
        let mut activity = self.activity();
        activity.last_seen = Some(when.to_string());
        activity
            .last_success
            .insert(channel.to_string(), when.to_string());
        activity.last_error = None;
        // Activity is informational; failing to record it never fails the exchange.
        if let Ok(bytes) = to_json(&activity) {
            let _ = write_private(&self.data, ACTIVITY_FILE, &bytes);
        }
    }

    /// Record (`Some`) or clear (`None`) a locked workspace (E9).
    pub(crate) fn record_locked(&self, reason: Option<&str>) {
        let mut activity = self.activity();
        if activity.locked.as_deref() == reason {
            return;
        }
        activity.locked = reason.map(str::to_string);
        if let Ok(bytes) = to_json(&activity) {
            let _ = write_private(&self.data, ACTIVITY_FILE, &bytes);
        }
    }

    pub(crate) fn record_error(&self, message: &str) {
        let mut activity = self.activity();
        activity.last_error = Some(message.to_string());
        if let Ok(bytes) = to_json(&activity) {
            let _ = write_private(&self.data, ACTIVITY_FILE, &bytes);
        }
    }

    /// Whether this engine may record anything for Hive now: connected (an
    /// instance record) and not revoked. Two file checks; the recording
    /// paths call it on every record (K2 review 2, N1).
    pub(crate) fn may_record(&self) -> bool {
        self.config.join(INSTANCE_FILE).is_file()
            && !self.config.join(super::revoked::REVOKED_FILE).is_file()
    }

    /// The slower half of the recording check (K2 review 3, NEW-3): the
    /// `[hive]` switches allow engine metrics and the stored lease has not
    /// lapsed (read from the local copy; a lease that cannot be read counts
    /// as lapsed).
    pub(crate) fn recording_allowed(&self, config: &super::config::HiveConfig) -> bool {
        config.enabled != Some(false) && config.metrics_upload && self.lease_live()
    }

    fn lease_live(&self) -> bool {
        use lean_ctx_protocol::hive::lease::{HiveLeaseV1, LeaseState};
        let Ok(Some(record)) = self.read_config::<LeaseRecord>(LEASE_FILE) else {
            return false;
        };
        let Ok(lease) = serde_json::from_value::<HiveLeaseV1>(record.envelope["lease"].clone())
        else {
            return false;
        };
        let service_now =
            (chrono::Utc::now() + chrono::Duration::seconds(record.offset_seconds)).fixed_offset();
        matches!(
            lease.state_at(service_now),
            Ok(LeaseState::Valid | LeaseState::Grace)
        )
    }

    /// Remove credentials, lease and keys; with `keep_local` the data
    /// directory (pulled knowledge, outbox, activity) stays, but nothing
    /// keeps recording for Hive (metrics, signals, receipts).
    pub fn forget(&self, keep_local: bool) -> Result<(), HiveError> {
        super::receipts::stop_local_recording(&self.data);
        for name in [
            TOKEN_FILE,
            LEASE_FILE,
            KEYS_FILE,
            INSTANCE_FILE,
            KX_KEY_FILE,
            INSTANCE_KEY_FILE,
            SHARING_FILE,
            MATCH_FILE,
            super::revoked::REVOKED_FILE,
            super::disconnect::PENDING_FILE,
        ] {
            self.remove_config(name)?;
        }
        if !keep_local && self.data.exists() {
            std::fs::remove_dir_all(&self.data)
                .map_err(|e| HiveError::Local(format!("remove {}: {e}", self.data.display())))?;
        }
        Ok(())
    }
}

fn to_json<T: Serialize>(value: &T) -> Result<Vec<u8>, HiveError> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|e| HiveError::Local(format!("encode: {e}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, HiveError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(HiveError::Local(format!("read {}: {e}", path.display()))),
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, HiveError> {
    let Some(bytes) = read_optional(path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| HiveError::Local(format!("{} is damaged: {e}", path.display())))
}

fn remove_optional(path: &Path) -> Result<(), HiveError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(HiveError::Local(format!("remove {}: {e}", path.display()))),
    }
}

/// Write `dir/name` so that it is never readable by others and never seen
/// half-written: a 0700 directory and the shared 0600 atomic writer.
pub(crate) fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), HiveError> {
    private_dir(dir)?;
    crate::core::secure_fs::write_secret_file(&dir.join(name), bytes)
        .map_err(|e| HiveError::Local(format!("write {name} in {}: {e}", dir.display())))
}

fn private_dir(dir: &Path) -> Result<(), HiveError> {
    let local =
        |what: &str, e: std::io::Error| HiveError::Local(format!("{what} {}: {e}", dir.display()));
    std::fs::create_dir_all(dir).map_err(|e| local("create", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| local("restrict", e))?;
    }
    Ok(())
}

/// Copies of a damaged state file kept beside it (`<name>.damaged-<n>`);
/// older ones are removed (RB8 M14).
pub(crate) const DAMAGED_KEPT: usize = 3;

/// Load the JSON state file `path` (RB8 M12). Missing: `Ok(None)`. A file
/// that does not parse, or that `accept` refuses (a version this build does
/// not know), is moved aside to `<name>.damaged-<n>` and reported as an
/// error: no caller silently starts again from empty state, each decides
/// how it fails closed. A file that cannot be read stays where it is.
pub(crate) fn load_or_quarantine<T: DeserializeOwned>(
    path: &Path,
    accept: impl FnOnce(&T) -> bool,
) -> Result<Option<T>, HiveError> {
    let Some(bytes) = read_optional(path)? else {
        return Ok(None);
    };
    let why = match serde_json::from_slice::<T>(&bytes) {
        Ok(value) if accept(&value) => return Ok(Some(value)),
        Ok(_) => "a version this build does not know".to_string(),
        Err(e) => e.to_string(),
    };
    let moved = quarantine(path)?;
    Err(HiveError::Local(format!(
        "{} is damaged ({why}); moved to {}",
        path.display(),
        moved.display()
    )))
}

/// Move `path` to `<name>.damaged-<n>` (the next free number) and keep only
/// the newest [`DAMAGED_KEPT`] such copies.
fn quarantine(path: &Path) -> Result<PathBuf, HiveError> {
    let local = |e: std::io::Error| HiveError::Local(format!("quarantine {}: {e}", path.display()));
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return Err(HiveError::Local(format!("{} has no name", path.display())));
    };
    let prefix = format!("{name}.damaged-");
    let mut copies: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)
        .map_err(local)?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let n = e
                .file_name()
                .to_str()?
                .strip_prefix(&prefix)?
                .parse()
                .ok()?;
            Some((n, e.path()))
        })
        .collect();
    copies.sort_unstable();
    let next = copies.last().map_or(1, |(n, _)| n + 1);
    let moved = dir.join(format!("{prefix}{next}"));
    std::fs::rename(path, &moved).map_err(local)?;
    copies.push((next, moved.clone()));
    let excess = copies.len().saturating_sub(DAMAGED_KEPT);
    for (_, old) in copies.drain(..excess) {
        let _ = std::fs::remove_file(old);
    }
    Ok(moved)
}

/// How long a state lock is waited for before the caller gets an error.
const STATE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// An exclusive lock on `<dir>/<name>.lock` for one read-modify-write of a
/// state file shared by processes (RB8 E5: the tool calls and the relay).
/// Held only around local file work, never across a network request.
pub(crate) struct StateLock(std::fs::File);

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

/// Take the lock for the state file `dir/name`, waiting at most
/// [`STATE_LOCK_WAIT`]; symlinks are refused.
pub(crate) fn lock_state(dir: &Path, name: &str) -> Result<StateLock, HiveError> {
    lock_state_within(dir, name, STATE_LOCK_WAIT)
}

/// [`lock_state`] waiting at most `wait` (zero: only when free right now).
pub(crate) fn lock_state_within(
    dir: &Path,
    name: &str,
    wait: std::time::Duration,
) -> Result<StateLock, HiveError> {
    private_dir(dir)?;
    let path = dir.join(format!("{name}.lock"));
    let local = |what: &str| HiveError::Local(format!("{what} {}", path.display()));
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .map_err(|e| local(&format!("open ({e})")))?;
    crate::core::file_lock::acquire_exclusive_timeout(&file, wait)
        .map_err(|e| local(&format!("lock ({e})")))?;
    Ok(StateLock(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Personal, Team, Company (Hive C line): the record says where the
    /// engine is enrolled; a record from before keeps the plain name.
    #[test]
    fn workspace_label_names_personal_team_and_company() {
        let mut r = InstanceRecord {
            instance_id: "inst_1".into(),
            org_id: "org_1".into(),
            org_name: "Platform".into(),
            label: "mbp".into(),
            url: "https://hive.example".into(),
            org_salt_id: "s".into(),
            enrolled_at: "2026-10-26T10:00:00Z".into(),
            web_url: None,
            org_kind: None,
            company_name: None,
        };
        assert_eq!(r.workspace_label(), "Platform");
        r.org_kind = Some("team".into());
        assert_eq!(r.workspace_label(), "Team Platform");
        r.company_name = Some("Acme".into());
        assert_eq!(r.workspace_label(), "Company Acme · Team Platform");
        r.company_name = None;
        r.org_kind = Some("personal".into());
        assert_eq!(r.workspace_label(), "Personal (Platform)");
        // an old record without the new fields still reads
        let old: InstanceRecord = serde_json::from_value(serde_json::json!({
            "instance_id": "i", "org_id": "o", "org_name": "Acme", "label": "l", "url": "u",
            "org_salt_id": "s", "enrolled_at": "t"
        }))
        .unwrap();
        assert_eq!((old.org_kind, old.company_name), (None, None));
    }

    #[test]
    fn private_files_are_atomic_and_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("config"), dir.path().join("data"));
        let token = TokenRecord {
            access_token: "atl_secret".into(),
            expires_at: 1,
        };
        store.write_config(TOKEN_FILE, &token).unwrap();
        assert_eq!(
            store.read_config::<TokenRecord>(TOKEN_FILE).unwrap(),
            Some(token.clone())
        );
        assert!(!format!("{token:?}").contains("atl_secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(store.config_dir()), 0o700);
            assert_eq!(mode(&store.config_dir().join(TOKEN_FILE)), 0o600);
        }
        let leftovers: Vec<_> = std::fs::read_dir(store.config_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn damaged_state_is_moved_aside_and_only_three_copies_stay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let load = |accept: bool| load_or_quarantine::<Value>(&path, |_| accept);
        assert_eq!(load(true).unwrap(), None, "missing");
        std::fs::write(&path, b"{\"ok\": 1}").unwrap();
        assert_eq!(load(true).unwrap(), Some(serde_json::json!({"ok": 1})));
        // Refused by the caller (an unknown version): moved, an error.
        let error = load(false).unwrap_err().to_string();
        assert!(error.contains("damaged-1"), "{error}");
        assert!(!path.exists());
        for n in 0..4 {
            std::fs::write(&path, format!("broken {n}")).unwrap();
            assert!(load(true).is_err());
        }
        let mut kept: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .collect();
        kept.sort();
        assert_eq!(
            kept,
            [
                "state.json.damaged-3",
                "state.json.damaged-4",
                "state.json.damaged-5"
            ]
        );
        assert_eq!(
            std::fs::read(dir.path().join("state.json.damaged-5")).unwrap(),
            b"broken 3",
            "the newest copy is the last damaged file"
        );
    }

    #[test]
    fn a_held_state_lock_is_not_taken_twice() {
        let dir = tempfile::tempdir().unwrap();
        let held = lock_state(dir.path(), "leases.json").unwrap();
        assert!(lock_state_within(dir.path(), "leases.json", std::time::Duration::ZERO).is_err());
        drop(held);
        assert!(lock_state_within(dir.path(), "leases.json", std::time::Duration::ZERO).is_ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.path().join("leases.json.lock"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn forget_keeps_local_data_on_request() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("config"), dir.path().join("data"));
        store
            .write_config(TOKEN_FILE, &serde_json::json!({}))
            .unwrap();
        store.record_success("lease", "2026-10-20T12:00:00Z");
        store.forget(true).unwrap();
        assert!(store.read_config_bytes(TOKEN_FILE).unwrap().is_none());
        assert_eq!(
            store.activity().last_seen.as_deref(),
            Some("2026-10-20T12:00:00Z")
        );
        store.forget(false).unwrap();
        assert!(store.activity().last_seen.is_none());
    }
}
