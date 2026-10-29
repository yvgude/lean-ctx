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

/// `lookup` for a caller that USES the answer (an edge it writes, a target
/// it names): the row is returned only while every file it derives from is
/// still served — the caller file and each target file (`target_file` holds
/// several, newline-separated, for `implementations`). `serves` is the
/// serve-time check of the opened graph (`CodeGraph::serves_file`: the file
/// is present inside the project, and under protection the admitted rooted
/// reader returns it now); it is asked per call and nothing is kept here.
///
/// A row is derived from ALL its files: one that is withdrawn withholds the
/// answer, which is then asked from the backend again like any miss. A row
/// without a target (`external`, `ambiguous`) depends on the caller alone.
pub fn lookup_served(
    conn: &Connection,
    site: SiteKey<'_>,
    caller_hash: &str,
    mut serves: impl FnMut(&str) -> bool,
) -> anyhow::Result<Option<CachedResolution>> {
    Ok(lookup(conn, site, caller_hash)?.filter(|row| {
        serves(site.0)
            && row
                .target_file
                .as_deref()
                .is_none_or(|targets| targets.lines().all(&mut serves))
    }))
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

/// `store` through storage admission (E3), for the writer of an enrichment
/// run: `admission` is the ONE policy snapshot of that run. The row is
/// persisted only if every text it keeps — caller file, backend, outcome,
/// target file(s), target symbol — passes that snapshot verbatim. `Ok(false)`:
/// withheld, restricted or rewritten by admission; nothing was written and
/// the answer is asked from the backend again like any miss.
pub fn store_admitted(
    conn: &Connection,
    site: SiteKey<'_>,
    caller_hash: &str,
    value: &CachedResolution,
    admission: &crate::core::context_admission::stores::StoreAdmission,
) -> anyhow::Result<bool> {
    let kept = [
        Some(site.0),
        Some(value.backend.as_str()),
        Some(value.outcome.as_str()),
        value.target_file.as_deref(),
        value.target_symbol.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n");
    if admission.admit_text(&kept).as_deref() != Some(kept.as_str()) {
        return Ok(false);
    }
    store(conn, site, caller_hash, value)?;
    Ok(true)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property_graph::CodeGraph;

    /// G04: a cached answer is used while the caller file and its target are
    /// served by the opened graph, and no longer once the target is
    /// withdrawn. The row stays in the store; it is not handed to a user.
    #[test]
    fn a_cached_answer_is_used_only_while_its_files_are_served() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project = tempfile::tempdir().expect("project");
        let root = project.path().canonicalize().expect("canonical root");
        let root_text = root.to_str().expect("utf-8 root");
        for name in ["app.rs", "billing.rs"] {
            std::fs::write(root.join(name), "pub fn settle() {}\n").expect("source");
        }
        let site: SiteKey<'_> = ("app.rs", 2, 4, "definition");
        let answer = CachedResolution {
            backend: "lsp:test".to_owned(),
            outcome: "resolved".to_owned(),
            target_file: Some("billing.rs".to_owned()),
            target_line: Some(1),
            target_symbol: Some("settle".to_owned()),
            context: "c".to_owned(),
        };
        // One opened graph is one request: its gate decides each file once.
        let used = || {
            let graph = CodeGraph::open(root_text).expect("graph");
            lookup_served(&graph.conn, site, "h", |file| graph.serves_file(file)).expect("query")
        };
        let graph = CodeGraph::open(root_text).expect("graph");
        store(&graph.conn, site, "h", &answer).expect("stored");
        assert_eq!(used(), Some(answer.clone()));

        std::fs::remove_file(root.join("billing.rs")).expect("withdraw");
        assert_eq!(used(), None);
        assert_eq!(lookup(&graph.conn, site, "h").expect("row"), Some(answer));
    }
}
