// SPDX-License-Identifier: Apache-2.0

//! Bounded, durable replay protection for authenticated relay deliveries.
//!
//! Three ledger paths coexist and are deliberately kept apart. This store owns
//! relay-replay-v1.sqlite. The singleton replay-v2.json ledger belongs to the
//! remote-replay module and is a separate format, path, and namespace that this
//! store neither reads nor migrates. relay-replay-v1.json is the earlier
//! relay-specific JSON ledger in this same directory: a non-empty one is
//! refused with MigrationRequired rather than silently superseded.
//!
//! SQLite uses the rollback journal with synchronous=FULL, not WAL. The main
//! database is capped by max_page_count: its logical size is at most
//! max_page_count * page_size, which is no greater than the configured byte
//! limit. A rollback journal is finite as well (at most a 512-byte header
//! plus one page record and 8-byte record header per database page, before
//! filesystem-sector padding); the configured byte limit is a main-database
//! bound, not an unverified whole-filesystem quota. SQLite reuses free pages
//! and leaf space after retention pruning. SQLITE_FULL is reported as
//! unavailable because a page limit cannot be distinguished from a full disk.
//! Lowering the limit below an existing file is refused without deleting proofs;
//! restore the previous limit or perform an explicitly coordinated migration.
//!
//! Retention and signed expiry are bounded relative to the trusted local clock
//! (MAX_RETENTION_SECONDS), not an independent wall-clock authority. After an
//! accepted forward clock jump, rollback fails closed until time catches up.
//! Never lower the persisted clock or delete proofs merely to restore service:
//! that could permit replay. Operators must maintain a trustworthy system clock.
//!
//! A random lease is stored with every reservation. This prevents a stale
//! completion from applying after an explicit administrative reset. An external
//! creation marker detects deletion/replacement of an established database.
//! Deleting both files still destroys the old replay proofs; this cannot be
//! distinguished from a deliberate reset without an external authority. SQLite
//! atomicity protects the reservation state only; it does not make a
//! downstream business effect exactly-once.
//!
//! Pending reservations never expire. An interrupted delivery has an unknown
//! downstream effect, so the row is held until an operator completes or
//! releases it; automatic expiry would turn a crash into a second delivery.
//! Capacity held this way is recovered by reconciliation, not by waiting, and
//! the retry hint reflects that (see `UNRECLAIMABLE_RETRY_AFTER_SECONDS`).
//!
//! File-safety scope: the database, its marker, and the a2a directory this
//! store creates are private to the owner (0600/0700), symlinks are refused at
//! every component, and the database is opened with SQLITE_OPEN_NOFOLLOW. Those
//! checks assume the parent directories are under the same trust boundary as
//! the process. A concurrent writer that already has write access to a checked
//! parent can still swap a component between the check and the open; against
//! such an adversary no same-user filesystem guarantee is claimed here.

use rusqlite::{
    Connection, Error as SqliteError, ErrorCode, OpenFlags, OptionalExtension, Transaction,
    TransactionBehavior, params,
};
use std::fmt::Display;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

pub(crate) const MIN_RETENTION_SECONDS: i64 = 86_730;
/// Finite upper horizon for retention and for a signed expiry, in seconds.
///
/// 366 days. The production signed relay TTL is 24 hours, so a year already
/// exceeds every legitimate replay window by orders of magnitude, while a
/// finite bound keeps `now + retention` and `retain_until` representable for
/// any plausible wall clock. The generic store must still refuse pathological
/// internal inputs: without this bound `retention_seconds = i64::MAX` is
/// accepted and persisted at construction and then makes every ordinary
/// reservation fail, and `signed_expires_at = i64::MAX` pins a completed row
/// against the capacity bound effectively forever.
pub(crate) const MAX_RETENTION_SECONDS: i64 = 366 * 86_400;
pub(crate) const DEFAULT_MAX_ROWS: u64 = 262_144;
pub(crate) const DEFAULT_PER_ORIGIN_MAX_ROWS: u64 = 8_192;
pub(crate) const DEFAULT_MAX_DATABASE_BYTES: u64 = 256 * 1024 * 1024;

const SCHEMA_VERSION: i64 = 2;
const DATABASE_PAGE_SIZE: u64 = 4 * 1024;
const BUSY_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_LEASE_BYTES: usize = 32;
const NONCE_BYTES: usize = 16;
const PENDING: i64 = 0;
const COMPLETED: i64 = 1;

/// Retry hint used when no completed proof is scheduled to expire.
///
/// Capacity held by pending reservations is released by operator
/// reconciliation, never by the clock, so a one-second hint would advertise a
/// recovery that cannot happen. This value is an honest back-off, not a promise.
const UNRECLAIMABLE_RETRY_AFTER_SECONDS: u64 = 3_600;

/// Canonical schema. The same statements create the database and, compared
/// against sqlite_master, prove that an established database still has the
/// constraints, column types, and index definitions this store relies on.
const CREATE_META_TABLE: &str = "CREATE TABLE relay_replay_meta (
                 id INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
                 schema_version INTEGER NOT NULL,
                 creation_marker TEXT NOT NULL,
                 retention_seconds INTEGER NOT NULL,
                 last_observed_at INTEGER NOT NULL
             )";
const CREATE_REPLAY_TABLE: &str = "CREATE TABLE relay_replay (
                 storage_id TEXT PRIMARY KEY NOT NULL,
                 origin TEXT NOT NULL,
                 tenant TEXT NOT NULL,
                 project TEXT NOT NULL,
                 fingerprint TEXT NOT NULL,
                 signed_expires_at INTEGER NOT NULL,
                 retain_until INTEGER NOT NULL,
                 lease TEXT NOT NULL UNIQUE,
                 state INTEGER NOT NULL CHECK (state IN (0, 1)),
                 accepted_at INTEGER NOT NULL
             )";
