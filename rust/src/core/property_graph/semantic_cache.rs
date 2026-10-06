// SPDX-License-Identifier: Apache-2.0
//! Persistent cache of semantic-backend answers, keyed by call site.
//!
//! A row is valid only while the caller file's content hash is unchanged and
//! its `context` fingerprint still matches (see [`CachedResolution::context`]).
//! Only definitive answers are stored; transient failures and "no result"
//! (a cold server answers that while indexing) are retried instead.

use rusqlite::{Connection, OptionalExtension, params};

/// One cached semantic answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedResolution {
    /// Identity of the backend that answered (`lsp:rust-analyzer@…`).
    pub backend: String,
    /// `resolved` | `external` | `ambiguous`. For `implementations`,
    /// `target_file` holds the implementor files newline-separated.
    pub outcome: String,
    pub target_file: Option<String>,
    pub target_line: Option<usize>,
    pub target_symbol: Option<String>,
    /// Fingerprint of what the answer depends on *outside* the caller file
    /// (e.g. which files define the callee's name; the project revision for
    /// implementations). A mismatch makes the row stale.
    pub context: String,
}

/// Call-site key: `(caller_file, 1-based line, 0-based byte column, op)`.
pub type SiteKey<'a> = (&'a str, usize, usize, &'a str);

pub(super) fn lookup(
    conn: &Connection,
    site: SiteKey<'_>,
    caller_hash: &str,
) -> anyhow::Result<Option<CachedResolution>> {
    let (file, line, col, op) = site;
    let row = conn
        .query_row(
            "SELECT backend, outcome, target_file, target_line, target_symbol, context
             FROM semantic_resolutions
             WHERE caller_file = ?1 AND line = ?2 AND col = ?3 AND op = ?4
               AND caller_hash = ?5",
            params![file, line as i64, col as i64, op, caller_hash],
            |r| {
                Ok(CachedResolution {
                    backend: r.get(0)?,
                    outcome: r.get(1)?,
                    target_file: r.get(2)?,
                    target_line: r.get::<_, Option<i64>>(3)?.map(|l| l as usize),
                    target_symbol: r.get(4)?,
                    context: r.get(5)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

pub(super) fn store(
    conn: &Connection,
    site: SiteKey<'_>,
    caller_hash: &str,
    value: &CachedResolution,
) -> anyhow::Result<()> {
    let (file, line, col, op) = site;
    conn.execute(
        "INSERT INTO semantic_resolutions
            (caller_file, line, col, op, caller_hash, backend, outcome,
             target_file, target_line, target_symbol, context)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(caller_file, line, col, op) DO UPDATE SET
            caller_hash = excluded.caller_hash,
            backend = excluded.backend,
            outcome = excluded.outcome,
            target_file = excluded.target_file,
            target_line = excluded.target_line,
            target_symbol = excluded.target_symbol,
            context = excluded.context",
        params![
            file,
            line as i64,
            col as i64,
            op,
            caller_hash,
            value.backend,
            value.outcome,
            value.target_file,
            value.target_line.map(|l| l as i64),
            value.target_symbol,
            value.context,
        ],
    )?;
    Ok(())
}
/// Whether any semantic answer is stored — evidence that enrichment has
/// consolidated this graph's call edges at least once.
pub(super) fn any(conn: &Connection) -> anyhow::Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM semantic_resolutions)",
        [],
        |r| r.get(0),
    )?)
}

/// Drops rows whose caller file is no longer indexed, and rows for indexed
/// files whose content changed (stale hash). Returns the number removed.
pub(super) fn prune(
    conn: &Connection,
    live_hashes: &std::collections::HashMap<String, String>,
) -> anyhow::Result<usize> {
    let mut stmt =
        conn.prepare("SELECT DISTINCT caller_file, caller_hash FROM semantic_resolutions")?;
    let stale: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(Result::ok)
        .filter(|(file, hash)| live_hashes.get(file) != Some(hash))
        .collect();
    let mut removed = 0;
    for (file, hash) in stale {
        removed += conn.execute(
            "DELETE FROM semantic_resolutions WHERE caller_file = ?1 AND caller_hash = ?2",
            params![file, hash],
        )?;
    }
    Ok(removed)
}
