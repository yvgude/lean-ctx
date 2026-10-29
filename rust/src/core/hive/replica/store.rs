// SPDX-License-Identifier: Apache-2.0
//! The local replica of one place (SPEC-ENGINE §3.4.1):
//! `<data>/hive/replica/<place_id>/replica.db`, SQLite in WAL mode with
//! `secure_delete`, every text column encrypted under the place's replica
//! key `rk` ([`super::crypt`]). Ids, node types and edge structure stay
//! plaintext so folding and graph queries never decrypt.
//!
//! Keys that could confirm a guessed statement (`dedupe_keys`,
//! `purge_deny`) are stored as `blake3_keyed(rk, dk)`, so a copied
//! `replica.db` without `mk` reveals nothing about known statements.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension as _, params};

use super::crypt::{self, Key};
use crate::core::hive::HiveError;
use crate::core::hive::capture::dedupe::Signature;

/// Schema version this engine writes (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA_V1: &str = "
CREATE TABLE events (
    seq INTEGER PRIMARY KEY,
    event_id TEXT NOT NULL UNIQUE,
    op TEXT NOT NULL,
    target TEXT NOT NULL,
    hlc_wall INTEGER NOT NULL,
    hlc_ctr INTEGER NOT NULL,
    provenance TEXT,
    review_state TEXT,
    reviewed_by TEXT,
    batch_id TEXT,
    purged INTEGER NOT NULL DEFAULT 0,
    sig_invalid INTEGER NOT NULL DEFAULT 0,
    withheld INTEGER NOT NULL DEFAULT 0,
    key_lost INTEGER NOT NULL DEFAULT 0,
    converted INTEGER NOT NULL DEFAULT 0,
    body_ct BLOB,
    ck_ct BLOB,
    worker_id_ct BLOB,
    envelope_ct BLOB,
    leaf_ct BLOB
);
CREATE TABLE pending (
    event_id TEXT PRIMARY KEY,
    hlc_wall INTEGER NOT NULL,
    hlc_ctr INTEGER NOT NULL,
    outbox_id TEXT,
    event_ct BLOB NOT NULL
);
CREATE TABLE nodes (
    node_id TEXT PRIMARY KEY,
    node_type TEXT NOT NULL,
    last_seq INTEGER NOT NULL,
    retracted INTEGER NOT NULL DEFAULT 0,
    bucket TEXT,
    provenance TEXT,
    review_state TEXT,
    reviewed_by TEXT,
    fields_ct BLOB
);
CREATE TABLE edges (
    edge_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    last_seq INTEGER NOT NULL,
    retracted INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX edges_from ON edges(from_id);
CREATE INDEX edges_to ON edges(to_id);
CREATE TABLE dedupe_keys (key BLOB PRIMARY KEY, node_id TEXT NOT NULL);
CREATE TABLE minhash (node_id TEXT PRIMARY KEY, sig_ct BLOB NOT NULL);
CREATE TABLE purge_deny (key BLOB PRIMARY KEY, reason TEXT NOT NULL);
CREATE TABLE bus_cache (
    bus_kind TEXT NOT NULL,
    bus_ws TEXT NOT NULL,
    bus_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    fetched_at INTEGER NOT NULL,
    msg_ct BLOB NOT NULL,
    PRIMARY KEY (bus_kind, bus_ws, bus_id, seq)
);
CREATE TABLE own_sources (
    node_id TEXT PRIMARY KEY,
    source_kind TEXT NOT NULL,
    source_ref_ct BLOB NOT NULL
);
CREATE TABLE meta (k TEXT PRIMARY KEY, v BLOB NOT NULL);
";

/// The four purge reasons (owner decision 23.10.).
pub const PURGE_REASONS: [&str; 4] = [
    "leaked_secret",
    "gdpr_erasure",
    "retention_schedule",
    "illegal_content",
];

/// Where an own-authored node's text came from (for the local-store purge).
pub const SOURCE_KINDS: [&str; 4] = ["fact", "diary", "session_finding", "episode"];

/// Meta keys; every value is encrypted except the wrapped `rk`.
const META_RK: &str = "rk_wrapped";
pub const META_CURSOR: &str = "cursor";
pub const META_SNAPSHOT_REV: &str = "snapshot_rev";
pub const META_PLACE_NAME: &str = "place_name";
pub const META_ID_SALT: &str = "id_salt";
pub const META_EPOCH: &str = "epoch";
const META_PURGE_ACKS: &str = "purge_acks";

/// Purge acknowledgements of a place (E9 / `purge_acks[]` on E5).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PurgeAcks {
    /// Made and not yet accepted: `(purge_id, seq, applied_at)`.
    pub pending: Vec<(String, u64, u64)>,
    /// Accepted by the place: `(purge_id, seq)`.
    pub sent: std::collections::BTreeSet<(String, u64)>,
}