const CREATE_RETAIN_INDEX: &str = "CREATE INDEX relay_replay_retain_idx
                 ON relay_replay (state, retain_until)";
const CREATE_ORIGIN_INDEX: &str = "CREATE INDEX relay_replay_origin_state_idx
                 ON relay_replay (origin, state)";
const CREATE_RESPONSE_TABLE: &str = "CREATE TABLE relay_task_responses (
                 storage_id TEXT PRIMARY KEY NOT NULL REFERENCES relay_replay(storage_id) ON DELETE CASCADE,
                 body TEXT NOT NULL CHECK (length(CAST(body AS BLOB)) BETWEEN 1 AND 65536)
             )";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RelayReplayLimits {
    pub(crate) max_rows: u64,
    pub(crate) per_origin_max_rows: u64,
    pub(crate) max_database_bytes: u64,
}

impl RelayReplayLimits {
    pub(crate) const fn new(
        max_rows: u64,
        per_origin_max_rows: u64,
        max_database_bytes: u64,
    ) -> Self {
        Self {
            max_rows,
            per_origin_max_rows,
            max_database_bytes,
        }
    }

    fn validate(self) -> Result<(), StoreError> {
        if self.max_rows == 0
            || self.per_origin_max_rows == 0
            || self.per_origin_max_rows > self.max_rows
            || self.max_rows > i64::MAX as u64
            || self.per_origin_max_rows > i64::MAX as u64
            || self.max_database_bytes < DATABASE_PAGE_SIZE
            || self.max_database_bytes > DEFAULT_MAX_DATABASE_BYTES
        {
            return Err(StoreError::InvalidInput(
                "relay replay limits are outside bounded range".to_string(),
            ));
        }
        Ok(())
    }
}

impl Default for RelayReplayLimits {
    fn default() -> Self {
        Self::new(
            DEFAULT_MAX_ROWS,
            DEFAULT_PER_ORIGIN_MAX_ROWS,
            DEFAULT_MAX_DATABASE_BYTES,
        )
    }
}

