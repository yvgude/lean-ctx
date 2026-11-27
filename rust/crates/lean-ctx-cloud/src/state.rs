// SPDX-License-Identifier: Apache-2.0
//! Durable Cloud state on this machine.
//!
//! `<config_dir>/cloud/` (directory 0700, files 0600, written atomically):
//! `instance.key`, `kx.key`, `instance.json`, `lease.json` (re-verified on
//! every load), `keys.json`, `token.json` (never logged).
//! `<data_dir>/cloud/`: `activity.json` and, with later packages, project
//! maps, cursors and the outbox.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::CloudError;

pub(crate) const INSTANCE_KEY_FILE: &str = "instance.key";
pub(crate) const KX_KEY_FILE: &str = "kx.key";
pub(crate) const INSTANCE_FILE: &str = "instance.json";
pub(crate) const LEASE_FILE: &str = "lease.json";
pub(crate) const KEYS_FILE: &str = "keys.json";
pub(crate) const TOKEN_FILE: &str = "token.json";
pub(crate) const ACTIVITY_FILE: &str = "activity.json";
/// The person's sharing answer at connect (`super::connect`).
pub(crate) const SHARING_FILE: &str = "sharing.json";
/// When the enrolled Cloud last answered that it has no match route
/// (`super::mapping`).
pub(crate) const MATCH_FILE: &str = "vault-match.json";
/// Who `<data>/cloud` belongs to (3.12.8 account scope, [`DataScope`]).
pub(crate) const SCOPE_FILE: &str = "scope.json";

/// Every entry of `<config>/cloud` (rc4-274). In a single-dir layout (a
/// legacy `~/.lean-ctx`, a custom `LEAN_CTX_DATA_DIR`) `<config>/cloud` and
/// `<data>/cloud` are one directory: removing the data there keeps these,
/// with their `.damaged-<n>` copies and in-flight temporary files. Every
/// name written through [`CloudStore::write_config`] must be listed (checked
/// in debug builds); an entry not listed counts as data and is removed.
const CONFIG_ENTRIES: &[&str] = &[
    INSTANCE_KEY_FILE,
    KX_KEY_FILE,
    INSTANCE_FILE,
    LEASE_FILE,
    KEYS_FILE,
    TOKEN_FILE,
    SHARING_FILE,
    MATCH_FILE,
    super::keyset_v3::KEYS_V3_FILE,
    super::root_cert::CERT_FILE,
    super::revoked::REVOKED_FILE,
    super::disconnect::PENDING_FILE,
    super::session_ref::CAPABILITIES_FILE,
    super::company_bound::FILE,
    super::bus::relay_v2::PROTOCOL_FILE,
    super::consent::chat_key::CHAT_KEY_FILE,
    super::consent::optin_key::OPTIN_KEY_FILE,
    super::consent::task_key::TASK_KEY_FILE,
    super::consent::broker::PRESENCE_KEY_FILE,
    super::consent::broker::ACCOUNT_FILE,
    super::consent::broker::KEY_FILE,
    super::consent::broker::DIR,
    // `replica::crypt::master_key`: the replica master key, when no
    // keychain holds it.
    "replica.key",
];

/// `bring_local` ledgers, one per checkout: `bring-local-<hash>.json`.
const CONFIG_PREFIX_BRING_LOCAL: &str = "bring-local-";

/// Whether `name`, an entry of `<config>/cloud`, belongs to the config
/// (rc4-274). Lock files always do: a lock file removed while held no
/// longer orders anything, and an empty lock of the data costs nothing.
fn is_config_entry(name: &str) -> bool {
    if Path::new(name).extension().is_some_and(|ext| ext == "lock") {
        return true;
    }
    let base = name
        .strip_prefix('.')
        .and_then(|n| n.split_once(".tmp."))
        .map_or(name, |(base, _)| base);
    let base = base.split_once(".damaged-").map_or(base, |(base, _)| base);
    CONFIG_ENTRIES.contains(&base) || base.starts_with(CONFIG_PREFIX_BRING_LOCAL)
}

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
    /// Origin of the Cloud web app, when enrollment named one (1.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url: Option<String>,
    /// `personal` | `team`, when enrollment named it (Cloud C line).
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

/// When Cloud last answered, per channel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityRecord {
    pub last_seen: Option<String>,
    #[serde(default)]
    pub last_success: BTreeMap<String, String>,
    pub last_error: Option<String>,
    /// Why Cloud last refused this workspace as locked (`trial_ended`,
    /// `plan_ended`); cleared by the next lease Cloud issues.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<String>,
    /// Why the last lease renewal failed; kept until a renewal succeeds
    /// (unlike `last_error`, which the next success of any channel clears).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renew_error: Option<String>,
}

/// Paths of the Cloud state; [`CloudStore::open`] for this machine,
/// [`CloudStore::at`] for tests and embedders.
#[derive(Debug, Clone)]
pub struct CloudStore {
    config: PathBuf,
    data: PathBuf,
}

