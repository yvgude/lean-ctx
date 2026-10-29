// SPDX-License-Identifier: Apache-2.0
//! Private, durable observations owned by the ContextBus.

use crate::core::context_admission::stores::StoreAdmission;
use crate::core::events::LeanCtxEvent;
use crate::core::ocla_bus::OclaEvent;
use std::fmt;
use std::sync::{Mutex, MutexGuard, OnceLock, TryLockError};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalObservationSource {
    Ocla,
    Legacy,
}

impl LocalObservationSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ocla => "ocla",
            Self::Legacy => "legacy",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ObservationPersistenceError> {
        match value {
            "ocla" => Ok(Self::Ocla),
            "legacy" => Ok(Self::Legacy),
            other => Err(ObservationPersistenceError::InvalidRow(format!(
                "unknown local observation source {other:?}"
            ))),
        }
    }
}

#[derive(Debug)]
pub enum ObservationPersistenceError {
    Io(std::io::Error),
    LockPoisoned,
    LockTimeout,
    IdentityConflict,
    Serialization(String),
    Sqlite(String),
    InvalidRow(String),
    /// Storage admission would not keep the payload as it is: not persisted.
    Withheld,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LocalObservationPayload {
    Ocla(OclaEvent),
    Legacy(LeanCtxEvent),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalObservationV1 {
    /// SQLite's local cursor; this is not the producer's identity.
    pub cursor_id: i64,
    /// A producer instance distinguishes IDs reused after a process restart.
    pub producer_instance: String,
    /// The original producer ID, retained as u64 independently of SQLite.
    pub source_id: u64,
    /// OCLA IDs use decimal milliseconds; legacy timestamps remain verbatim.
    pub source_timestamp: String,
    pub source: LocalObservationSource,
    pub payload: LocalObservationPayload,
}

impl fmt::Display for ObservationPersistenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "context bus storage unavailable: {error}"),
            Self::LockPoisoned => f.write_str("context bus observation lock poisoned"),
            Self::LockTimeout => f.write_str("context bus observation lock timed out"),
            Self::IdentityConflict => {
                f.write_str("observation identity reused with different data")
            }
            Self::Serialization(error) => write!(f, "observation serialization failed: {error}"),
            Self::Sqlite(error) => write!(f, "observation sqlite operation failed: {error}"),
            Self::InvalidRow(error) => write!(f, "invalid local observation row: {error}"),
            Self::Withheld => f.write_str("observation withheld by the context gateway"),
        }
    }
}

// ─── Admission at the write, current source rights at the use (G04, E3) ─────
//
// An observation is a typed record of an operation of this process. It is not
// text copied out of a source, but it carries strings its producer filled in
// (a path, a reason, a detail) and — in five event kinds — names the file the
// operation was about. Its `path` field is the source reference on record;
// nothing is added to the row, and the table format is unchanged.

/// Tool identity the file an observation names is re-read for.
const SOURCE_TOOL: &str = "ctx_read";

/// Every string value of a payload, decoded, one per line: what admission
/// inspects (the content a reader will see — not JSON escapes and not field
/// names next to numbers).
fn payload_text(payload: &serde_json::Value) -> String {
    fn collect<'a>(value: &'a serde_json::Value, leaves: &mut Vec<&'a str>) {
        match value {
            serde_json::Value::String(text) => leaves.push(text),
            serde_json::Value::Array(items) => {
                for item in items {
                    collect(item, leaves);
                }
            }
            serde_json::Value::Object(fields) => {
                fields.values().for_each(|field| collect(field, leaves));
            }
            _ => {}
        }
    }
    let mut leaves = Vec::new();
    collect(payload, &mut leaves);
    leaves.join("\n")
}

/// The file a payload names: the first string field called `path`.
fn payload_path(payload: &serde_json::Value) -> Option<&str> {
    let fields = payload.as_object()?;
    fields
        .get("path")
        .and_then(serde_json::Value::as_str)
        .or_else(|| fields.values().find_map(payload_path))
}