/// Compatibility alias for callers that prefer the shorter name.
pub(crate) type StoreLimits = RelayReplayLimits;

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum StoreError {
    #[error("invalid relay replay input: {0}")]
    InvalidInput(String),
    #[error("relay replay proof expired")]
    Expired,
    #[error("relay replay capacity exhausted; retry after {retry_after_seconds}s")]
    CapacityExhausted { retry_after_seconds: u64 },
    #[error("relay replay migration required: {0}")]
    MigrationRequired(String),
    #[error("relay replay lease mismatch")]
    LeaseMismatch,
    #[error("relay replay store unavailable: {0}")]
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reservation {
    Reserved(String),
    InFlight,
    Completed,
    Conflict,
}

#[derive(Debug)]
pub(crate) struct RelayReplayStore {
    database_path: PathBuf,
    creation_marker: String,
    limits: RelayReplayLimits,
    retention_seconds: i64,
}

impl RelayReplayStore {
    /// Open or create .lean-ctx/a2a/relay-replay-v1.sqlite below root.
    ///
    /// The retention floor is intentionally part of construction: lowering it
    /// below the signed relay deadline would turn an in-flight retry into a
    /// second delivery. Existing databases retain the larger established
    /// floor when reopened with a smaller caller value.
    pub(crate) fn new(
        root: impl AsRef<Path>,
        limits: RelayReplayLimits,
        retention_seconds: i64,
    ) -> Result<Self, StoreError> {
        limits.validate()?;
        validate_retention(retention_seconds)?;

        let root = root.as_ref();
        if root.as_os_str().is_empty() {
            return Err(StoreError::InvalidInput(
                "relay replay root must not be empty".to_string(),
            ));
        }
        reject_symlink(root, "relay replay root")?;
        let root = canonicalize_existing_path(root)?;
        ensure_directory_no_symlinks(&root, false)?;
        let a2a_dir = root.join(".lean-ctx").join("a2a");
        // Only the a2a directory itself is made private, and only if this call
        // creates it: the project root and .lean-ctx are shared directories
        // whose existing permissions are none of this store's business.
        ensure_directory_no_symlinks(&a2a_dir, true)?;

        let database_path = a2a_dir.join("relay-replay-v1.sqlite");
        let legacy_path = a2a_dir.join("relay-replay-v1.json");
        reject_symlink(&legacy_path, "legacy relay replay ledger")?;
        if path_len(&legacy_path)? > 0 {
            return Err(StoreError::MigrationRequired(format!(
                "non-empty legacy ledger at {}",
                legacy_path.display()
            )));
        }

        reject_database_sidecars(&database_path)?;
        let exists = database_state(&database_path)?;
        let marker_path = database_path.with_extension("established");
        let creation_marker = match (exists, read_creation_marker(&marker_path)?) {
            (true, Some(marker)) => marker,
            (false, None) => create_creation_marker(&marker_path)?,
            _ => return Err(StoreError::Unavailable(
                "relay replay database and established marker must both exist; explicit recovery required".into(),
            )),
        };
        let mut connection = open_connection(&database_path, !exists)?;
        configure_connection(&mut connection, limits.max_database_bytes, !exists)?;

        if exists {
            quick_check(&connection)?;
            enforce_database_bound(&connection, limits.max_database_bytes)?;
            migrate_response_schema(&mut connection, &creation_marker)?;
        } else {
            initialize_schema(&mut connection, retention_seconds, &creation_marker)?;
        }
        let metadata = validate_schema(&connection)?;
        if metadata.creation_marker != creation_marker {
            return Err(StoreError::Unavailable(
                "relay replay database identity mismatch".into(),
            ));
        }
        let established_retention = metadata.retention_seconds;
        let effective_retention = established_retention.max(retention_seconds);
        if effective_retention != established_retention {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| sqlite_unavailable("retention update", &error))?;
            transaction
                .execute(
                    "UPDATE relay_replay_meta SET retention_seconds = ?1 WHERE id = 1",
                    params![effective_retention],
                )
                .map_err(|error| sqlite_unavailable("retention update", &error))?;
            transaction
                .commit()
                .map_err(|error| sqlite_unavailable("retention update commit", &error))?;
        }
        enforce_database_bound(&connection, limits.max_database_bytes)?;

        Ok(Self {
            database_path,
            creation_marker,
            limits,
            retention_seconds: effective_retention,
        })
    }

    /// Reserve one authenticated relay proof at caller-supplied time.
    ///
    /// storage_id and fingerprint are canonical lowercase SHA-256 hex
    /// digests. Scope is compared as three exact SQLite values, never as a
    /// delimiter-based composite key.
    pub(crate) fn reserve(
        &self,
        storage_id: &str,
        origin: &str,
        tenant: &str,
        project: &str,
        fingerprint: &str,
        signed_expires_at: i64,
        now_seconds: i64,
    ) -> Result<Reservation, StoreError> {
        validate_digest(storage_id, "storage_id")?;
        validate_scope(origin, "origin")?;
        validate_scope(tenant, "tenant")?;
        validate_scope(project, "project")?;
        validate_digest(fingerprint, "fingerprint")?;

        let mut connection = self.open_for_operation()?;
        let metadata = validate_schema(&connection)?;
        let retention_seconds = self.retention_seconds.max(metadata.retention_seconds);
        let allowed_pages = enforce_database_bound(&connection, self.limits.max_database_bytes)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| sqlite_unavailable("reserve transaction", &error))?;

        let persisted_now = read_last_observed(&transaction)?;
        let effective_now = now_seconds.max(persisted_now);
        // Reject an absurd signed expiry before it can reach storage: a proof
        // whose deadline lies beyond the finite horizon would pin capacity for
        // effectively unbounded time. Checked against the effective clock, so a
        // caller is never penalised for a clock that ran backwards.
        let horizon = effective_now
            .checked_add(MAX_RETENTION_SECONDS)
            .ok_or_else(|| StoreError::InvalidInput("relay replay time overflow".to_string()))?;
        if signed_expires_at > horizon {
            return Err(StoreError::InvalidInput(format!(
                "signed relay expiry exceeds the bounded {MAX_RETENTION_SECONDS}s replay horizon"
            )));
        }
        prune_expired(&transaction, effective_now)?;

        if let Some(existing) = find_entry(&transaction, storage_id)? {
            let same_scope = existing.origin == origin
                && existing.tenant == tenant
                && existing.project == project;
            let result = if !same_scope || existing.fingerprint != fingerprint {
                Reservation::Conflict
            } else {
                match existing.state {
                    PENDING => Reservation::InFlight,
                    COMPLETED => Reservation::Completed,
                    _ => {
                        return Err(StoreError::Unavailable(
                            "relay replay row has an invalid state".to_string(),
                        ));
                    }
                }
            };
            update_last_observed(&transaction, effective_now)?;
            transaction
                .commit()
                .map_err(|error| sqlite_unavailable("duplicate reserve commit", &error))?;
            return Ok(result);
        }

        if signed_expires_at < effective_now {
            // A gross local clock rollback is an infrastructure fault, not a
            // permanent peer error. Preserve the floor and all replay checks;
            // only classify the refusal as retryable by the HTTP caller.
            if persisted_now.saturating_sub(now_seconds) > MAX_RETENTION_SECONDS {
                return Err(StoreError::Unavailable(
                    "persisted relay replay clock is ahead of the system clock".to_string(),
                ));
            }
            return Err(StoreError::Expired);
        }
        let retention_deadline = effective_now
            .checked_add(retention_seconds)
            .ok_or_else(|| StoreError::InvalidInput("relay replay time overflow".to_string()))?;
        let retain_until = signed_expires_at.max(retention_deadline);

        let total_rows = count_rows(&transaction, None)?;
        if total_rows >= self.limits.max_rows {
            let retry_after = retry_after_seconds(&transaction, None, effective_now)?;
            return Err(StoreError::CapacityExhausted {
                retry_after_seconds: retry_after,
            });
        }
        let origin_rows = count_rows(&transaction, Some(origin))?;
        if origin_rows >= self.limits.per_origin_max_rows {
            let retry_after = retry_after_seconds(&transaction, Some(origin), effective_now)?;
            return Err(StoreError::CapacityExhausted {
                retry_after_seconds: retry_after,
            });
        }
        // A full page allocation can still contain reusable B-tree leaf space.
        // Let SQLite enforce max_page_count rather than rejecting such inserts.

        let mut inserted_lease = None;
        for _ in 0..4 {
            let lease = new_lease()?;
            let inserted = match transaction.execute(
                "INSERT INTO relay_replay (
                         storage_id, origin, tenant, project, fingerprint,
                         signed_expires_at, retain_until, lease, state, accepted_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT(lease) DO NOTHING",
                params![
                    storage_id,
                    origin,
                    tenant,
                    project,
                    fingerprint,
                    signed_expires_at,
                    retain_until,
                    lease,
                    PENDING,
                    effective_now,
                ],
            ) {
                Ok(inserted) => inserted,
                Err(error) => {
                    // SQLITE_FULL cannot distinguish our page limit from a full
                    // filesystem. Do not invent a retryable quota classification
                    // or query a transaction SQLite may already have rolled back.
                    return Err(sqlite_unavailable("reserve insert", &error));
                }
            };
            if inserted == 1 {
                inserted_lease = Some(lease);
                break;
            }
        }
        let lease = inserted_lease
            .ok_or_else(|| StoreError::Unavailable("random relay lease collision".to_string()))?;
        update_last_observed(&transaction, effective_now)?;
        if database_exceeds_bound(&transaction, allowed_pages)? {
            return Err(StoreError::Unavailable(
                "relay replay database exceeded its page bound".to_string(),
            ));
        }
        transaction
            .commit()
            .map_err(|error| sqlite_unavailable("reserve commit", &error))?;
        Ok(Reservation::Reserved(lease))
    }

    /// Mark a matching pending reservation completed.
    pub(crate) fn complete(&self, storage_id: &str, opaque_lease: &str) -> Result<(), StoreError> {
        self.transition(storage_id, opaque_lease, true, None)
    }

    /// Publish response bytes and the completion proof in the same transaction.
    /// Callers retain Pending on failure: an external effect may already exist.
    pub(crate) fn complete_with_response(
        &self,
        storage_id: &str,
        opaque_lease: &str,
        body: &str,
    ) -> Result<(), StoreError> {
        if body.is_empty() || body.len() > 64 * 1024 {
            return Err(StoreError::InvalidInput(
                "invalid task response size".into(),
            ));
        }
        self.transition(storage_id, opaque_lease, true, Some(body))
    }

    /// Return only an exact, completed delivery's response. Missing, pending,
    /// foreign-scope and changed-content lookups expose no response bytes.
    /// The origin must still verify the signature and its current expiry.
    pub(crate) fn completed_response(
        &self,
        storage_id: &str,
        origin: &str,
        tenant: &str,
        project: &str,
        fingerprint: &str,
    ) -> Result<Option<String>, StoreError> {
        validate_digest(storage_id, "storage_id")?;
        validate_digest(fingerprint, "fingerprint")?;
        validate_scope(origin, "origin")?;
        validate_scope(tenant, "tenant")?;
        validate_scope(project, "project")?;
        let connection = self.open_for_operation()?;
        let response: Option<Option<String>> = connection
            .query_row(
                "SELECT CASE WHEN length(CAST(response.body AS BLOB)) BETWEEN 1 AND 65536
                         THEN response.body ELSE NULL END
             FROM relay_task_responses AS response
             JOIN relay_replay AS proof ON proof.storage_id = response.storage_id
             WHERE proof.storage_id = ?1 AND proof.origin = ?2 AND proof.tenant = ?3
               AND proof.project = ?4 AND proof.fingerprint = ?5 AND proof.state = ?6",
                params![storage_id, origin, tenant, project, fingerprint, COMPLETED],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| sqlite_unavailable("read completed task response", &error))?;
        response
            .map(|body| {
                body.ok_or_else(|| {
                    StoreError::Unavailable("stored task response violates bound".into())
                })
            })
            .transpose()
    }

    /// Release a matching pending reservation without evicting a completed proof.
    pub(crate) fn release(&self, storage_id: &str, opaque_lease: &str) -> Result<(), StoreError> {
        self.transition(storage_id, opaque_lease, false, None)
    }

    pub(crate) fn database_path(&self) -> &Path {
        &self.database_path
    }

    fn transition(
        &self,
        storage_id: &str,
        opaque_lease: &str,
        complete: bool,
        response: Option<&str>,
    ) -> Result<(), StoreError> {
        validate_digest(storage_id, "storage_id")?;
        validate_lease(opaque_lease)?;
        let mut connection = self.open_for_operation()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| sqlite_unavailable("lease transaction", &error))?;
        let existing = transaction
            .query_row(
                "SELECT state, lease FROM relay_replay WHERE storage_id = ?1",
                params![storage_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| sqlite_unavailable("lease lookup", &error))?;
        let Some((state, stored_lease)) = existing else {
            return Err(StoreError::LeaseMismatch);
        };
        if stored_lease != opaque_lease {
            return Err(StoreError::LeaseMismatch);
        }
        if complete && state == COMPLETED {
            let matches: bool = transaction.query_row(
                "SELECT CASE WHEN ?2 IS NULL
                   THEN NOT EXISTS (SELECT 1 FROM relay_task_responses WHERE storage_id = ?1)
                   ELSE EXISTS (SELECT 1 FROM relay_task_responses WHERE storage_id = ?1 AND body = ?2)
                 END",
                params![storage_id, response], |row| row.get(0),
            ).map_err(|error| sqlite_unavailable("compare completed response", &error))?;
            if !matches {
                return Err(StoreError::LeaseMismatch);
            }
            transaction
                .commit()
                .map_err(|error| sqlite_unavailable("idempotent completion", &error))?;
            return Ok(());
        }
        if state != PENDING {
            return Err(StoreError::LeaseMismatch);
        }

        let changed = if complete {
            if let Some(body) = response {
                transaction
                    .execute(
                        "INSERT INTO relay_task_responses (storage_id, body) VALUES (?1, ?2)",
                        params![storage_id, body],
                    )
                    .map_err(|error| sqlite_unavailable("persist task response", &error))?;
            }
            transaction
                .execute(
                    "UPDATE relay_replay SET state = ?1
                     WHERE storage_id = ?2 AND lease = ?3 AND state = ?4",
                    params![COMPLETED, storage_id, opaque_lease, PENDING],
                )
                .map_err(|error| sqlite_unavailable("complete relay proof", &error))?
        } else {
            transaction
                .execute(
                    "DELETE FROM relay_replay
                     WHERE storage_id = ?1 AND lease = ?2 AND state = ?3",
                    params![storage_id, opaque_lease, PENDING],
                )
                .map_err(|error| sqlite_unavailable("release relay proof", &error))?
        };
        if changed != 1 {
            return Err(StoreError::LeaseMismatch);
        }
        transaction
            .commit()
            .map_err(|error| sqlite_unavailable("lease commit", &error))
    }

    fn open_for_operation(&self) -> Result<Connection, StoreError> {
        reject_database_sidecars(&self.database_path)?;
        if read_creation_marker(&self.database_path.with_extension("established"))?.as_deref()
            != Some(self.creation_marker.as_str())
        {
            return Err(StoreError::Unavailable(
                "relay replay established marker missing or changed".into(),
            ));
        }
        let mut connection = open_connection(&self.database_path, false)?;
        if validate_schema(&connection)?.creation_marker != self.creation_marker {
            return Err(StoreError::Unavailable(
                "relay replay database identity changed".into(),
            ));
        }
        configure_connection(&mut connection, self.limits.max_database_bytes, false)?;
        enforce_database_bound(&connection, self.limits.max_database_bytes)?;
        Ok(connection)
    }
}

