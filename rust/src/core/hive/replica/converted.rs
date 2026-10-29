// SPDX-License-Identifier: Apache-2.0
//! Plaintext rows of a vault sealed after creation (SPEC-ENGINE §3.16,
//! source `v2_plain`, RESOLUTIONS R90): what the conversion lease holder
//! reads, and the `converted` mark every engine sets once the sealed copy
//! carries a row's effect. A converted row keeps its id and envelope, loses
//! its plaintext body and folds as a no-op; it is never shown as purged.

use rusqlite::params;
use serde_json::Value;

use super::store::{Replica, db_err};
use crate::core::hive::HiveError;

/// One stored event as the conversion reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Original {
    pub seq: u64,
    pub event_id: String,
    pub op: String,
    pub purged: bool,
    pub sig_invalid: bool,
    /// The signed envelope with its signature.
    pub envelope: Value,
    pub body: Option<Value>,
}

fn u64_of(n: i64) -> Result<u64, HiveError> {
    u64::try_from(n).map_err(|_| HiveError::Local("negative seq".into()))
}

impl Replica {
    /// Every not yet converted row with `seq ≤ max_seq`, in `seq` order.
    pub fn originals_upto(&self, max_seq: u64) -> Result<Vec<Original>, HiveError> {
        let max = i64::try_from(max_seq).map_err(|_| HiveError::Local("seq range".into()))?;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT seq, event_id, op, purged, sig_invalid, envelope_ct, body_ct
                 FROM events WHERE seq <= ?1 AND converted = 0 ORDER BY seq",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([max], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, bool>(4)?,
                    r.get::<_, Vec<u8>>(5)?,
                    r.get::<_, Option<Vec<u8>>>(6)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, id, op, purged, sig_invalid, env_ct, body_ct) = row.map_err(db_err)?;
            let env = self.open_value("events", &id, "envelope", &env_ct)?;
            let envelope =
                serde_json::from_slice(&env).map_err(|e| HiveError::Local(e.to_string()))?;
            let body = match (purged, body_ct) {
                (false, Some(ct)) => {
                    let plain = self.open_value("events", &id, "body", &ct)?;
                    Some(
                        serde_json::from_slice(&plain)
                            .map_err(|e| HiveError::Local(e.to_string()))?,
                    )
                }
                _ => None,
            };
            out.push(Original {
                seq: u64_of(seq)?,
                event_id: id,
                op,
                purged,
                sig_invalid,
                envelope,
                body,
            });
        }
        Ok(out)
    }

    /// Mark rows converted: their plaintext body goes, they fold as a
    /// no-op. Purged rows stay purged. Returns how many changed.
    pub fn mark_converted(&self, seqs: &[u64]) -> Result<usize, HiveError> {
        let mut changed = 0;
        for seq in seqs {
            let seq = i64::try_from(*seq).map_err(|_| HiveError::Local("seq range".into()))?;
            changed += self
                .conn
                .execute(
                    "UPDATE events SET converted = 1, body_ct = NULL, ck_ct = NULL
                     WHERE seq = ?1 AND converted = 0 AND purged = 0",
                    params![seq],
                )
                .map_err(db_err)?;
        }
        Ok(changed)
    }

    /// `(seq, event_id)` of every row not converted.
    pub fn unconverted_ids(&self) -> Result<Vec<(u64, String)>, HiveError> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, event_id FROM events WHERE converted = 0 AND purged = 0")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(db_err)?;
        rows.map(|r| {
            let (seq, id) = r.map_err(db_err)?;
            Ok((u64_of(seq)?, id))
        })
        .collect()
    }

    /// Whether the log holds an event with this id.
    pub fn has_event(&self, event_id: &str) -> Result<bool, HiveError> {
        self.conn
            .query_row(
                "SELECT 1 FROM events WHERE event_id = ?1",
                [event_id],
                |_| Ok(()),
            )
            .map(|()| true)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                other => Err(db_err(other)),
            })
    }
}