/// How a replica came to be open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    Existing,
    Created,
    /// `rk` no longer unwrapped (`replica_key_missing`): the place was
    /// deleted and starts empty; the next sync clones it again.
    RebuiltKeyMissing,
}

/// An open replica.
pub struct Replica {
    pub(super) conn: Connection,
    place_id: String,
    rk: Key,
    dir: PathBuf,
}

impl std::fmt::Debug for Replica {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Replica")
            .field("place_id", &self.place_id)
            .finish_non_exhaustive()
    }
}

#[allow(clippy::needless_pass_by_value)] // used as `map_err(db_err)`
pub(super) fn db_err(e: rusqlite::Error) -> HiveError {
    HiveError::Local(format!("replica: {e}"))
}

/// A place id is a path segment here: letters, digits, `_` and `-` only.
fn place_ok(place_id: &str) -> bool {
    (1..=64).contains(&place_id.len())
        && place_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `<data>/hive/replica/<place_id>`.
pub fn place_dir(data_dir: &Path, place_id: &str) -> PathBuf {
    data_dir.join("replica").join(place_id)
}

fn private_dir(dir: &Path) -> Result<(), HiveError> {
    std::fs::create_dir_all(dir).map_err(|e| HiveError::Local(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| HiveError::Local(e.to_string()))?;
    }
    Ok(())
}

impl Replica {
    /// Open (or create) the replica of `place_id` with the machine key `mk`.
    pub fn open(
        data_dir: &Path,
        place_id: &str,
        mk: &[u8; 32],
    ) -> Result<(Self, Opened), HiveError> {
        if !place_ok(place_id) {
            return Err(HiveError::Protocol(format!("place id {place_id:?}")));
        }
        let dir = place_dir(data_dir, place_id);
        let existed = dir.join("replica.db").exists();
        if let Some(replica) = Self::open_at(&dir, place_id, mk)? {
            let opened = if existed {
                Opened::Existing
            } else {
                Opened::Created
            };
            return Ok((replica, opened));
        }
        std::fs::remove_dir_all(&dir).map_err(|e| HiveError::Local(e.to_string()))?;
        let replica = Self::open_at(&dir, place_id, mk)?
            .ok_or_else(|| HiveError::Local("replica: fresh key does not open".into()))?;
        Ok((replica, Opened::RebuiltKeyMissing))
    }

    /// `None` when the stored `rk` does not unwrap under `mk`.
    fn open_at(dir: &Path, place_id: &str, mk: &[u8; 32]) -> Result<Option<Self>, HiveError> {
        private_dir(dir)?;
        let path = dir.join("replica.db");
        let mut conn = Connection::open(&path).map_err(db_err)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| HiveError::Local(e.to_string()))?;
        }
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db_err)?;
        conn.pragma_update(None, "secure_delete", "ON")
            .map_err(db_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(db_err)?;
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(db_err)?;
        if version > SCHEMA_VERSION {
            return Err(HiveError::Config(format!(
                "engine_too_old: replica schema {version}, this engine knows {SCHEMA_VERSION}"
            )));
        }
        if version == 0 {
            let rk = crypt::new_replica_key()?;
            let wrapped = crypt::wrap(mk, place_id, &rk)?;
            // Schema, wrapped key and version in one transaction: a crash in
            // between leaves version 0 and the next open starts over.
            let tx = conn.transaction().map_err(db_err)?;
            tx.execute_batch(SCHEMA_V1).map_err(db_err)?;
            tx.execute_batch(super::history::SCHEMA).map_err(db_err)?;
            tx.execute(
                "INSERT INTO meta (k, v) VALUES (?1, ?2)",
                params![META_RK, wrapped],
            )
            .map_err(db_err)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(db_err)?;
            tx.commit().map_err(db_err)?;
            return Ok(Some(Self {
                conn,
                place_id: place_id.to_string(),
                rk,
                dir: dir.to_path_buf(),
            }));
        }
        let wrapped: Option<Vec<u8>> = conn
            .query_row("SELECT v FROM meta WHERE k = ?1", [META_RK], |r| r.get(0))
            .optional()
            .map_err(db_err)?;
        let Some(rk) = wrapped.and_then(|w| crypt::unwrap(mk, place_id, &w)) else {
            return Ok(None);
        };
        // Tables added within schema 1 before any replica was live.
        conn.execute_batch(super::history::SCHEMA).map_err(db_err)?;
        Ok(Some(Self {
            conn,
            place_id: place_id.to_string(),
            rk,
            dir: dir.to_path_buf(),
        }))
    }

    pub fn place_id(&self) -> &str {
        &self.place_id
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub(super) fn seal(
        &self,
        table: &str,
        row: &str,
        column: &str,
        plain: &[u8],
    ) -> Result<Vec<u8>, HiveError> {
        crypt::seal(
            &self.rk,
            &crypt::aad(&self.place_id, table, row, column),
            plain,
        )
    }

    pub(super) fn open_value(
        &self,
        table: &str,
        row: &str,
        column: &str,
        sealed: &[u8],
    ) -> Result<zeroize::Zeroizing<Vec<u8>>, HiveError> {
        crypt::open(
            &self.rk,
            &crypt::aad(&self.place_id, table, row, column),
            sealed,
        )
    }

    /// `blake3_keyed(rk, dk)`: the stored form of a dedupe or purge key.
    pub(super) fn keyed(&self, dk: &[u8; 32]) -> [u8; 32] {
        *blake3::keyed_hash(&self.rk, dk).as_bytes()
    }

    // ---- meta -----------------------------------------------------------

    pub fn set_meta(&self, key: &str, value: &[u8]) -> Result<(), HiveError> {
        if key == META_RK {
            return Err(HiveError::Local("replica: rk is not meta data".into()));
        }
        let ct = self.seal("meta", key, "v", value)?;
        self.conn
            .execute(
                "INSERT INTO meta (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = ?2",
                params![key, ct],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn meta(&self, key: &str) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, HiveError> {
        if key == META_RK {
            return Ok(None);
        }
        let ct: Option<Vec<u8>> = self
            .conn
            .query_row("SELECT v FROM meta WHERE k = ?1", [key], |r| r.get(0))
            .optional()
            .map_err(db_err)?;
        ct.map(|ct| self.open_value("meta", key, "v", &ct))
            .transpose()
    }

    fn meta_u64(&self, key: &str) -> Result<Option<u64>, HiveError> {
        Ok(self.meta(key)?.and_then(|v| {
            let bytes: [u8; 8] = v.as_slice().try_into().ok()?;
            Some(u64::from_le_bytes(bytes))
        }))
    }

    /// Last server `seq` folded (`None` before the first clone).
    pub fn cursor(&self) -> Result<Option<u64>, HiveError> {
        self.meta_u64(META_CURSOR)
    }

    /// Move the cursor; it never goes back.
    pub fn set_cursor(&self, seq: u64) -> Result<(), HiveError> {
        if self.cursor()?.is_some_and(|at| at > seq) {
            return Err(HiveError::Protocol(format!(
                "replica cursor would move back to {seq}"
            )));
        }
        self.set_meta(META_CURSOR, &seq.to_le_bytes())
    }

    pub fn epoch(&self) -> Result<Option<u64>, HiveError> {
        self.meta_u64(META_EPOCH)
    }

    pub fn set_epoch(&self, epoch: u64) -> Result<(), HiveError> {
        self.set_meta(META_EPOCH, &epoch.to_le_bytes())
    }

    /// The place's dedupe key `pk_id` once the server delivered `id_salt`.
    pub fn id_salt(&self) -> Result<Option<[u8; 32]>, HiveError> {
        Ok(self
            .meta(META_ID_SALT)?
            .and_then(|v| v.as_slice().try_into().ok()))
    }

    pub fn set_id_salt(&self, salt: &[u8; 32]) -> Result<(), HiveError> {
        self.set_meta(META_ID_SALT, salt)
    }

    // ---- dedupe and purge deny -----------------------------------------

    pub fn add_dedupe_key(&self, dk: &[u8; 32], node_id: &str) -> Result<(), HiveError> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO dedupe_keys (key, node_id) VALUES (?1, ?2)",
                params![self.keyed(dk).as_slice(), node_id],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// The node an exact dedupe key belongs to.
    pub fn dedupe_node(&self, dk: &[u8; 32]) -> Result<Option<String>, HiveError> {
        self.conn
            .query_row(
                "SELECT node_id FROM dedupe_keys WHERE key = ?1",
                [self.keyed(dk).as_slice()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_err)
    }

    pub fn deny_purged(&self, dk: &[u8; 32], reason: &str) -> Result<(), HiveError> {
        if !PURGE_REASONS.contains(&reason) {
            return Err(HiveError::Protocol(format!("purge reason {reason:?}")));
        }
        self.conn
            .execute(
                "INSERT OR REPLACE INTO purge_deny (key, reason) VALUES (?1, ?2)",
                params![self.keyed(dk).as_slice(), reason],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Whether a statement with this exact key was purged (capture step 3c).
    pub fn is_purged(&self, dk: &[u8; 32]) -> Result<bool, HiveError> {
        self.conn
            .query_row(
                "SELECT 1 FROM purge_deny WHERE key = ?1",
                [self.keyed(dk).as_slice()],
                |_| Ok(()),
            )
            .optional()
            .map(|r| r.is_some())
            .map_err(db_err)
    }

    /// A purge removed `node_id` (SPEC-ENGINE §3.4.6 step 3): its dedupe
    /// keys move to `purge_deny` (same keyed digest, so the statement is
    /// never captured again) and its MinHash signature is deleted.
    pub fn deny_node(&mut self, node_id: &str, reason: &str) -> Result<(), HiveError> {
        if !PURGE_REASONS.contains(&reason) {
            return Err(HiveError::Protocol(format!("purge reason {reason:?}")));
        }
        let tx = self.conn.transaction().map_err(db_err)?;
        tx.execute(
            "INSERT OR REPLACE INTO purge_deny (key, reason)
             SELECT key, ?2 FROM dedupe_keys WHERE node_id = ?1",
            params![node_id, reason],
        )
        .map_err(db_err)?;
        tx.execute("DELETE FROM dedupe_keys WHERE node_id = ?1", [node_id])
            .map_err(db_err)?;
        tx.execute("DELETE FROM minhash WHERE node_id = ?1", [node_id])
            .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// Purge acknowledgements not yet accepted by the place:
    /// `(purge_id, tombstone seq, applied_at ms)`, plus the ones already
    /// sent (an ack is made once per tombstone, its first `applied_at` kept).
    pub fn purge_acks(&self) -> Result<PurgeAcks, HiveError> {
        Ok(self
            .meta(META_PURGE_ACKS)?
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_default())
    }

    pub fn set_purge_acks(&self, acks: &PurgeAcks) -> Result<(), HiveError> {
        let bytes = serde_json::to_vec(acks).map_err(|e| HiveError::Local(e.to_string()))?;
        self.set_meta(META_PURGE_ACKS, &bytes)
    }

    // ---- minhash ---------------------------------------------------------

    pub fn put_signature(&self, node_id: &str, sig: &Signature) -> Result<(), HiveError> {
        let bytes: Vec<u8> = sig.iter().flat_map(|v| v.to_le_bytes()).collect();
        let ct = self.seal("minhash", node_id, "sig", &bytes)?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO minhash (node_id, sig_ct) VALUES (?1, ?2)",
                params![node_id, ct],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Every stored signature, decrypted, in node-id order.
    pub fn signatures(&self) -> Result<Vec<(String, Signature)>, HiveError> {
        let mut stmt = self
            .conn
            .prepare("SELECT node_id, sig_ct FROM minhash ORDER BY node_id")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (node, ct) = row.map_err(db_err)?;
            let plain = self.open_value("minhash", &node, "sig", &ct)?;
            let mut sig: Signature = [0; 64];
            if plain.len() != sig.len() * 8 {
                return Err(HiveError::Local("replica: minhash signature size".into()));
            }
            let (chunks, _) = plain.as_chunks::<8>();
            for (slot, chunk) in sig.iter_mut().zip(chunks) {
                *slot = u64::from_le_bytes(*chunk);
            }
            out.push((node, sig));
        }
        Ok(out)
    }

    // ---- pending (offline queue) ----------------------------------------

    /// An own event waiting for the server log (offline or not yet tailed).
    pub fn add_pending(
        &self,
        event_id: &str,
        hlc_wall: u64,
        hlc_ctr: u16,
        outbox_id: Option<&str>,
        event: &[u8],
    ) -> Result<(), HiveError> {
        let ct = self.seal("pending", event_id, "event", event)?;
        let wall = i64::try_from(hlc_wall).map_err(|_| HiveError::Protocol("hlc wall".into()))?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO pending (event_id, hlc_wall, hlc_ctr, outbox_id, event_ct)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![event_id, wall, hlc_ctr, outbox_id, ct],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Pending events in HLC order: `(event_id, event bytes)`.
    pub fn pending(&self) -> Result<Vec<(String, zeroize::Zeroizing<Vec<u8>>)>, HiveError> {
        let mut stmt = self
            .conn
            .prepare("SELECT event_id, event_ct FROM pending ORDER BY hlc_wall, hlc_ctr, event_id")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (id, ct) = row.map_err(db_err)?;
            let plain = self.open_value("pending", &id, "event", &ct)?;
            out.push((id, plain));
        }
        Ok(out)
    }

    /// The server log delivered `event_id`: it is no longer pending.
    pub fn settle_pending(&self, event_id: &str) -> Result<bool, HiveError> {
        self.conn
            .execute("DELETE FROM pending WHERE event_id = ?1", [event_id])
            .map(|n| n > 0)
            .map_err(db_err)
    }

    // ---- own sources -------------------------------------------------------

    /// Where an own-authored node's text came from (capture step 6).
    pub fn put_own_source(
        &self,
        node_id: &str,
        source_kind: &str,
        source_ref: &serde_json::Value,
    ) -> Result<(), HiveError> {
        if !SOURCE_KINDS.contains(&source_kind) {
            return Err(HiveError::Local(format!("source kind {source_kind:?}")));
        }
        let json = serde_json::to_vec(source_ref).map_err(|e| HiveError::Local(e.to_string()))?;
        let ct = self.seal("own_sources", node_id, "source_ref", &json)?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO own_sources (node_id, source_kind, source_ref_ct)
                 VALUES (?1, ?2, ?3)",
                params![node_id, source_kind, ct],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn own_source(
        &self,
        node_id: &str,
    ) -> Result<Option<(String, serde_json::Value)>, HiveError> {
        let row: Option<(String, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT source_kind, source_ref_ct FROM own_sources WHERE node_id = ?1",
                [node_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        let Some((kind, ct)) = row else {
            return Ok(None);
        };
        let plain = self.open_value("own_sources", node_id, "source_ref", &ct)?;
        let value = serde_json::from_slice(&plain).map_err(|e| HiveError::Local(e.to_string()))?;
        Ok(Some((kind, value)))
    }

    /// `hex(blake3_keyed(rk, value))`, the `value_digest` of a source ref.
    pub fn value_digest(&self, value: &str) -> String {
        blake3::keyed_hash(&self.rk, value.as_bytes())
            .to_hex()
            .to_string()
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