#[derive(Debug)]
struct StoredMetadata {
    retention_seconds: i64,
    creation_marker: String,
}

fn read_creation_marker(path: &Path) -> Result<Option<String>, StoreError> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(io_unavailable(
                "read relay replay established marker",
                error,
            ));
        }
    };
    let metadata = file
        .metadata()
        .map_err(|error| io_unavailable("marker metadata", error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(StoreError::Unavailable(
            "relay replay marker must be a regular file".into(),
        ));
    }
    let mut marker = String::new();
    file.take((MAX_LEASE_BYTES + 1) as u64)
        .read_to_string(&mut marker)
        .map_err(|error| io_unavailable("read relay replay marker bytes", error))?;
    if marker.len() != MAX_LEASE_BYTES
        || !marker
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(StoreError::Unavailable(
            "invalid relay replay established marker".into(),
        ));
    }
    Ok(Some(marker))
}

fn create_creation_marker(path: &Path) -> Result<String, StoreError> {
    let marker = new_lease()?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| io_unavailable("create relay replay established marker", error))?;
    file.write_all(marker.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| io_unavailable("persist relay replay established marker", error))?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| io_unavailable("sync relay replay marker directory", error))?;
    }
    // Keep the marker after any later initialization failure: fail closed on
    // uncertain state rather than deleting evidence and silently starting over.
    Ok(marker)
}