/// Storage admission of a payload before it becomes a durable row (E3): its
/// strings pass the current gateway policy (`StoreAdmission`, as every derived
/// store) and are kept only verbatim. A payload admission would withhold,
/// restrict or rewrite is not persisted; the refusal carries no value.
pub(crate) fn admit_payload(payload_json: &str) -> Result<(), ObservationPersistenceError> {
    let text = payload_text(&serde_json::from_str(payload_json)?);
    if kept_verbatim(&admission_snapshot(), &text) {
        Ok(())
    } else {
        Err(ObservationPersistenceError::Withheld)
    }
}

/// The current gateway policy as a snapshot that is not observed: a producer
/// may hold a store lock, and observing a policy change drops resident
/// stores. Payloads are kept only verbatim, so they are decided by the
/// built-in floor (`StoreAdmission::built_in_floor`): a semantic finding in a
/// path or a tool name could only drop the observation, never mask it.
fn admission_snapshot() -> StoreAdmission {
    StoreAdmission::with_policy(
        crate::core::context_admission::AdmissionPolicy::from_config(
            &crate::core::config::Config::load_arc(),
        ),
    )
    .built_in_floor()
}

/// The ONE payload rule, for the write and for every use: the strings of a
/// payload may be persisted — and shown — only if admission keeps them
/// verbatim NOW. The project policy decides on the original text, then the
/// gateway detectors, then the project policy again on what they left
/// (`StoreAdmission::admit_shell_command`: that exact composition; nothing in
/// it is specific to a shell). A masked variant is never stored and never
/// returned.
fn kept_verbatim(admission: &StoreAdmission, text: &str) -> bool {
    text.is_empty() || admission.admit_shell_command(text).as_deref() == Some(text)
}

/// The serve-time check of ONE read of the observation store, for a surface
/// that shows observations — paths, names, free text — to a host or a model.
/// Built per call and dropped with it; counts and timestamps need no gate.
///
/// - An observation that names a file, read by a reader with a project: the
///   file is read NOW through the admitted rooted reader in that project
///   (`SourceGate`: path jail, role, project policy, gateway; bounded, over
///   the bound refused). Gone, outside the project, withheld or restricted:
///   not shown.
/// - An observation without a file, or a reader without a project: no source
///   authority on record — the originless rule (`serve_stored` without a
///   binding): withheld while protection is active in any layer, re-admitted
///   under today's content rules in Community developer mode.
///
/// BOTH conditions hold for every observation that leaves: the source right
/// above AND the payload rule of the write side ([`kept_verbatim`]) under
/// the policy of NOW. A row stored before admission existed, or before a
/// rule was tightened, is not legitimised by an admitted file.
///
/// One view per gate, taken in [`Self::for_project`]: one gateway snapshot
/// for every payload, one reading of the protection state for the originless
/// rule. Nothing is loaded per observation.
pub(crate) struct ObservationGate {
    files: Option<crate::core::bm25_index::source_gate::SourceGate>,
    admission: StoreAdmission,
    /// `stored::source_authority_required()` when the gate was built.
    protected: bool,
    withheld: usize,
}

impl ObservationGate {
    /// The gate of a reader working in `project`; without one, the project of
    /// the running request, if any.
    pub(crate) fn for_project(project: Option<&std::path::Path>) -> Self {
        use crate::core::bm25_index::source_gate::SourceGate;
        let request = crate::tools::admitted_sources::request_root();
        // ONE gateway snapshot for the payload checks and the source reads
        // of this gate: the file gate decides under the same rules.
        let admission = admission_snapshot();
        let root = match project {
            Some(root) => Some(root),
            None if request.is_empty() => None,
            None => Some(std::path::Path::new(&request)),
        };
        let files = root.map(|root| SourceGate::with_admission(root, SOURCE_TOOL, &admission));
        Self {
            files,
            admission,
            protected: crate::core::context_admission::stored::source_authority_required(),
            withheld: 0,
        }
    }

    /// Whether `observation` (an event or a decoded payload) may be shown now.
    pub(crate) fn serves<T: serde::Serialize>(&mut self, observation: &T) -> bool {
        let served = serde_json::to_value(observation).is_ok_and(|payload| {
            // The payload first, in both branches: every string value, the
            // set the write side walks, under today's rules.
            if !kept_verbatim(&self.admission, &payload_text(&payload)) {
                return false;
            }
            match (payload_path(&payload), self.files.as_mut()) {
                (Some(path), Some(files)) => files.serves(path, ""),
                // The originless rule: no source authority on record, so
                // nothing is shown under protection; without protection the
                // payload check above is the re-admission.
                _ => !self.protected,
            }
        });
        if !served {
            self.withheld += 1;
        }
        served
    }