impl CloudStore {
    /// `<config_dir>/cloud` and `<data_dir>/cloud` of this installation. Opening
    /// only resolves the paths: a read-only config directory keeps its mode (the
    /// Cloud's own directories are made private when something is written there).
    pub fn open() -> Result<Self, CloudError> {
        let config = lean_ctx::core::paths::config_dir_read_only().map_err(CloudError::Local)?;
        let data = lean_ctx::core::paths::data_dir().map_err(CloudError::Local)?;
        Ok(Self::at(config.join("cloud"), data.join("cloud")))
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

    pub(crate) fn read_config_bytes(&self, name: &str) -> Result<Option<Vec<u8>>, CloudError> {
        read_optional(&self.config.join(name))
    }

    pub(crate) fn write_config_bytes(&self, name: &str, bytes: &[u8]) -> Result<(), CloudError> {
        debug_assert!(is_config_entry(name), "{name} is missing in CONFIG_ENTRIES");
        write_private(&self.config, name, bytes)
    }

    pub(crate) fn read_config<T: DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<Option<T>, CloudError> {
        read_json(&self.config.join(name))
    }

    pub(crate) fn write_config<T: Serialize>(
        &self,
        name: &str,
        value: &T,
    ) -> Result<(), CloudError> {
        debug_assert!(is_config_entry(name), "{name} is missing in CONFIG_ENTRIES");
        write_private(&self.config, name, &to_json(value)?)
    }

    pub(crate) fn remove_config(&self, name: &str) -> Result<(), CloudError> {
        remove_optional(&self.config.join(name))
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
    pub fn record_locked(&self, reason: Option<&str>) {
        let mut activity = self.activity();
        if activity.locked.as_deref() == reason {
            return;
        }
        activity.locked = reason.map(str::to_string);
        if let Ok(bytes) = to_json(&activity) {
            let _ = write_private(&self.data, ACTIVITY_FILE, &bytes);
        }
    }

    /// Record (`Some`) or clear (`None`) why the lease renewal failed.
    pub(crate) fn record_renew_error(&self, message: Option<&str>) {
        let mut activity = self.activity();
        if activity.renew_error.as_deref() == message {
            return;
        }
        activity.renew_error = message.map(str::to_string);
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

    /// Whether this engine may record anything for Cloud now: connected (an
    /// instance record) and not revoked. Two file checks; the recording
    /// paths call it on every record (K2 review 2, N1).
    pub(crate) fn may_record(&self) -> bool {
        self.config.join(INSTANCE_FILE).is_file()
            && !self.config.join(super::revoked::REVOKED_FILE).is_file()
    }

    /// The slower half of the recording check (K2 review 3, NEW-3): the
    /// `[cloud]` switches allow engine metrics and the stored lease has not
    /// lapsed (read from the local copy; a lease that cannot be read counts
    /// as lapsed).
    pub(crate) fn recording_allowed(&self, config: &super::config::CloudConfig) -> bool {
        config.enabled != Some(false) && config.metrics_upload && self.lease_live()
    }

    fn lease_live(&self) -> bool {
        use lean_ctx_protocol::cloud::lease::{CloudLeaseV1, LeaseState};
        let Ok(Some(record)) = self.read_config::<LeaseRecord>(LEASE_FILE) else {
            return false;
        };
        let Ok(lease) = serde_json::from_value::<CloudLeaseV1>(record.envelope["lease"].clone())
        else {
            return false;
        };
        let service_now = (lean_ctx_time::UtcDateTime::now()
            + lean_ctx_time::Duration::seconds(record.offset_seconds))
        .fixed_offset();
        matches!(
            lease.state_at(service_now),
            Ok(LeaseState::Valid | LeaseState::Grace)
        )
    }

    /// Who `<data>/cloud` belongs to, when stamped (3.12.8 account scope).
    #[cfg(test)]
    fn scope(&self) -> Option<DataScope> {
        self.read_scope().ok().flatten()
    }

    /// The stamp of `<data>/cloud`; `Err` when it cannot be read, which
    /// counts as another account's.
    fn read_scope(&self) -> Result<Option<DataScope>, ()> {
        read_json(&self.data.join(SCOPE_FILE)).map_err(|_| ())
    }

    fn stamp_scope(&self, instance: &InstanceRecord) -> Result<(), CloudError> {
        write_private(&self.data, SCOPE_FILE, &to_json(&DataScope::of(instance))?)
    }

    /// Remove `<data>/cloud` in one step: renamed aside first, so no reader
    /// sees half of it, then deleted. The replica master key lives in the
    /// config directory and stays. When the config directory is the data
    /// directory or lies inside it (a single-dir layout, rc4-274), only the
    /// data entries are moved aside: the enrolment, its keys and its lease
    /// stay.
    fn wipe_data(&self) -> Result<(), CloudError> {
        if !self.data.exists() {
            return Ok(());
        }
        let name = self
            .data
            .file_name()
            .map_or_else(|| "cloud".into(), |n| n.to_string_lossy().into_owned());
        let aside = self
            .data
            .with_file_name(format!("{name}.wiped-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&aside);
        let move_aside = |e: std::io::Error| {
            CloudError::Local(format!("move {} aside: {e}", self.data.display()))
        };
        match self.config_in_data() {
            None => std::fs::rename(&self.data, &aside).map_err(move_aside)?,
            Some(config) => {
                // The entry of `<data>/cloud` that holds the config directory,
                // or none when the two are the same directory.
                let holder = config.components().next().map(|c| c.as_os_str().to_owned());
                private_dir(&aside)?;
                for entry in std::fs::read_dir(&self.data).map_err(move_aside)? {
                    let entry = entry.map_err(move_aside)?;
                    let entry_name = entry.file_name();
                    let keep = match &holder {
                        Some(holder) => *holder == entry_name,
                        None => is_config_entry(&entry_name.to_string_lossy()),
                    };
                    if !keep {
                        std::fs::rename(entry.path(), aside.join(&entry_name))
                            .map_err(move_aside)?;
                    }
                }
            }
        }
        std::fs::remove_dir_all(&aside)
            .map_err(|e| CloudError::Local(format!("remove {}: {e}", aside.display())))?;
        self.sweep_wiped(&name);
        Ok(())
    }

    /// Where the config directory lies inside the data directory, relative
    /// to it (empty: the same directory), or `None` when it lies elsewhere
    /// (the XDG split). Compared as resolved paths, so a symlinked root is
    /// seen as well.
    fn config_in_data(&self) -> Option<PathBuf> {
        let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let (config, data) = (resolve(&self.config), resolve(&self.data));
        config.strip_prefix(&data).ok().map(Path::to_path_buf)
    }

    /// Copies an earlier removal moved aside but did not finish deleting
    /// (killed process); best effort.
    fn sweep_wiped(&self, name: &str) {
        let Some(parent) = self.data.parent() else {
            return;
        };
        let prefix = format!("{name}.wiped-");
        let Ok(entries) = std::fs::read_dir(parent) else {
            return;
        };
        for entry in entries.flatten() {
            let is_copy = entry.file_name().to_string_lossy().starts_with(&prefix);
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            if is_copy && is_dir {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }

    /// Remove another account's local Cloud data: first the knowledge
    /// pulled from it into project knowledge (the vault map that finds the
    /// checkouts goes with the data), then `<data>/cloud`. Both fail
    /// closed: nothing is removed half while its knowledge stays findable.
    fn purge_data(&self) -> Result<(), CloudError> {
        self.remove_pulled_knowledge().map_err(|failed| {
            CloudError::Local(format!(
                "the knowledge pulled from LeanCTX Cloud could not be removed from {failed}. Until it is, nothing of LeanCTX Cloud is used on this computer. Try again; if it keeps failing, run `lean-ctx cloud disconnect` in a terminal."
            ))
        })?;
        self.wipe_data()
    }

    /// The lock that orders scope decisions (wipe and stamp) between
    /// processes; held in the config directory, which a wipe leaves alone.
    fn lock_scope(&self) -> Result<StateLock, CloudError> {
        lock_state(&self.config, SCOPE_FILE)
    }

    /// Before a new enrolment is stored: the local Cloud data of another
    /// server or team goes (fail closed, 3.12.8 cross-account leak); data
    /// of the same server and team stays (a reconnect kept with
    /// `--keep-local`). Data without a stamp is of an unknown account and
    /// goes as well.
    pub(crate) fn scope_to_enrollment(&self, next: &InstanceRecord) -> Result<(), CloudError> {
        let _lock = self.lock_scope()?;
        let keep = matches!(
            self.read_scope(),
            Ok(Some(s)) if s.server == next.url && s.org_id == next.org_id
        );
        if !keep {
            self.purge_data()?;
        }
        self.stamp_scope(next)
    }

    /// The enrolled instance. `<data>/cloud` is made to belong to it first
    /// (3.12.8 account scope): every reader of local Cloud data (replicas,
    /// vault map, drafts, docs, tasks, status) asks here, so data stamped
    /// for another server or team is removed before anything of it is read.
    /// Another instance of the same server and team (a reconnect, whose
    /// enrolment already decided what stays) only moves the stamp, so a
    /// reader racing an enrolment never removes kept data. Data from before
    /// the stamp existed is adopted by the connected instance.
    pub fn instance(&self) -> Result<Option<InstanceRecord>, CloudError> {
        let Some(instance) = self.read_config::<InstanceRecord>(INSTANCE_FILE)? else {
            return Ok(None);
        };
        let now = DataScope::of(&instance);
        if matches!(self.read_scope(), Ok(Some(ref scope)) if *scope == now) {
            return Ok(Some(instance));
        }
        // Decided again under the lock: an enrolment or another reader may
        // have moved the stamp meanwhile. Only the removal fails closed; a
        // stamp that cannot be written (a read-only data directory) is
        // written by the next reader.
        let _lock = self.lock_scope()?;
        match self.read_scope() {
            Ok(Some(scope)) if scope == now => {}
            Ok(Some(scope)) if scope.server == now.server && scope.org_id == now.org_id => {
                let _ = self.stamp_scope(&instance);
            }
            Ok(None) if !self.data.exists() => {}
            Ok(None) => {
                let _ = self.stamp_scope(&instance);
            }
            Ok(Some(_)) | Err(()) => {
                self.purge_data()?;
                let _ = self.stamp_scope(&instance);
            }
        }
        Ok(Some(instance))
    }

    /// Facts pulled from Cloud into the project knowledge of every checkout
    /// the vault map knows. Every checkout and every store is tried; the
    /// ones that failed are returned, each with its folder and store file.
    fn remove_pulled_knowledge(&self) -> Result<(), String> {
        let map = super::vault::VaultMap::load(&self.data);
        let mut roots: Vec<&String> = map.vaults.values().flat_map(|r| &r.roots).collect();
        roots.sort();
        roots.dedup();
        let failed: Vec<String> = roots
            .into_iter()
            .filter_map(|root| {
                super::knowledge::learn::remove_all_pulled(root)
                    .err()
                    .map(|e| format!("the folder {root}: {e}"))
            })
            .collect();
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }

    /// The enrolled instance as stored, without the account-scope check of
    /// [`instance`](Self::instance). Only for a disconnect, which must work
    /// when that check cannot remove another account's pulled knowledge
    /// (a damaged store): it ends the enrolment it names.
    pub fn enrolled_instance(&self) -> Result<Option<InstanceRecord>, CloudError> {
        self.read_config::<InstanceRecord>(INSTANCE_FILE)
    }

    /// Remove credentials, lease and keys; with `keep_local` the data
    /// directory (outbox, replicas, activity) stays, but nothing keeps
    /// recording for Cloud (metrics, signals, receipts). Knowledge pulled
    /// into project knowledge always goes (3.12.8 account scope), from every
    /// store that can be cleaned; a store that cannot (damaged) is named
    /// after everything else is removed.
    pub fn forget(&self, keep_local: bool) -> Result<(), CloudError> {
        let knowledge = self.remove_pulled_knowledge().map_err(|failed| {
            CloudError::Local(format!(
                "LeanCTX Cloud is disconnected on this computer, but the knowledge pulled from it could not be removed from {failed}. Delete that knowledge.json (it holds that folder's local knowledge; a damaged one cannot be read anyway), then connect again."
            ))
        });
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
        if !keep_local {
            // Not `remove_dir_all`: in a single-dir layout that would take
            // the rest of the config with it (rc4-274).
            self.wipe_data()?;
        }
        knowledge
    }
}

/// `<data>/cloud/scope.json`: the server, team and instance the local Cloud
/// data was made for (3.12.8 account scope).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DataScope {
    server: String,
    org_id: String,
    instance_id: String,
}

impl DataScope {
    fn of(instance: &InstanceRecord) -> Self {
        Self {
            server: instance.url.clone(),
            org_id: instance.org_id.clone(),
            instance_id: instance.instance_id.clone(),
        }
    }
}

fn to_json<T: Serialize>(value: &T) -> Result<Vec<u8>, CloudError> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|e| CloudError::Local(format!("encode: {e}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, CloudError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(CloudError::Local(format!("read {}: {e}", path.display()))),
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, CloudError> {
    let Some(bytes) = read_optional(path)? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| CloudError::Local(format!("{} is damaged: {e}", path.display())))
}

fn remove_optional(path: &Path) -> Result<(), CloudError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CloudError::Local(format!("remove {}: {e}", path.display()))),
    }
}

/// Write `dir/name` so that it is never readable by others and never seen
/// half-written: a 0700 directory and the shared 0600 atomic writer.
pub(crate) fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), CloudError> {
    lean_ctx::core::secure_fs::write_private(dir, name, bytes)
        .map_err(|e| CloudError::Local(e.to_string()))
}

fn private_dir(dir: &Path) -> Result<(), CloudError> {
    lean_ctx::core::secure_fs::private_dir(dir).map_err(|e| CloudError::Local(e.to_string()))
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
) -> Result<Option<T>, CloudError> {
    let Some(bytes) = read_optional(path)? else {
        return Ok(None);
    };
    let why = match serde_json::from_slice::<T>(&bytes) {
        Ok(value) if accept(&value) => return Ok(Some(value)),
        Ok(_) => "a version this build does not know".to_string(),
        Err(e) => e.to_string(),
    };
    let moved = quarantine(path)?;
    Err(CloudError::Local(format!(
        "{} is damaged ({why}); moved to {}",
        path.display(),
        moved.display()
    )))
}

/// Move `path` to `<name>.damaged-<n>` (the next free number) and keep only
/// the newest [`DAMAGED_KEPT`] such copies.
pub(crate) fn quarantine(path: &Path) -> Result<PathBuf, CloudError> {
    let local =
        |e: std::io::Error| CloudError::Local(format!("quarantine {}: {e}", path.display()));
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return Err(CloudError::Local(format!("{} has no name", path.display())));
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
pub(crate) fn lock_state(dir: &Path, name: &str) -> Result<StateLock, CloudError> {
    lock_state_within(dir, name, STATE_LOCK_WAIT)
}

/// [`lock_state`] waiting at most `wait` (zero: only when free right now).
pub(crate) fn lock_state_within(
    dir: &Path,
    name: &str,
    wait: std::time::Duration,
) -> Result<StateLock, CloudError> {
    private_dir(dir)?;
    let path = dir.join(format!("{name}.lock"));
    let local = |what: &str| CloudError::Local(format!("{what} {}", path.display()));
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
    lean_ctx::core::file_lock::acquire_exclusive_timeout(&file, wait)
        .map_err(|e| local(&format!("lock ({e})")))?;
    Ok(StateLock(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Personal, Team, Company (Cloud C line): the record says where the
    /// engine is enrolled; a record from before keeps the plain name.
    #[test]
    fn workspace_label_names_personal_team_and_company() {
        let mut r = InstanceRecord {
            instance_id: "inst_1".into(),
            org_id: "org_1".into(),
            org_name: "Platform".into(),
            label: "mbp".into(),
            url: "https://cloud.example".into(),
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
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
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
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
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

/// 3.12.8 cross-account leak: `<data>/cloud` belongs to one server and team.
#[cfg(test)]
mod scope_tests {
    use super::*;

    fn instance(server: &str, org: &str, id: &str) -> InstanceRecord {
        InstanceRecord {
            instance_id: id.into(),
            org_id: org.into(),
            org_name: org.into(),
            label: "mbp".into(),
            url: server.into(),
            org_salt_id: "s".into(),
            enrolled_at: "2026-11-23T10:00:00Z".into(),
            web_url: None,
            org_kind: None,
            company_name: None,
        }
    }

    /// A's vault map and an unsent draft of A.
    fn data_of_a(store: &CloudStore) {
        write_private(store.data_dir(), "projects.json", b"{\"projects\":{}}").unwrap();
        let drafts = store.data_dir().join("outbox").join("knowledge");
        std::fs::create_dir_all(&drafts).unwrap();
        std::fs::write(drafts.join("d1.json"), b"{}").unwrap();
    }

    fn has_data_of_a(store: &CloudStore) -> bool {
        store.data_dir().join("projects.json").exists()
            && store.data_dir().join("outbox/knowledge/d1.json").exists()
    }

    fn connect(store: &CloudStore, record: &InstanceRecord) {
        store.write_config(INSTANCE_FILE, record).unwrap();
    }

    #[test]
    fn enrolling_another_team_or_server_removes_the_local_data() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let a = instance("https://cloud.example", "org_a", "inst_1");
        store.scope_to_enrollment(&a).unwrap();
        data_of_a(&store);

        // A reconnect to the same server and team (kept with --keep-local)
        store
            .scope_to_enrollment(&instance("https://cloud.example", "org_a", "inst_2"))
            .unwrap();
        assert!(has_data_of_a(&store), "same team keeps its drafts");
        assert_eq!(store.scope().unwrap().instance_id, "inst_2");

        // B: another team on the same server
        let b = instance("https://cloud.example", "org_b", "inst_3");
        store.scope_to_enrollment(&b).unwrap();
        assert!(!has_data_of_a(&store), "nothing of A stays for B");
        assert_eq!(store.scope(), Some(DataScope::of(&b)));

        // the same team id on another server is another account too
        data_of_a(&store);
        store
            .scope_to_enrollment(&instance("https://staging.example", "org_b", "inst_4"))
            .unwrap();
        assert!(!has_data_of_a(&store));
    }

    #[test]
    fn data_without_a_stamp_is_removed_at_enrolment() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        data_of_a(&store); // left by an older version with --keep-local
        store
            .scope_to_enrollment(&instance("https://cloud.example", "org_b", "inst_1"))
            .unwrap();
        assert!(!has_data_of_a(&store));
    }

    #[test]
    fn readers_get_nothing_while_disconnected_or_of_another_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let a = instance("https://cloud.example", "org_a", "inst_1");
        store.scope_to_enrollment(&a).unwrap();
        data_of_a(&store);

        // not connected (disconnect --keep-local): no instance, nothing read
        assert_eq!(store.instance().unwrap(), None);
        assert!(has_data_of_a(&store), "kept for a reconnect, not read");

        // connected to the stamped instance: read as before
        connect(&store, &a);
        assert_eq!(store.instance().unwrap(), Some(a.clone()));
        assert!(has_data_of_a(&store));

        // another instance of the same team only moves the stamp
        let a2 = instance("https://cloud.example", "org_a", "inst_2");
        connect(&store, &a2);
        assert_eq!(store.instance().unwrap(), Some(a2.clone()));
        assert!(has_data_of_a(&store));
        assert_eq!(store.scope(), Some(DataScope::of(&a2)));

        // an instance of another team the enrolment did not scope: A's data
        // is removed before anything reads it
        let b = instance("https://cloud.example", "org_b", "inst_3");
        connect(&store, &b);
        assert_eq!(store.instance().unwrap(), Some(b.clone()));
        assert!(!has_data_of_a(&store));
        assert_eq!(store.scope(), Some(DataScope::of(&b)));
    }

    #[test]
    fn data_from_before_the_stamp_is_adopted_by_the_connected_instance() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let a = instance("https://cloud.example", "org_a", "inst_1");
        data_of_a(&store);
        connect(&store, &a);
        assert_eq!(store.instance().unwrap(), Some(a.clone()));
        assert!(has_data_of_a(&store), "an upgrade keeps the drafts");
        assert_eq!(store.scope(), Some(DataScope::of(&a)));
    }

    /// A checkout of team A with one fact pulled from A and one own fact,
    /// found through A's vault map in `store`.
    fn knowledge_of_a(store: &CloudStore, root: &str) {
        use lean_ctx::core::knowledge::ProjectKnowledge;
        use lean_ctx_protocol::cloud::knowledge::{FactV1, ResearchContextDocumentV1};
        let doc = ResearchContextDocumentV1 {
            facts: vec![FactV1 {
                id: "f1".into(),
                statement: "Team A pulled claim here.".into(),
                kind: "observation".into(),
                applies_to: vec![],
                sources: vec![],
                from_contributions: vec![],
                anchors: vec![],
            }],
            ..ResearchContextDocumentV1::default()
        };
        super::super::knowledge::learn::sync_into_knowledge(root, "acme", "pay", 1, &doc).unwrap();
        let policy = lean_ctx::core::memory_policy::MemoryPolicy::default();
        ProjectKnowledge::mutate_locked(root, |k| {
            k.remember("arch", "own", "My own claim stays here.", "s", 0.9, &policy);
        })
        .unwrap();
        let mut map = super::super::vault::VaultMap::default();
        map.vaults.insert(
            "stable".into(),
            super::super::vault::VaultRecord {
                vault_id: "v1".into(),
                ns: "acme".into(),
                name: "pay".into(),
                sealed: false,
                roots: vec![root.to_string()],
            },
        );
        map.save(store.data_dir()).unwrap();
    }

    /// (pulled fact of A still there, own fact still there)
    fn knowledge_left(root: &str) -> (bool, bool) {
        let values: Vec<String> = lean_ctx::core::knowledge::ProjectKnowledge::load(root)
            .unwrap()
            .facts
            .iter()
            .map(|f| f.value.clone())
            .collect();
        (
            values.iter().any(|v| v.contains("Team A pulled")),
            values.iter().any(|v| v.contains("My own claim")),
        )
    }

    /// Disconnect always removes the facts pulled into project knowledge,
    /// also with --keep-local; the person's own facts stay.
    #[test]
    fn forget_removes_pulled_knowledge_also_when_keeping_local_data() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().to_string_lossy().into_owned();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let a = instance("https://cloud.example", "org_a", "inst_1");
        store.scope_to_enrollment(&a).unwrap();
        connect(&store, &a);
        knowledge_of_a(&store, &root);

        store.forget(true).unwrap();
        assert_eq!(knowledge_left(&root), (false, true));
        assert!(
            store.data_dir().join("projects.json").exists(),
            "--keep-local"
        );
        assert_eq!(store.instance().unwrap(), None);
    }

    /// 3.12.7 disconnect --keep-local left A's pulled facts behind; enrolling
    /// B removes them with A's data, or B's agents would recall them.
    #[test]
    fn enrolling_another_team_removes_the_knowledge_pulled_from_the_old_one() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().to_string_lossy().into_owned();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        knowledge_of_a(&store, &root); // unstamped, as 3.12.7 left it

        store
            .scope_to_enrollment(&instance("https://cloud.example", "org_b", "inst_2"))
            .unwrap();
        assert_eq!(knowledge_left(&root), (false, true));
        assert!(!store.data_dir().join("projects.json").exists());
    }

    /// The same for a reader that finds data stamped for another team.
    #[test]
    fn a_reader_removes_the_knowledge_pulled_from_another_team() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().to_string_lossy().into_owned();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        store
            .scope_to_enrollment(&instance("https://cloud.example", "org_a", "inst_1"))
            .unwrap();
        knowledge_of_a(&store, &root);

        connect(
            &store,
            &instance("https://cloud.example", "org_b", "inst_2"),
        );
        assert!(store.instance().unwrap().is_some());
        assert_eq!(knowledge_left(&root), (false, true));
    }

    #[test]
    fn a_damaged_stamp_counts_as_another_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let a = instance("https://cloud.example", "org_a", "inst_1");
        store.scope_to_enrollment(&a).unwrap();
        data_of_a(&store);
        std::fs::write(store.data_dir().join(SCOPE_FILE), b"{not json").unwrap();
        connect(&store, &a);
        assert_eq!(store.instance().unwrap(), Some(a.clone()));
        assert!(!has_data_of_a(&store));
        assert_eq!(store.scope(), Some(DataScope::of(&a)));

        // also at enrolment of the same team
        data_of_a(&store);
        std::fs::write(store.data_dir().join(SCOPE_FILE), b"{not json").unwrap();
        store.scope_to_enrollment(&a).unwrap();
        assert!(!has_data_of_a(&store));
    }

    #[test]
    fn copies_left_by_an_interrupted_removal_are_swept() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        let left = dir.path().join("data.wiped-4242");
        std::fs::create_dir_all(left.join("outbox")).unwrap();
        std::fs::write(left.join("outbox/d.json"), b"{}").unwrap();
        data_of_a(&store);
        store
            .scope_to_enrollment(&instance("https://cloud.example", "org_b", "inst_1"))
            .unwrap();
        assert!(!left.exists());
        assert!(!has_data_of_a(&store));
    }

