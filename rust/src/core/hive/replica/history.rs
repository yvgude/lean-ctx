// SPDX-License-Identifier: Apache-2.0
//! Place epoch and failover repair state of a replica (SPEC-ENGINE §3.4.4,
//! RESOLUTIONS R48): the own batches pushed in the last 24 h, kept to
//! re-push them after a failover, and when each `seq` was received, to know
//! where a re-tail starts. Both live in the encrypted replica (the spec's
//! `outbox/history/` files hold the same batches; keeping them here keeps
//! signed bodies of sealed places off disk in the clear).

use rusqlite::params;

use super::store::{META_CURSOR, Replica, db_err};
use crate::core::hive::HiveError;

/// How long pushed batches and receive times are kept.
pub const KEEP_MS: u64 = 24 * 3_600_000;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS history (
    event_id TEXT PRIMARY KEY,
    batch_id TEXT NOT NULL,
    pos INTEGER NOT NULL,
    pushed_at INTEGER NOT NULL,
    event_ct BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS received (seq INTEGER PRIMARY KEY, at INTEGER NOT NULL);
";

fn i64_of(n: u64) -> Result<i64, HiveError> {
    i64::try_from(n).map_err(|_| HiveError::Protocol("value out of range".into()))
}

impl Replica {
    /// Keep a batch the place answered (whatever each event's status), in
    /// its order. An event already kept keeps its first batch.
    pub fn record_history(
        &self,
        batch_id: &str,
        pushed_at: u64,
        events: &[(&str, &[u8])],
    ) -> Result<(), HiveError> {
        for (pos, (id, bytes)) in events.iter().enumerate() {
            let ct = self.seal("history", id, "event", bytes)?;
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO history (event_id, batch_id, pos, pushed_at, event_ct)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, batch_id, i64_of(pos as u64)?, i64_of(pushed_at)?, ct],
                )
                .map_err(db_err)?;
        }
        Ok(())
    }

    /// Kept batches in push order, each event's bytes in batch order.
    pub fn history_batches(&self) -> Result<Vec<Vec<zeroize::Zeroizing<Vec<u8>>>>, HiveError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT batch_id, event_id, event_ct FROM history
                 ORDER BY pushed_at, batch_id, pos",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                ))
            })
            .map_err(db_err)?;
        let mut batches: Vec<(String, Vec<zeroize::Zeroizing<Vec<u8>>>)> = Vec::new();
        for row in rows {
            let (batch, id, ct) = row.map_err(db_err)?;
            let plain = self.open_value("history", &id, "event", &ct)?;
            match batches.last_mut() {
                Some((b, events)) if *b == batch => events.push(plain),
                _ => batches.push((batch, vec![plain])),
            }
        }
        Ok(batches.into_iter().map(|(_, e)| e).collect())
    }

    /// The bytes of an own event kept in the history.
    pub fn history_event(
        &self,
        event_id: &str,
    ) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, HiveError> {
        let ct: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT event_ct FROM history WHERE event_id = ?1",
                [event_id],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(db_err(other)),
            })?;
        ct.map(|ct| self.open_value("history", event_id, "event", &ct))
            .transpose()
    }

    /// Event ids served without content after a failover (`key_lost`).
    pub fn key_lost_ids(&self) -> Result<Vec<String>, HiveError> {
        let mut stmt = self
            .conn
            .prepare("SELECT event_id FROM events WHERE key_lost = 1 ORDER BY seq")
            .map_err(db_err)?;
        stmt.query_map([], |r| r.get(0))
            .map_err(db_err)?
            .collect::<Result<_, _>>()
            .map_err(db_err)
    }

    /// The signed envelope of a stored event (with its signature).
    pub fn stored_envelope(&self, event_id: &str) -> Result<Option<serde_json::Value>, HiveError> {
        let ct: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT envelope_ct FROM events WHERE event_id = ?1",
                [event_id],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(db_err(other)),
            })?;
        ct.map(|ct| {
            let plain = self.open_value("events", event_id, "envelope", &ct)?;
            serde_json::from_slice(&plain).map_err(|e| HiveError::Local(e.to_string()))
        })
        .transpose()
    }

    /// Stored events that failed their checks (not purged, not key_lost),
    /// as envelope + body, to check again once the signer's keys are
    /// current: a worker key fetched before its worker registered lists no
    /// worker, so that worker's events failed on every member.
    pub fn unverified(&self) -> Result<Vec<(String, serde_json::Value)>, HiveError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT event_id, envelope_ct, body_ct FROM events
                 WHERE sig_invalid = 1 AND key_lost = 0 AND purged = 0 ORDER BY seq",
            )
            .map_err(db_err)?;
        let rows: Vec<(String, Vec<u8>, Option<Vec<u8>>)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(db_err)?
            .collect::<Result<_, _>>()
            .map_err(db_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, env_ct, body_ct) in rows {
            let plain = self.open_value("events", &id, "envelope", &env_ct)?;
            let mut event: serde_json::Value =
                serde_json::from_slice(&plain).map_err(|e| HiveError::Local(e.to_string()))?;
            if let Some(ct) = body_ct {
                let body = self.open_value("events", &id, "body", &ct)?;
                event["body"] =
                    serde_json::from_slice(&body).map_err(|e| HiveError::Local(e.to_string()))?;
            }
            out.push((id, event));
        }
        Ok(out)
    }

    /// An unverified event passed its checks now: let it fold.
    pub fn mark_verified(&self, event_id: &str) -> Result<(), HiveError> {
        self.conn
            .execute(
                "UPDATE events SET sig_invalid = 0 WHERE event_id = ?1 AND sig_invalid = 1",
                [event_id],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// A `key_lost` event's content came back and verified: store it and
    /// let it fold.
    pub fn restore_body(&self, event_id: &str, body: &serde_json::Value) -> Result<(), HiveError> {
        let bytes = serde_json::to_vec(body).map_err(|e| HiveError::Local(e.to_string()))?;
        let ct = self.seal("events", event_id, "body", &bytes)?;
        self.conn
            .execute(
                "UPDATE events SET body_ct = ?2, key_lost = 0, sig_invalid = 0
                 WHERE event_id = ?1 AND key_lost = 1",
                params![event_id, ct],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Note when the log delivered `seqs`.
    pub fn note_received(&self, seqs: &[u64], at: u64) -> Result<(), HiveError> {
        for seq in seqs {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO received (seq, at) VALUES (?1, ?2)",
                    params![i64_of(*seq)?, i64_of(at)?],
                )
                .map_err(db_err)?;
        }
        Ok(())
    }

    /// Drop batches and receive times older than [`KEEP_MS`].
    pub fn prune_history(&self, now_ms: u64) -> Result<usize, HiveError> {
        let cut = i64_of(now_ms.saturating_sub(KEEP_MS))?;
        let h = self
            .conn
            .execute("DELETE FROM history WHERE pushed_at < ?1", [cut])
            .map_err(db_err)?;
        let r = self
            .conn
            .execute("DELETE FROM received WHERE at < ?1", [cut])
            .map_err(db_err)?;
        Ok(h + r)
    }

    /// The lowest `seq` received in the last 24 h.
    pub fn rewind_seq(&self, now_ms: u64) -> Result<Option<u64>, HiveError> {
        let cut = i64_of(now_ms.saturating_sub(KEEP_MS))?;
        let min: Option<i64> = self
            .conn
            .query_row("SELECT MIN(seq) FROM received WHERE at >= ?1", [cut], |r| {
                r.get(0)
            })
            .map_err(db_err)?;
        Ok(min.and_then(|s| u64::try_from(s).ok()))
    }

    /// A re-tail from `from` returned `kept` (`seq`, `event_id`): every
    /// local row at or after `from` that did not come back with the same
    /// id is deleted, and the cursor goes back to `from - 1` so the rows
    /// that came back are stored and folded again.
    pub fn drop_changed_from(&self, from: u64, kept: &[(u64, String)]) -> Result<usize, HiveError> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, event_id FROM events WHERE seq >= ?1")
            .map_err(db_err)?;
        let local: Vec<(i64, String)> = stmt
            .query_map([i64_of(from)?], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db_err)?
            .collect::<Result<_, _>>()
            .map_err(db_err)?;
        let mut dropped = 0;
        for (seq, id) in local {
            let same = kept
                .iter()
                .any(|(s, k)| i64::try_from(*s).ok() == Some(seq) && *k == id);
            if !same {
                dropped += self
                    .conn
                    .execute("DELETE FROM events WHERE seq = ?1", [seq])
                    .map_err(db_err)?;
            }
        }
        self.set_meta(META_CURSOR, &from.saturating_sub(1).to_le_bytes())?;
        Ok(dropped)
    }
}
