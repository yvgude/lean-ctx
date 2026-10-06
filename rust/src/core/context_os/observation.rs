// SPDX-License-Identifier: Apache-2.0
//! Private, durable observations owned by the ContextBus.

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
        }
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
pub(crate) fn decode_row(
    row: &rusqlite::Row<'_>,
) -> Result<LocalObservationV1, ObservationPersistenceError> {
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
            let event: LeanCtxEvent = serde_json::from_str(&payload_json)?;
            if event.id != source_id || event.timestamp != source_timestamp {
                return Err(ObservationPersistenceError::InvalidRow(
                    "legacy payload contradicts observation identity or timestamp".into(),
                ));
            }
            LocalObservationPayload::Legacy(event)
        }
    };
    Ok(LocalObservationV1 {
        cursor_id,
        producer_instance,
        source_id,
        source_timestamp,
        source,
        payload,
    })
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