    /// A checkout removed since (a deleted worktree) does not block the
    /// enrolment of another team: its pulled knowledge goes all the same.
    #[test]
    fn a_removed_checkout_does_not_block_enrolling_another_team() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().to_string_lossy().into_owned();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        knowledge_of_a(&store, &root);
        let store_dir = lean_ctx::core::knowledge::knowledge_dir_of(&root).unwrap();
        drop(repo);
        assert!(!std::path::Path::new(&root).exists());

        store
            .scope_to_enrollment(&instance("https://cloud.example", "org_b", "inst_2"))
            .unwrap();
        let stored = std::fs::read_to_string(store_dir.join("knowledge.json")).unwrap();
        assert!(!stored.contains("Team A pulled"), "{stored}");
        assert!(stored.contains("My own claim"), "{stored}");
    }

    /// An MCP request bound to another project still removes the knowledge
    /// pulled into every checkout (a removal needs no policy admission).
    #[test]
    fn a_request_bound_to_another_project_still_removes_pulled_knowledge() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let root = repo.path().to_string_lossy().into_owned();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        knowledge_of_a(&store, &root);

        let (config, data) = (dir.path().join("config"), dir.path().join("data"));
        lean_ctx::core::policy::runtime::spawn_for_project(other.path().to_path_buf(), move || {
            CloudStore::at(config, data)
                .scope_to_enrollment(&instance("https://cloud.example", "org_b", "inst_2"))
                .unwrap();
        })
        .join()
        .unwrap();
        assert_eq!(knowledge_left(&root), (false, true));
    }

    /// A damaged store keeps readers out (fail closed), but a disconnect
    /// still gets out: it cleans every readable store, ends the enrolment
    /// and names the damaged store's file.
    #[test]
    fn a_damaged_store_never_locks_the_disconnect_out() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let (healthy, damaged) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let healthy = healthy.path().to_string_lossy().into_owned();
        let damaged_root = damaged.path().to_string_lossy().into_owned();
        let store = CloudStore::at(dir.path().join("config"), dir.path().join("data"));
        store
            .scope_to_enrollment(&instance("https://cloud.example", "org_a", "inst_1"))
            .unwrap();
        knowledge_of_a(&store, &damaged_root);
        knowledge_of_a(&store, &healthy);
        let mut map = super::super::vault::VaultMap::load(store.data_dir());
        for record in map.vaults.values_mut() {
            record.roots = vec![healthy.clone(), damaged_root.clone()];
        }
        map.save(store.data_dir()).unwrap();
        let broken = lean_ctx::core::knowledge::knowledge_dir_of(&damaged_root)
            .unwrap()
            .join("knowledge.json");
        std::fs::write(&broken, b"{ damaged").unwrap();

        // connected to team B now: A's knowledge cannot all go, nothing is read
        let b = instance("https://cloud.example", "org_b", "inst_2");
        connect(&store, &b);
        assert!(store.instance().is_err());
        assert_eq!(store.enrolled_instance().unwrap(), Some(b));

        // the disconnect's local side
        let error = store.forget(false).unwrap_err().to_string();
        assert!(error.contains(&broken.display().to_string()), "{error}");
        assert_eq!(knowledge_left(&healthy), (false, true));
        assert_eq!(store.enrolled_instance().unwrap(), None);
        assert_eq!(store.instance().unwrap(), None);
    }

    /// The config of a single-dir layout: one of every kind of entry.
    const SINGLE_DIR_CONFIG: &[&str] = &[
        INSTANCE_KEY_FILE,
        KX_KEY_FILE,
        LEASE_FILE,
        KEYS_FILE,
        "keys_v3.json",
        "chat_key.json",
        "bring-local-0123abcd.json",
        "consent.key",
        "consent/offer-1.json",
        "replica.key",
        "scope.json.lock",
        "lease.json.damaged-1",
        ".keys.json.tmp.4242.0",
    ];

    fn single_dir_config(cloud: &Path) {
        for name in SINGLE_DIR_CONFIG {
            let path = cloud.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"{}").unwrap();
        }
    }

    fn missing_config(cloud: &Path) -> Vec<&'static str> {
        SINGLE_DIR_CONFIG
            .iter()
            .copied()
            .filter(|name| !cloud.join(name).exists())
            .collect()
    }

    /// rc4-274: a legacy `~/.lean-ctx` or a custom `LEAN_CTX_DATA_DIR` puts
    /// `<config>/cloud` and `<data>/cloud` in one directory. Enrolling
    /// another account removes only the data there; the config (keys,
    /// lease, consent, locks) stays and nothing is left beside it.
    #[test]
    fn a_single_dir_layout_keeps_the_config_when_the_data_goes() {
        let dir = tempfile::tempdir().unwrap();
        let cloud = dir.path().join("cloud");
        let store = CloudStore::at(&cloud, &cloud);
        single_dir_config(&cloud);
        data_of_a(&store);
        std::fs::write(cloud.join(ACTIVITY_FILE), b"{}").unwrap();

        let b = instance("https://cloud.example", "org_b", "inst_2");
        store.scope_to_enrollment(&b).unwrap();
        assert!(!has_data_of_a(&store), "nothing of A stays for B");
        assert!(!cloud.join(ACTIVITY_FILE).exists());
        assert_eq!(missing_config(&cloud), Vec::<&str>::new());
        assert_eq!(store.scope(), Some(DataScope::of(&b)));
        let beside: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(beside, ["cloud"], "no copy left beside it");

        // A reader connected to another team later: the same.
        data_of_a(&store);
        connect(
            &store,
            &instance("https://cloud.example", "org_c", "inst_3"),
        );
        assert!(store.instance().unwrap().is_some());
        assert!(!has_data_of_a(&store));
        assert_eq!(missing_config(&cloud), Vec::<&str>::new());
        assert!(cloud.join(INSTANCE_FILE).exists());
    }

    /// rc4-274: the config directory reached through a symlink, or lying
    /// inside the data directory, is kept as well.
    #[cfg(unix)]
    #[test]
    fn a_config_dir_behind_a_symlink_or_inside_the_data_stays() {
        let dir = tempfile::tempdir().unwrap();
        let cloud = dir.path().join("cloud");
        std::fs::create_dir_all(&cloud).unwrap();
        std::os::unix::fs::symlink(&cloud, dir.path().join("linked")).unwrap();
        let store = CloudStore::at(dir.path().join("linked"), &cloud);
        single_dir_config(&cloud);
        data_of_a(&store);
        store
            .scope_to_enrollment(&instance("https://cloud.example", "org_b", "inst_2"))
            .unwrap();
        assert!(!has_data_of_a(&store));
        assert_eq!(missing_config(&cloud), Vec::<&str>::new());

        let data = dir.path().join("data");
        let config = data.join("config").join("cloud");
        let store = CloudStore::at(&config, &data);
        single_dir_config(&config);
        data_of_a(&store);
        store
            .scope_to_enrollment(&instance("https://cloud.example", "org_b", "inst_2"))
            .unwrap();
        assert!(!has_data_of_a(&store));
        assert_eq!(missing_config(&config), Vec::<&str>::new());
    }

    /// rc4-274: a disconnect without `--keep-local` in a single-dir layout
    /// removes the data and the enrolment, but not the rest of the config
    /// (as in the XDG split, where `<config>/cloud` is never removed whole).
    #[test]
    fn a_single_dir_disconnect_removes_the_data_but_not_the_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cloud = dir.path().join("cloud");
        let store = CloudStore::at(&cloud, &cloud);
        store
            .scope_to_enrollment(&instance("https://cloud.example", "org_a", "inst_1"))
            .unwrap();
        connect(
            &store,
            &instance("https://cloud.example", "org_a", "inst_1"),
        );
        data_of_a(&store);
        std::fs::write(cloud.join("consent.key"), b"k").unwrap();
        store.forget(false).unwrap();
        assert!(!has_data_of_a(&store));
        assert!(!cloud.join(INSTANCE_FILE).exists());
        assert!(!cloud.join(SCOPE_FILE).exists());
        assert!(cloud.join("consent.key").exists());
    }

    /// Every name in the list is one a module writes (a renamed constant
    /// cannot leave a stale entry), and data names are not in it.
    #[test]
    fn config_entries_name_the_config_only() {
        for name in CONFIG_ENTRIES {
            assert!(is_config_entry(name), "{name}");
            assert!(is_config_entry(&format!(".{name}.tmp.1.0")), "{name}");
            assert!(is_config_entry(&format!("{name}.damaged-2")), "{name}");
        }
        for data in [
            ACTIVITY_FILE,
            SCOPE_FILE,
            "projects.json",
            "outbox",
            "bus",
            "replicas",
            ".projects.json.tmp.1.0",
        ] {
            assert!(!is_config_entry(data), "{data}");
        }
    }
}