#[derive(Debug)]
struct ReplayEntry {
    origin: String,
    tenant: String,
    project: String,
    fingerprint: String,
    state: i64,
}

/// Reject a retention that is below the signed relay deadline or outside the
/// finite horizon. Called before any filesystem or database work at
/// construction, so an invalid value leaves no state behind, and again on the
/// persisted value so a tampered database cannot smuggle one back in.
fn validate_retention(retention_seconds: i64) -> Result<(), StoreError> {
    if !(MIN_RETENTION_SECONDS..=MAX_RETENTION_SECONDS).contains(&retention_seconds) {
        return Err(StoreError::InvalidInput(format!(
            "retention must be between {MIN_RETENTION_SECONDS} and \
             {MAX_RETENTION_SECONDS} seconds"
        )));
    }
    Ok(())
}

fn validate_digest(value: &str, name: &str) -> Result<(), StoreError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(StoreError::InvalidInput(format!(
            "{name} must be exactly 64 lowercase hexadecimal bytes"
        )));
    }
    Ok(())
}

fn validate_scope(value: &str, name: &str) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > 128 {
        return Err(StoreError::InvalidInput(format!(
            "{name} must be between 1 and 128 bytes"
        )));
    }
    Ok(())
}

fn validate_lease(value: &str) -> Result<(), StoreError> {
    if value.len() != MAX_LEASE_BYTES || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(StoreError::InvalidInput(
            "opaque relay lease is malformed".to_string(),
        ));
    }
    Ok(())
}

fn database_state(path: &Path) -> Result<bool, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(StoreError::Unavailable(format!(
                    "relay replay database must not be a symlink: {}",
                    path.display()
                )));
            }
            if !metadata.is_file() {
                return Err(StoreError::Unavailable(format!(
                    "relay replay database is not a regular file: {}",
                    path.display()
                )));
            }
            if metadata.len() == 0 {
                return Err(StoreError::Unavailable(
                    "empty established relay replay database refused".to_string(),
                ));
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_unavailable("relay replay database metadata", &error)),
    }
}

fn open_connection(path: &Path, create: bool) -> Result<Connection, StoreError> {
    if create {
        create_private_database(path)?;
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW
        | OpenFlags::SQLITE_OPEN_EXRESCODE;
    Connection::open_with_flags(path, flags)
        .map_err(|error| sqlite_unavailable("open relay replay database", &error))
}

fn configure_connection(
    connection: &mut Connection,
    max_database_bytes: u64,
    initializing: bool,
) -> Result<(), StoreError> {
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(|error| sqlite_unavailable("enable response ownership constraint", &error))?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| sqlite_unavailable("configure SQLite busy timeout", &error))?;
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|error| sqlite_unavailable("read SQLite journal mode", &error))?;
    if !journal_mode.eq_ignore_ascii_case("delete") {
        if !initializing {
            return Err(StoreError::Unavailable(format!(
                "relay replay database uses unsupported journal mode {journal_mode}"
            )));
        }
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .map_err(|error| sqlite_unavailable("set SQLite rollback journal", &error))?;
    }
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(|error| sqlite_unavailable("set SQLite synchronous mode", &error))?;
    if initializing {
        connection
            .pragma_update(None, "page_size", DATABASE_PAGE_SIZE as i64)
            .map_err(|error| sqlite_unavailable("set SQLite page size", &error))?;
    }
    let requested_pages = max_database_bytes / DATABASE_PAGE_SIZE;
    if requested_pages == 0 || requested_pages > i64::MAX as u64 {
        return Err(StoreError::InvalidInput(
            "relay replay database byte bound has no usable pages".to_string(),
        ));
    }
    if initializing {
        connection
            .pragma_update(None, "max_page_count", requested_pages as i64)
            .map_err(|error| sqlite_unavailable("set SQLite page bound", &error))?;
    }
    Ok(())
}