    /// Whether the gateway rules configured NOW are still the ones this gate
    /// took its snapshot of: the digests are compared (`StoreAdmission::digest`,
    /// the form of the importer's `gateway_rules_unchanged`). Asked once, when
    /// the last observation was decided; `false` means the source reads of
    /// this call may have run under other rules than its payload checks.
    pub(crate) fn gateway_unchanged(&self) -> bool {
        self.admission.digest() == admission_snapshot().digest()
    }

    /// Observations this gate refused. A count only: no path, no text.
    pub(crate) fn withheld(&self) -> usize {
        self.withheld
    }
}

impl std::error::Error for ObservationPersistenceError {}

impl From<serde_json::Error> for ObservationPersistenceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error.to_string())
    }
}

impl From<rusqlite::Error> for ObservationPersistenceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error.to_string())
    }
}

impl From<std::io::Error> for ObservationPersistenceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) fn validate_identity(
    producer_instance: &str,
    source_id: u64,
) -> Result<(), ObservationPersistenceError> {
    if source_id == 0
        || !uuid::Uuid::parse_str(producer_instance)
            .is_ok_and(|id| id.to_string() == producer_instance && !id.is_nil())
    {
        return Err(ObservationPersistenceError::InvalidRow(
            "invalid producer instance or source ID".into(),
        ));
    }
    Ok(())
}

/// Validate persisted metadata before either reader projects a payload.
///
/// Returns `Ok(None)` for a legacy record whose event kind this build retired
/// (see `events::is_retired_event_record`): such rows are valid history from an
/// older release and are skipped instead of failing the whole projection.
pub(crate) fn decode_row(
    row: &rusqlite::Row<'_>,
) -> Result<Option<LocalObservationV1>, ObservationPersistenceError> {
    if row.get::<_, i64>(6)? != 1 {
        return Err(ObservationPersistenceError::InvalidRow(
            "unsupported local observation schema".into(),
        ));
    }
    let cursor_id: i64 = row.get(0)?;
    let source = LocalObservationSource::parse(row.get::<_, String>(1)?.as_str())?;
    let source_id_raw: String = row.get(2)?;
    let source_id = source_id_raw.parse::<u64>().map_err(|_| {
        ObservationPersistenceError::InvalidRow("invalid source ID encoding".into())
    })?;
    let producer_instance: String = row.get(5)?;
    validate_identity(&producer_instance, source_id)?;
    if cursor_id <= 0 || source_id.to_string() != source_id_raw {
        return Err(ObservationPersistenceError::InvalidRow(
            "noncanonical observation identity".into(),
        ));
    }
    let source_timestamp: String = row.get(3)?;
    let payload_json: String = row.get(4)?;
    let payload = match source {
        LocalObservationSource::Ocla => {
            if !source_timestamp
                .parse::<u64>()
                .is_ok_and(|value| value.to_string() == source_timestamp)
            {
                return Err(ObservationPersistenceError::InvalidRow(
                    "invalid OCLA timestamp encoding".into(),
                ));
            }
            LocalObservationPayload::Ocla(serde_json::from_str(&payload_json)?)
        }
        LocalObservationSource::Legacy => {
            if crate::core::events::is_retired_event_record(&payload_json) {
                return Ok(None);
            }
            let event: LeanCtxEvent = serde_json::from_str(&payload_json)?;
            if event.id != source_id || event.timestamp != source_timestamp {
                return Err(ObservationPersistenceError::InvalidRow(
                    "legacy payload contradicts observation identity or timestamp".into(),
                ));
            }
            LocalObservationPayload::Legacy(event)
        }
    };
    Ok(Some(LocalObservationV1 {
        cursor_id,
        producer_instance,
        source_id,
        source_timestamp,
        source,
        payload,
    }))
}

pub(crate) fn producer_instance() -> &'static str {
    static INSTANCE: OnceLock<String> = OnceLock::new();
    INSTANCE.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

