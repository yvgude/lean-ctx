// SPDX-License-Identifier: Apache-2.0
//! The place's event log in the replica (SPEC-ENGINE §3.4.1 `events`):
//! verified events from the server log, stored with their plaintext ids
//! and server fields and their envelope, worker id and body encrypted.
//! The graph is rebuilt from this log ([`super::fold`]), so the log is the
//! only state that has to be right.

use rusqlite::params;
use serde_json::Value;

use super::fold::{FoldEvent, Graph};
use super::store::{Replica, db_err};
use crate::core::hive::HiveError;

/// One event as the server log delivered it, after verification.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub seq: u64,
    /// The whole event as received (envelope, server fields, body).
    pub event: Value,
    /// Signature or body check failed: kept, never folded or delivered.
    pub sig_invalid: bool,
}

fn s(event: &Value, key: &str) -> Option<String> {
    event.get(key).and_then(Value::as_str).map(str::to_string)
}

fn i64_of(n: u64, what: &str) -> Result<i64, HiveError> {
    i64::try_from(n).map_err(|_| HiveError::Protocol(format!("{what} out of range")))
}

impl Replica {
    /// Store verified events; an event already stored (same `seq`) is left
    /// as it is. Returns how many were new.
    pub fn insert_events(&mut self, events: &[StoredEvent]) -> Result<usize, HiveError> {
        let mut rows = Vec::with_capacity(events.len());
        for e in events {
            let id = s(&e.event, "event_id")
                .ok_or_else(|| HiveError::Protocol("event without event_id".into()))?;
            let mut envelope =
                crate::core::hive::events::sign::envelope(&e.event).map_err(HiveError::Protocol)?;
            if let Some(sig) = e.event.get("signature") {
                envelope.insert("signature".into(), sig.clone());
            }
            let envelope = serde_json::to_vec(&Value::Object(envelope))
                .map_err(|err| HiveError::Local(err.to_string()))?;
            let body = match e.event.get("body") {
                Some(b) if !b.is_null() => Some(self.seal(
                    "events",
                    &id,
                    "body",
                    &serde_json::to_vec(b).map_err(|err| HiveError::Local(err.to_string()))?,
                )?),
                _ => None,
            };
            let worker = s(&e.event, "worker_id")
                .map(|w| self.seal("events", &id, "worker_id", w.as_bytes()))
                .transpose()?;
            let hlc = e.event.get("hlc");
            let wall = hlc
                .and_then(|h| h.get("wall_ms"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let ctr = hlc
                .and_then(|h| h.get("counter"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            rows.push((
                i64_of(e.seq, "seq")?,
                id.clone(),
                s(&e.event, "op").unwrap_or_default(),
                s(&e.event, "target").unwrap_or_default(),
                i64_of(wall, "hlc wall")?,
                i64_of(ctr, "hlc counter")?,
                (
                    s(&e.event, "provenance"),
                    s(&e.event, "review_state"),
                    s(&e.event, "reviewed_by"),
                    s(&e.event, "batch_id"),
                ),
                (
                    e.event
                        .get("purged")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    // Served without content after a server failover (R48).
                    e.event
                        .get("key_lost")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                ),
                e.sig_invalid,
                body,
                worker,
                self.seal("events", &id, "envelope", &envelope)?,
            ));
        }
        let tx = self.conn.transaction().map_err(db_err)?;
        let mut new = 0;
        for (
            seq,
            id,
            op,
            target,
            wall,
            ctr,
            (prov, review, by, batch),
            (purged, key_lost),
            invalid,
            body,
            worker,
            env,
        ) in rows
        {
            new += tx
                .execute(
                    "INSERT OR IGNORE INTO events (seq, event_id, op, target, hlc_wall, hlc_ctr,
                        provenance, review_state, reviewed_by, batch_id, purged, sig_invalid,
                        body_ct, worker_id_ct, envelope_ct, key_lost)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                        ?16)",
                    params![
                        seq, id, op, target, wall, ctr, prov, review, by, batch, purged, invalid,
                        body, worker, env, key_lost
                    ],
                )
                .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        Ok(new)
    }

    /// Every stored event as the fold sees it, in `seq` order. A `key_lost`
    /// event moves the cursor like a failed signature and never folds until
    /// its content is repaired; a row a sealing conversion replaced
    /// (`converted`, R90) moves it too and never folds again (its sealed copy
    /// carries the effect).
    pub fn fold_events(&self) -> Result<Vec<FoldEvent>, HiveError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT seq, event_id, op, target, purged, sig_invalid OR key_lost OR converted, body_ct,
                        envelope_ct, provenance, review_state
                 FROM events ORDER BY seq",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, bool>(4)?,
                    r.get::<_, bool>(5)?,
                    r.get::<_, Option<Vec<u8>>>(6)?,
                    r.get::<_, Vec<u8>>(7)?,
                    r.get::<_, Option<String>>(8)?,
                    r.get::<_, Option<String>>(9)?,
                ))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, id, op, target, purged, invalid, body_ct, env_ct, provenance, review_state) =
                row.map_err(db_err)?;
            let env = self.open_value("events", &id, "envelope", &env_ct)?;
            let env: Value =
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
            out.push(FoldEvent {
                seq: u64::try_from(seq).map_err(|_| HiveError::Local("negative seq".into()))?,
                event_id: id,
                op,
                target: (!target.is_empty()).then_some(target),
                node_type: s(&env, "node_type"),
                body,
                sig_invalid: invalid,
                provenance,
                review_state,
            });
        }
        Ok(out)
    }

    /// Shred the stored bodies of `ids` now, as [`Self::refold`] does for
    /// the events a folded purge names: for a tombstone applied before it
    /// is stored (a used-up transfer, `driver::deletions`).
    pub fn shred_events(
        &mut self,
        ids: &std::collections::BTreeSet<String>,
    ) -> Result<(), HiveError> {
        let tx = self.conn.transaction().map_err(db_err)?;
        for id in ids {
            tx.execute(
                "UPDATE events SET body_ct = NULL, ck_ct = NULL, purged = 1 WHERE event_id = ?1",
                [id],
            )
            .map_err(db_err)?;
            tx.execute("DELETE FROM history WHERE event_id = ?1", [id])
                .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)
    }

    /// Fold the whole stored log into the graph, store the materialised
    /// nodes and edges (fields encrypted) and move the cursor.
    pub fn refold(&mut self) -> Result<Graph, HiveError> {
        let events = self.fold_events()?;
        let mut graph = Graph::default();
        graph
            .fold(&events)
            .map_err(|e| HiveError::Protocol(e.to_string()))?;
        let mut nodes = Vec::with_capacity(graph.nodes.len());
        for (id, n) in &graph.nodes {
            let fields =
                serde_json::to_vec(&n.fields).map_err(|e| HiveError::Local(e.to_string()))?;
            nodes.push((
                id.clone(),
                n.node_type.clone(),
                i64_of(n.last_seq, "seq")?,
                n.retracted,
                self.seal("nodes", id, "fields", &fields)?,
            ));
        }
        // Exact dedupe keys of every live claim node, also those other
        // engines wrote: capture then never sends a claim the place holds.
        let mut dedupe = Vec::new();
        if let Some(pk) = self.id_salt()? {
            for (id, n) in graph.live_nodes() {
                let kind = crate::core::knowledge::NodeType::parse(&n.node_type)
                    .filter(|t| *t != crate::core::knowledge::NodeType::Procedure);
                let statement = n.fields.get("statement").and_then(Value::as_str);
                if let (Some(kind), Some(statement)) = (kind, statement) {
                    let dk = crate::core::hive::capture::dedupe::exact_key(&pk, kind, statement);
                    dedupe.push((self.keyed(&dk), id.clone()));
                }
            }
        }
        let tx = self.conn.transaction().map_err(db_err)?;
        for (key, id) in &dedupe {
            tx.execute(
                "INSERT OR IGNORE INTO dedupe_keys (key, node_id) VALUES (?1, ?2)",
                params![key.as_slice(), id],
            )
            .map_err(db_err)?;
        }
        tx.execute("DELETE FROM nodes", []).map_err(db_err)?;
        tx.execute("DELETE FROM edges", []).map_err(db_err)?;
        for (id, node_type, last_seq, retracted, fields) in nodes {
            tx.execute(
                "INSERT INTO nodes (node_id, node_type, last_seq, retracted, fields_ct)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, node_type, last_seq, retracted, fields],
            )
            .map_err(db_err)?;
        }
        for (id, e) in &graph.edges {
            tx.execute(
                "INSERT INTO edges (edge_id, kind, from_id, to_id, last_seq, retracted)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    e.kind,
                    e.from,
                    e.to,
                    i64_of(e.last_seq, "seq")?,
                    e.retracted
                ],
            )
            .map_err(db_err)?;
        }
        // Shredded bodies leave the log too (purge is applied to storage,
        // not only to the view).
        for id in &graph.purged_events {
            tx.execute(
                "UPDATE events SET body_ct = NULL, ck_ct = NULL, purged = 1 WHERE event_id = ?1",
                [id],
            )
            .map_err(db_err)?;
            // A purged own event is never re-pushed (§3.4.6).
            tx.execute("DELETE FROM history WHERE event_id = ?1", [id])
                .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        if graph.cursor > 0 {
            self.set_cursor(graph.cursor)?;
        }
        Ok(graph)
    }

    /// Fields of a materialised node, decrypted.
    pub fn node_fields(&self, node_id: &str) -> Result<Option<Value>, HiveError> {
        let ct: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT fields_ct FROM nodes WHERE node_id = ?1",
                [node_id],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(db_err(other)),
            })?;
        ct.map(|ct| {
            let plain = self.open_value("nodes", node_id, "fields", &ct)?;
            serde_json::from_slice(&plain).map_err(|e| HiveError::Local(e.to_string()))
        })
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(seq: u64, op: &str, target: &str, body: &Value) -> StoredEvent {
        StoredEvent {
            seq,
            event: serde_json::json!({
                "event_id": format!("E{seq:025}"),
                "op": op,
                "target": target,
                "node_type": (op == "node.add").then_some("fact"),
                "hlc": {"wall_ms": 1000 + seq, "counter": 0, "node": "x"},
                "worker_id": "wkr_aaaaaaaaaaaaaaaaaaaa",
                "signature": "00",
                "seq": seq,
                "body": body,
            }),
            sig_invalid: false,
        }
    }

    #[test]
    fn the_log_refolds_and_purges_storage() {
        let data = tempfile::tempdir().unwrap();
        let (mut r, _) = Replica::open(data.path(), "pl_log", &[4u8; 32]).unwrap();
        let canary = "Canary statement that must be shredded.";
        let events = vec![
            event(
                1,
                "node.add",
                "n_a",
                &serde_json::json!({"statement": canary, "body_nonce": "00"}),
            ),
            event(
                2,
                "node.add",
                "n_b",
                &serde_json::json!({"statement": "A claim that stays here."}),
            ),
            event(
                3,
                "field.set",
                "n_b",
                &serde_json::json!({"field": "evidence", "value": "observed"}),
            ),
        ];
        assert_eq!(r.insert_events(&events).unwrap(), 3);
        assert_eq!(r.insert_events(&events).unwrap(), 0, "idempotent");
        let g = r.refold().unwrap();
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(r.cursor().unwrap(), Some(3));
        assert_eq!(
            r.node_fields("n_b").unwrap().unwrap()["evidence"],
            "observed"
        );

        let mut purge = event(
            4,
            "purge",
            "",
            &serde_json::json!({"purge_id": "prg_1", "reason": "leaked_secret",
                               "targets": ["E0000000000000000000000001"], "cascade": [], "bus_cascade": []}),
        );
        purge.event["actor"] = serde_json::json!({"kind": "server"});
        r.insert_events(&[purge]).unwrap();
        let g = r.refold().unwrap();
        assert!(g.nodes["n_a"].purged);
        assert_eq!(
            r.node_fields("n_a").unwrap().unwrap(),
            serde_json::json!({})
        );
        let shredded: Option<Vec<u8>> = r
            .conn
            .query_row(
                "SELECT body_ct FROM events WHERE event_id = 'E0000000000000000000000001'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(shredded.is_none(), "the purged body is gone from the log");

        // A later refold of the stored log gives the same graph.
        assert_eq!(r.refold().unwrap(), g);
        let dir = r.dir().to_path_buf();
        drop(r);
        for f in std::fs::read_dir(dir).unwrap() {
            let bytes = std::fs::read(f.unwrap().path()).unwrap_or_default();
            assert!(!bytes.windows(canary.len()).any(|w| w == canary.as_bytes()));
        }
    }
}