fn initialize_schema(
    connection: &mut Connection,
    retention_seconds: i64,
    marker: &str,
) -> Result<(), StoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite_unavailable("initialize relay replay schema", &error))?;
    transaction
        .execute_batch(&format!(
            "{CREATE_META_TABLE};{CREATE_REPLAY_TABLE};\
             {CREATE_RETAIN_INDEX};{CREATE_ORIGIN_INDEX};{CREATE_RESPONSE_TABLE};"
        ))
        .map_err(|error| sqlite_unavailable("create relay replay schema", &error))?;
    transaction
        .execute(
            "INSERT INTO relay_replay_meta
                 (id, schema_version, creation_marker, retention_seconds, last_observed_at)
             VALUES (1, ?1, ?2, ?3, 0)",
            params![SCHEMA_VERSION, marker, retention_seconds],
        )
        .map_err(|error| sqlite_unavailable("write relay replay marker", &error))?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|error| sqlite_unavailable("write relay replay schema version", &error))?;
    transaction
        .commit()
        .map_err(|error| sqlite_unavailable("commit relay replay schema", &error))?;
    Ok(())
}

fn migrate_response_schema(
    connection: &mut Connection,
    expected_marker: &str,
) -> Result<(), StoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| sqlite_unavailable("begin response schema migration", &error))?;
    let version: i64 = transaction
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| sqlite_unavailable("read response schema version", &error))?;
    if !matches!(version, 1 | SCHEMA_VERSION) {
        return Err(StoreError::Unavailable(
            "unsupported replay migration version".into(),
        ));
    }
    let metadata = validate_schema_version(&transaction, version)?;
    if metadata.creation_marker != expected_marker {
        return Err(StoreError::Unavailable(
            "replay migration identity mismatch".into(),
        ));
    }
    if version == 1 {
        // Existing rows, leases, clock floor and retention are never rewritten.
        // DDL and both version markers commit together or roll back together.
        transaction
            .execute_batch(CREATE_RESPONSE_TABLE)
            .map_err(|error| sqlite_unavailable("create response schema", &error))?;
        transaction
            .execute(
                "UPDATE relay_replay_meta SET schema_version = ?1 WHERE id = 1",
                params![SCHEMA_VERSION],
            )
            .map_err(|error| sqlite_unavailable("update response schema marker", &error))?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|error| sqlite_unavailable("update response schema version", &error))?;
        validate_schema(&transaction)?;
    }
    transaction
        .commit()
        .map_err(|error| sqlite_unavailable("commit response schema migration", &error))
}

fn validate_schema(connection: &Connection) -> Result<StoredMetadata, StoreError> {
    validate_schema_version(connection, SCHEMA_VERSION)
}

fn validate_schema_version(
    connection: &Connection,
    expected_version: i64,
) -> Result<StoredMetadata, StoreError> {
    let user_version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| sqlite_unavailable("read relay replay schema version", &error))?;
    if user_version != expected_version {
        return Err(StoreError::Unavailable(
            "relay replay database schema version mismatch".to_string(),
        ));
    }
    let metadata = connection
        .query_row(
            "SELECT schema_version, creation_marker, retention_seconds
             FROM relay_replay_meta WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| sqlite_unavailable("read relay replay marker", &error))?
        .ok_or_else(|| {
            StoreError::Unavailable(
                "established relay replay database has no creation marker".to_string(),
            )
        })?;
    if metadata.0 != expected_version
        || metadata.1.len() != MAX_LEASE_BYTES
        || !metadata
            .1
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(StoreError::Unavailable(
            "relay replay creation marker or schema version is invalid".to_string(),
        ));
    }
    validate_retention(metadata.2)?;

    let meta_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM relay_replay_meta", [], |row| {
            row.get(0)
        })
        .map_err(|error| sqlite_unavailable("validate relay replay metadata", &error))?;
    if meta_count != 1 {
        return Err(StoreError::Unavailable(
            "relay replay metadata cardinality mismatch".to_string(),
        ));
    }
    validate_object_definitions(connection, expected_version)?;
    Ok(StoredMetadata {
        retention_seconds: metadata.2,
        creation_marker: metadata.1,
    })
}

/// Prove that the established database still carries the exact schema this
/// store depends on.
///
/// Comparing column names alone accepts a same-name database whose tables were
/// recreated without PRIMARY KEY, without UNIQUE(lease), without CHECK(state),
/// with different column types, or with differently defined indexes; such a
/// database silently permits duplicate storage_id rows, and `find_entry` would
/// then answer from an arbitrary duplicate. The stored DDL from sqlite_master
/// is therefore compared (whitespace-normalised) against the canonical
/// statements, and any object beyond the four we create — an extra table, view,
/// trigger, or index — is refused. SQLite's own `sqlite_%` internal entries,
/// including the autoindexes implied by the primary key and the unique lease,
/// are not user-definable and are skipped.
fn validate_object_definitions(connection: &Connection, version: i64) -> Result<(), StoreError> {
    let mut expected = vec![
        ("relay_replay_meta", CREATE_META_TABLE),
        ("relay_replay", CREATE_REPLAY_TABLE),
        ("relay_replay_retain_idx", CREATE_RETAIN_INDEX),
        ("relay_replay_origin_state_idx", CREATE_ORIGIN_INDEX),
    ];
    if version == SCHEMA_VERSION {
        expected.push(("relay_task_responses", CREATE_RESPONSE_TABLE));
    }
    let mut statement = connection
        .prepare("SELECT name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'")
        .map_err(|error| sqlite_unavailable("read relay replay schema", &error))?;
    let objects = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(|error| sqlite_unavailable("read relay replay schema", &error))?;

    if objects.len() != expected.len() {
        return Err(StoreError::Unavailable(
            "relay replay database defines unexpected schema objects".to_string(),
        ));
    }
    for (name, canonical) in expected {
        let actual = objects
            .iter()
            .find(|(object, _)| object == name)
            .and_then(|(_, sql)| sql.as_deref())
            .ok_or_else(|| {
                StoreError::Unavailable(format!("relay replay schema object missing: {name}"))
            })?;
        if normalize_sql(actual) != normalize_sql(canonical) {
            return Err(StoreError::Unavailable(format!(
                "relay replay schema definition mismatch: {name}"
            )));
        }
    }
    Ok(())
}

