// SPDX-License-Identifier: Apache-2.0
//! Retired routing history in the canonical personal learning database.
//!
//! Adaptive model routing read accepted/rejected outcomes per model from this
//! table. v4 removed automatic model selection, so nothing writes here any
//! more; the table stays so history recorded before stays exportable
//! (`autopilot history|export`) and deletable (`autopilot reset`).

use anyhow::{Result, ensure};
use lean_ctx_protocol::AcceptanceState;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use super::AdaptiveLearningStore;

pub const MAX_ROUTING_HISTORY: usize = 10_000;

/// Local inspection/export only. Names and execution content are not retained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingHistoryEntry {
    pub schema_version: u32,
    pub receipt_id: String,
    pub model_key: String,
    pub outcome: AcceptanceState,
}

pub(super) fn initialize(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS routing_learning_history_v1 (
            scope TEXT NOT NULL,
            receipt_id TEXT NOT NULL CHECK(length(CAST(receipt_id AS BLOB)) <= 256),
            model_key TEXT NOT NULL CHECK(length(model_key) = 64),
            accepted INTEGER NOT NULL CHECK(accepted IN (0, 1)),
            UNIQUE(scope, receipt_id)
        );
        CREATE INDEX IF NOT EXISTS routing_learning_model_v1
            ON routing_learning_history_v1(scope, model_key);",
    )?;
    Ok(())
}

impl AdaptiveLearningStore {
    /// Complete bounded local export; ordering is newest committed observation first.
    pub fn routing_history(&self, limit: usize) -> Result<Vec<RoutingHistoryEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT CASE WHEN length(CAST(receipt_id AS BLOB)) <= 256 THEN receipt_id ELSE NULL END,
             CASE WHEN length(CAST(model_key AS BLOB)) = 64 THEN model_key ELSE NULL END,
             accepted FROM routing_learning_history_v1
             WHERE scope = ?1 ORDER BY rowid DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![self.scope, i64::try_from(limit.min(MAX_ROUTING_HISTORY))?],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (receipt_id, model_key, accepted) = row?;
            lean_ctx_protocol::ReceiptId::new(receipt_id.clone())?;
            ensure!(
                model_key.len() == 64
                    && model_key
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "invalid routing identity"
            );
            Ok(RoutingHistoryEntry {
                schema_version: 1,
                receipt_id,
                model_key,
                outcome: outcome(accepted)?,
            })
        })
        .collect()
    }
}

fn outcome(value: i64) -> Result<AcceptanceState> {
    match value {
        1 => Ok(AcceptanceState::Accepted),
        0 => Ok(AcceptanceState::Rejected),
        _ => anyhow::bail!("invalid routing acceptance state"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_kernel::autopilot::tests::protocol_task;

    #[test]
    fn retired_history_stays_exportable_resettable_and_rejects_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("learning.db");
        let task = protocol_task();
        let open = |project: &str| {
            AdaptiveLearningStore::new(
                Connection::open(&path).unwrap(),
                lean_ctx_protocol::ProjectId::new(project.to_owned()).unwrap(),
                task.tenant_id.clone(),
            )
            .unwrap()
        };
        let project = task.project_id.as_str().to_owned();
        let store = open(&project);
        let key = "a".repeat(64);
        for (index, accepted) in [(0, 1), (1, 0)] {
            store
                .connection
                .execute(
                    "INSERT INTO routing_learning_history_v1 VALUES (?1, ?2, ?3, ?4)",
                    params![store.scope, format!("receipt-{index}"), key, accepted],
                )
                .unwrap();
        }
        let exported = store.routing_history(MAX_ROUTING_HISTORY).unwrap();
        assert_eq!(exported.len(), 2);
        assert_eq!(
            exported[0].outcome,
            AcceptanceState::Rejected,
            "newest first"
        );
        drop(store);

        // Another project never sees or resets this scope.
        let mut other = open("other-project");
        assert!(other.routing_history(10).unwrap().is_empty());
        other.reset().unwrap();
        let mut store = open(&project);
        assert_eq!(
            store.routing_history(MAX_ROUTING_HISTORY).unwrap(),
            exported
        );

        store
            .connection
            .pragma_update(None, "ignore_check_constraints", true)
            .unwrap();
        store
            .connection
            .execute("UPDATE routing_learning_history_v1 SET accepted=7", [])
            .unwrap();
        assert!(store.routing_history(1).is_err());
        store.reset().unwrap();
        assert!(
            store
                .routing_history(MAX_ROUTING_HISTORY)
                .unwrap()
                .is_empty()
        );
    }
}