/// Bound contention and reject poisoned state; callers never recover a guard.
pub(crate) fn lock_observation<T>(
    mutex: &Mutex<T>,
) -> Result<MutexGuard<'_, T>, ObservationPersistenceError> {
    let started = Instant::now();
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(_)) => {
                return Err(ObservationPersistenceError::LockPoisoned);
            }
            Err(TryLockError::WouldBlock) => {
                if started.elapsed() >= Duration::from_secs(5) {
                    return Err(ObservationPersistenceError::LockTimeout);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_os::context_bus::ContextBus;
    use crate::core::events::EventKind;

    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    fn event(id: u64, kind: EventKind) -> LeanCtxEvent {
        LeanCtxEvent {
            id,
            timestamp: "2026-01-01T00:00:00.000".to_owned(),
            kind,
        }
    }

    /// G04/E3: an observation that names a file is shown while the admitted
    /// reader returns that file and no longer once it is withdrawn; a payload
    /// storage admission would rewrite never becomes a row, and the refusal
    /// carries nothing of it.
    #[test]
    fn an_observation_is_shown_only_while_the_file_it_names_is_admitted() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project = tempfile::tempdir().expect("project");
        let root = project.path().canonicalize().expect("canonical root");
        let store = tempfile::tempdir().expect("store");
        let bus = ContextBus::open_at(store.path().join("context-bus.db"));
        std::fs::write(root.join("billing.rs"), "pub fn settle() {}\n").expect("source");
        let hit = event(
            1,
            EventKind::CacheHit {
                path: "billing.rs".to_owned(),
                saved_tokens: 10,
            },
        );
        bus.append_legacy_observation(producer_instance(), &hit)
            .expect("an admitted payload is stored");
        let shown = || -> usize {
            let mut gate = ObservationGate::for_project(Some(root.as_path()));
            bus.read_legacy_observations(10)
                .expect("rows")
                .iter()
                .filter(|stored| gate.serves(*stored))
                .count()
        };
        assert_eq!(shown(), 1);

        std::fs::remove_file(root.join("billing.rs")).expect("withdraw");
        assert_eq!(shown(), 0);
        let mut gate = ObservationGate::for_project(Some(root.as_path()));
        assert!(!gate.serves(&hit));
        assert_eq!(gate.withheld(), 1);

        let keyed = event(
            2,
            EventKind::VerificationWarning {
                warning_kind: "drift".to_owned(),
                detail: format!("deploy token {AWS_KEY}"),
                severity: "warn".to_owned(),
            },
        );
        let refused = bus
            .append_legacy_observation(producer_instance(), &keyed)
            .expect_err("a payload admission would mask is not persisted");
        assert!(matches!(refused, ObservationPersistenceError::Withheld));
        assert!(!refused.to_string().contains(AWS_KEY));
        assert_eq!(bus.read_legacy_observations(10).expect("rows").len(), 1);

        // Both conditions, also for a row stored before a rule existed: two
        // observations name the same admitted file; one carries a string
        // today's policy refuses. An admitted file does not legitimise it.
        std::fs::write(root.join("ledger.rs"), "pub fn post() {}\n").expect("source");
        let call = |id: u64, mode: &str| {
            event(
                id,
                EventKind::ToolCall {
                    tool: "ctx_read".to_owned(),
                    tokens_original: 10,
                    tokens_saved: 5,
                    mode: Some(mode.to_owned()),
                    duration_ms: 1,
                    path: Some("ledger.rs".to_owned()),
                },
            )
        };
        for stored in [call(3, "map"), call(4, "CONFIDENTIAL")] {
            bus.append_legacy_observation(producer_instance(), &stored)
                .expect("stored under the looser rule");
        }
        drop(community);
        let policy = crate::core::policy::load(
            "name = \"observation-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .expect("policy");
        let _tightened = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let mut gate = ObservationGate::for_project(Some(root.as_path()));
        let shown: Vec<u64> = bus
            .read_legacy_observations(10)
            .expect("rows")
            .iter()
            .filter(|stored| gate.serves(*stored))
            .map(|stored| stored.id)
            .collect();
        assert_eq!(shown, [3]);
    }
}