/// Collapse the whitespace SQLite preserves verbatim in stored DDL so the
/// comparison is about the definition, not about indentation.
fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn quick_check(connection: &Connection) -> Result<(), StoreError> {
    let result: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|error| sqlite_unavailable("check relay replay database integrity", &error))?;
    if result != "ok" {
        return Err(StoreError::Unavailable(format!(
            "relay replay database integrity check failed: {result}"
        )));
    }
    Ok(())
}

fn enforce_database_bound(
    connection: &Connection,
    max_database_bytes: u64,
) -> Result<u64, StoreError> {
    let page_size: i64 = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .map_err(|error| sqlite_unavailable("read relay replay page size", &error))?;
    if page_size <= 0 {
        return Err(StoreError::Unavailable(
            "relay replay database has invalid page size".to_string(),
        ));
    }
    let requested_pages = max_database_bytes / page_size as u64;
    if requested_pages == 0 || requested_pages > i64::MAX as u64 {
        return Err(StoreError::InvalidInput(
            "relay replay database byte bound has no usable pages".to_string(),
        ));
    }
    let established_pages: i64 = connection
        .pragma_query_value(None, "max_page_count", |row| row.get(0))
        .map_err(|error| sqlite_unavailable("read relay replay page bound", &error))?;
    if established_pages <= 0 {
        return Err(StoreError::Unavailable(
            "relay replay database has invalid page bound".to_string(),
        ));
    }
    let allowed_pages = requested_pages.min(established_pages as u64);
    if allowed_pages < established_pages as u64 {
        connection
            .pragma_update(None, "max_page_count", allowed_pages as i64)
            .map_err(|error| sqlite_unavailable("lower relay replay page bound", &error))?;
    }
    let page_count: i64 = connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .map_err(|error| sqlite_unavailable("read relay replay page count", &error))?;
    if page_count < 0 || page_count as u64 > allowed_pages {
        return Err(StoreError::Unavailable(
            "relay replay database exceeds its configured byte bound".to_string(),
        ));
    }
    Ok(allowed_pages)
}

fn page_count(transaction: &Transaction<'_>) -> Result<u64, StoreError> {
    let pages: i64 = transaction
        .query_row("PRAGMA page_count", [], |row| row.get(0))
        .map_err(|error| sqlite_unavailable("read relay replay page count", &error))?;
    u64::try_from(pages)
        .map_err(|_| StoreError::Unavailable("relay replay page count is invalid".to_string()))
}

fn database_exceeds_bound(
    transaction: &Transaction<'_>,
    allowed_pages: u64,
) -> Result<bool, StoreError> {
    Ok(page_count(transaction)? > allowed_pages)
}

fn read_last_observed(transaction: &Transaction<'_>) -> Result<i64, StoreError> {
    let observed: i64 = transaction
        .query_row(
            "SELECT last_observed_at FROM relay_replay_meta WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|error| sqlite_unavailable("read relay replay monotonic clock", &error))?;
    if !(0..=i64::MAX - MAX_RETENTION_SECONDS).contains(&observed) {
        return Err(StoreError::Unavailable(
            "persisted relay replay clock is outside its supported range".into(),
        ));
    }
    Ok(observed)
}

fn update_last_observed(transaction: &Transaction<'_>, now: i64) -> Result<(), StoreError> {
    transaction
        .execute(
            "UPDATE relay_replay_meta
             SET last_observed_at = CASE
                 WHEN last_observed_at < ?1 THEN ?1 ELSE last_observed_at END
             WHERE id = 1",
            params![now],
        )
        .map_err(|error| sqlite_unavailable("persist relay replay monotonic clock", &error))?;
    Ok(())
}

/// Delete completed proofs whose retention has elapsed.
///
/// Only COMPLETED rows are pruned, and that is deliberate. A pending row means
/// a delivery was accepted and its downstream effect is unknown; expiring it
/// would let the same relay be delivered a second time after a crash. Such a
/// row is released by an operator calling `complete` or `release` after
/// reconciling the effect — never by the clock. Capacity held by pending rows
/// is therefore recovered manually, and callers must not read the retry hint as
/// a promise that waiting will free it.
fn prune_expired(transaction: &Transaction<'_>, now: i64) -> Result<(), StoreError> {
    transaction
        .execute(
            "DELETE FROM relay_replay
             WHERE state = ?1 AND retain_until <= ?2",
            params![COMPLETED, now],
        )
        .map_err(|error| sqlite_unavailable("prune expired relay proofs", &error))?;
    Ok(())
}

fn find_entry(
    transaction: &Transaction<'_>,
    storage_id: &str,
) -> Result<Option<ReplayEntry>, StoreError> {
    transaction
        .query_row(
            "SELECT origin, tenant, project, fingerprint, state
             FROM relay_replay WHERE storage_id = ?1",
            params![storage_id],
            |row| {
                Ok(ReplayEntry {
                    origin: row.get(0)?,
                    tenant: row.get(1)?,
                    project: row.get(2)?,
                    fingerprint: row.get(3)?,
                    state: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|error| sqlite_unavailable("read relay replay proof", &error))
}

fn count_rows(transaction: &Transaction<'_>, origin: Option<&str>) -> Result<u64, StoreError> {
    let count: i64 = match origin {
        Some(origin) => transaction
            .query_row(
                "SELECT COUNT(*) FROM relay_replay WHERE origin = ?1",
                params![origin],
                |row| row.get(0),
            )
            .map_err(|error| sqlite_unavailable("count relay replay rows", &error))?,
        None => transaction
            .query_row("SELECT COUNT(*) FROM relay_replay", [], |row| row.get(0))
            .map_err(|error| sqlite_unavailable("count relay replay rows", &error))?,
    };
    u64::try_from(count)
        .map_err(|_| StoreError::Unavailable("relay replay row count is invalid".to_string()))
}

fn retry_after_seconds(
    transaction: &Transaction<'_>,
    origin: Option<&str>,
    now: i64,
) -> Result<u64, StoreError> {
    let deadline: Option<i64> = match origin {
        Some(origin) => transaction
            .query_row(
                "SELECT MIN(retain_until) FROM relay_replay
                 WHERE origin = ?1 AND state = ?2",
                params![origin, COMPLETED],
                |row| row.get(0),
            )
            .map_err(|error| sqlite_unavailable("calculate relay replay retry", &error))?,
        None => transaction
            .query_row(
                "SELECT MIN(retain_until) FROM relay_replay WHERE state = ?1",
                params![COMPLETED],
                |row| row.get(0),
            )
            .map_err(|error| sqlite_unavailable("calculate relay replay retry", &error))?,
    };
    // No completed proof is scheduled to expire: the capacity is held by
    // pending rows, which only operator reconciliation releases. Report a
    // back-off instead of a one-second hint that would imply recovery.
    Ok(
        deadline.map_or(UNRECLAIMABLE_RETRY_AFTER_SECONDS, |deadline| {
            deadline.saturating_sub(now).max(1) as u64
        }),
    )
}

fn new_lease() -> Result<String, StoreError> {
    let mut bytes = [0_u8; NONCE_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|error| StoreError::Unavailable(format!("generate relay lease: {error}")))?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut lease = String::with_capacity(NONCE_BYTES * 2);
    for byte in bytes {
        lease.push(HEX[(byte >> 4) as usize] as char);
        lease.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(lease)
}

fn reject_database_sidecars(database_path: &Path) -> Result<(), StoreError> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{}", database_path.display(), suffix));
        reject_symlink(&sidecar, "relay replay database sidecar")?;
        if suffix != "-journal" && path_len(&sidecar)? > 0 {
            return Err(StoreError::Unavailable(format!(
                "unexpected SQLite {suffix} sidecar for rollback-journal database"
            )));
        }
    }
    Ok(())
}

fn reject_symlink(path: &Path, description: &str) -> Result<(), StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StoreError::Unavailable(format!(
            "{description} must not be a symlink: {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_unavailable(description, &error)),
    }
}

fn path_len(path: &Path) -> Result<u64, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(io_unavailable("inspect relay replay path", &error)),
    }
}

fn canonicalize_existing_path(path: &Path) -> Result<PathBuf, StoreError> {
    match fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(error) => Err(io_unavailable("canonicalize relay replay root", &error)),
    }
}

/// Exclusively create the empty file before SQLite opens it without CREATE.
/// Unix permissions are private from creation, without changing process umask.
/// Other platforms retain their inherited ACLs; parent directories are trusted.
fn create_private_database(path: &Path) -> Result<(), StoreError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|error| io_unavailable("create private relay replay database", error))?;
    file.sync_all()
        .map_err(|error| io_unavailable("sync new relay replay database", error))?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| io_unavailable("sync relay replay database directory", error))?;
    }
    Ok(())
}

/// Create one directory, owner-only when `private`. A racing creator is
/// tolerated; the caller re-verifies the result.
fn create_directory(path: &Path, private: bool) -> Result<(), StoreError> {
    #[cfg_attr(not(unix), allow(unused_mut))] // only the unix branch mutates it
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    #[cfg(not(unix))]
    let _ = private;
    builder
        .create(path)
        .or_else(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(error)
            }
        })
        .map_err(|error| io_unavailable("create relay replay directory", error))
}

/// Walk `path` component by component, refusing symlinks and non-directories,
/// creating what is missing. With `private_leaf`, a final component created by
/// this call is made owner-only; components that already exist, and every
/// parent, keep the permissions they have.
fn ensure_directory_no_symlinks(path: &Path, private_leaf: bool) -> Result<(), StoreError> {
    let last = path.components().count().saturating_sub(1);
    let mut current = PathBuf::new();
    for (index, component) in path.components().enumerate() {
        match component {
            // A drive/UNC prefix and the root are volume roots: never created
            // here, and on Windows `symlink_metadata` of a bare prefix (`C:`,
            // `\\?\C:`) fails with ERROR_INVALID_FUNCTION. Push the platform's
            // own separator so verbatim paths stay valid.
            Component::Prefix(_) | Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            Component::CurDir => current.push("."),
            Component::ParentDir => current.push(".."),
            Component::Normal(name) => current.push(name),
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(StoreError::Unavailable(format!(
                        "relay replay directory must not contain symlinks: {}",
                        current.display()
                    )));
                }
                if !metadata.is_dir() {
                    return Err(StoreError::Unavailable(format!(
                        "relay replay path is not a directory: {}",
                        current.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_directory(&current, private_leaf && index == last)?;
                let metadata = fs::symlink_metadata(&current).map_err(|metadata_error| {
                    io_unavailable("verify relay replay directory", metadata_error)
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(StoreError::Unavailable(format!(
                        "relay replay directory changed during creation: {}",
                        current.display()
                    )));
                }
            }
            Err(error) => return Err(io_unavailable("inspect relay replay directory", &error)),
        }
    }
    Ok(())
}

fn io_unavailable(context: &str, error: impl Display) -> StoreError {
    StoreError::Unavailable(format!("{context}: {error}"))
}

fn sqlite_unavailable(context: &str, error: &SqliteError) -> StoreError {
    let classification = match error.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => "SQLite busy/locked",
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => "SQLite corruption",
        Some(ErrorCode::DiskFull) => "SQLite or filesystem full",
        _ => "SQLite failure",
    };
    StoreError::Unavailable(format!("{context}: {classification}: {error}"))
}
