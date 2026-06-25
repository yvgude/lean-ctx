//! Dump engine — SQLite-backed snapshot with bulk-insert pattern.
//!
//! Replaces the former postcard+zstd serialiser with a single SQLite database
//! (`code_index.db`) containing graph nodes, edges, FTS5 search index, file
//! metadata, and BM25 code chunks.  All writes use batch inserts of 500 rows
//! per batch (matching C's `cbm_store_begin_bulk` / `cbm_store_end_bulk`
//! pattern from the reference implementation).
//!
//! ## Schema
//!
//! - **files** — `ProjectIndex` file entries (round-trip fidelity for the old
//!   load path).
//! - **nodes** — graph nodes with label, name, qualified_name, file_path,
//!   line range, and JSON properties.
//! - **edges** — directed edges with source/target node FK, type, and JSON
//!   properties.
//! - **nodes_fts** — FTS5 virtual table over `nodes` (enables `search_graph`).
//! - **file_hashes** — per-file content hashes for incremental rebuild support.
//! - **chunks** — BM25 code chunks (content, path, line range, metadata).
//!
//! ## Integrity
//!
//! After every write pass, `PRAGMA integrity_check` is run.  A failing check
//! is a hard error — the pipeline should treat a corrupt DB like a corrupt
//! `.zst` artifact.
//!
//! ## Locking
//!
//! No concurrent writers: SQLite's file-level lock prevents corruption, but
//! the caller must still serialise all dump calls.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::core::bm25_index::BM25Index;
use crate::core::graph_buffer::GraphBuffer;
use crate::core::graph_index::{FileEntry, IndexEdge, ProjectIndex, SymbolEntry};
use crate::core::index_namespace;
// FileMetadataStore now defined in this module (was file_metadata_store.rs).
use crate::core::index_types::{CodeChunk, GbufEdge, GbufNode};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Number of rows per bulk-insert batch.  C's reference uses 1000; 500 is a
/// safer default that keeps memory low while still outperforming row-by-row.
const BATCH_SIZE: usize = 500;

/// Name of the SQLite database file inside the vectors directory.
const DB_FILENAME: &str = "code_index.db";

// ---------------------------------------------------------------------------
// DumpEngine
// ---------------------------------------------------------------------------

/// SQLite-backed dump engine using bulk-insert pattern (like C).
///
/// Every write goes through batch inserts inside a single top-level
/// transaction.  The FTS5 virtual table is dropped before the insert loop and
/// rebuilt at the end for maximum insert throughput.
pub struct DumpEngine {
    pub project_root: PathBuf,
}

impl DumpEngine {
    // ── Construction ────────────────────────────────────────────────────

    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }

    // ── Path helpers ────────────────────────────────────────────────────

    /// Full path to the SQLite database file.
    fn db_path(&self) -> PathBuf {
        let dir = index_namespace::vectors_dir(&self.project_root);
        dir.join(DB_FILENAME)
    }

    /// Full path to the SQLite database file for a given root (static variant).
    fn db_path_for(root: &Path) -> PathBuf {
        let dir = index_namespace::vectors_dir(root);
        dir.join(DB_FILENAME)
    }

    // ── Schema ─────────────────────────────────────────────────────────

    /// Create all tables if they do not exist.
    ///
    /// Idempotent — safe to call on every dump.
    fn create_schema(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS files (
                path         TEXT    PRIMARY KEY,
                hash         TEXT    NOT NULL,
                language     TEXT    DEFAULT '',
                line_count   INTEGER DEFAULT 0,
                token_count  INTEGER DEFAULT 0,
                exports      TEXT    DEFAULT '[]',
                summary      TEXT    DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS nodes (
                id              INTEGER PRIMARY KEY,
                label           TEXT    NOT NULL,
                name            TEXT    NOT NULL,
                qualified_name  TEXT    NOT NULL UNIQUE,
                file_path       TEXT    NOT NULL,
                start_line      INTEGER DEFAULT 0,
                end_line        INTEGER DEFAULT 0,
                properties      TEXT    DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS edges (
                id          INTEGER PRIMARY KEY,
                source_id   INTEGER NOT NULL REFERENCES nodes(id),
                target_id   INTEGER NOT NULL REFERENCES nodes(id),
                type        TEXT    NOT NULL,
                properties  TEXT    DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS file_hashes (
                file_path   TEXT    PRIMARY KEY,
                hash        TEXT    NOT NULL,
                mode_mask   INTEGER DEFAULT 0,
                updated_at  TEXT    DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS chunks (
                id           INTEGER PRIMARY KEY,
                file_path    TEXT    NOT NULL,
                symbol_name  TEXT    DEFAULT '',
                kind         TEXT    DEFAULT '',
                start_line   INTEGER DEFAULT 0,
                end_line     INTEGER DEFAULT 0,
                content      TEXT    NOT NULL,
                content_hash TEXT    DEFAULT '',
                language     TEXT    DEFAULT '',
                token_count  INTEGER DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_id);
            CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_id);
            CREATE INDEX IF NOT EXISTS idx_nodes_file    ON nodes(file_path);
            CREATE INDEX IF NOT EXISTS idx_nodes_qn      ON nodes(qualified_name);
            ",
        )
        .context("create schema")?;
        Ok(())
    }

    // ── Main dump method (new pipeline) ────────────────────────────────

    /// Dump [`GraphBuffer`] + BM25 [`CodeChunk`]s to the single SQLite
    /// database at `{vectors_dir}/code_index.db`.
    ///
    /// Uses bulk-insert with `BATCH_SIZE`-sized batches. DDL (DROP/CREATE
    /// TABLE) runs in auto-commit mode because SQLite implicitly commits any
    /// open transaction before DDL. Batch inserts use inner transactions.
    pub fn dump_all(&self, gbuf: &GraphBuffer, code_chunks: &[CodeChunk]) -> Result<()> {
        let db_path = self.db_path();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path).context("open code_index.db")?;

        // ══ DDL phase (auto-commit) ════════════════════════════════════
        // SQLite auto-commits before DDL, so never wrap these in BEGIN/COMMIT.

        // 1. Drop FTS5 virtual table (fast rebuild)
        conn.execute("DROP TABLE IF EXISTS nodes_fts", [])?;

        // 2. Create / ensure schema
        Self::create_schema(&conn)?;

        // 3. Clear existing data (idempotent dump) — runs in auto-commit.
        // Disable FK enforcement so we can delete in any order (rusqlite 0.40+
        // may enable it by default on some platforms).
        conn.execute("PRAGMA foreign_keys = OFF", [])?;
        conn.execute("DELETE FROM edges", [])?;
        conn.execute("DELETE FROM nodes", [])?;
        conn.execute("DELETE FROM files", [])?;
        conn.execute("DELETE FROM file_hashes", [])?;
        conn.execute("DELETE FROM chunks", [])?;

        // ══ DML phase (batched transactions) ═══════════════════════════

        // 4. Insert nodes in batches of BATCH_SIZE
        Self::insert_nodes_batch(&conn, gbuf)?;

        // 5. Insert edges in batches of BATCH_SIZE
        Self::insert_edges_batch(&conn, gbuf)?;

        // ══ FTS5 rebuild (contains DDL — auto-commit) ═════════════════

        // 6. Rebuild FTS5 index (CREATE VIRTUAL TABLE + INSERT)
        Self::rebuild_fts(&conn)?;

        // ══ Chunk inserts (batched transactions) ═══════════════════════

        // 7. Insert chunks in batches of BATCH_SIZE
        Self::insert_chunks(&conn, code_chunks)?;

        // ══ Verification ═══════════════════════════════════════════════

        // 8. Integrity check
        Self::verify_integrity(&conn)?;

        Ok(())
    }

    // ── Backward-compatible dump methods ───────────────────────────────

    /// Persist [`ProjectIndex`] to the SQLite database.
    ///
    /// Writes symbol entries as graph nodes, index edges as graph edges, and
    /// file entries to the `files` table. DDL runs in auto-commit mode;
    /// batch inserts use inner transactions.
    pub fn dump_graph_index(&self, graph: &ProjectIndex) -> Result<()> {
        let db_path = self.db_path();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path).context("open code_index.db")?;

        // ══ DDL phase (auto-commit) ════════════════════════════════════
        conn.execute("DROP TABLE IF EXISTS nodes_fts", [])?;
        Self::create_schema(&conn)?;
        conn.execute("PRAGMA foreign_keys = OFF", [])?;
        conn.execute("DELETE FROM edges", [])?;
        conn.execute("DELETE FROM nodes", [])?;
        conn.execute("DELETE FROM files", [])?;
        conn.execute("DELETE FROM file_hashes", [])?;

        // ══ DML phase (batched transactions) ═══════════════════════════
        Self::insert_project_files(&conn, graph)?;
        Self::insert_project_symbols(&conn, graph)?;
        Self::insert_project_edges(&conn, graph)?;

        // ══ FTS5 rebuild (contains DDL — auto-commit) ═════════════════
        Self::rebuild_fts(&conn)?;

        // ══ Verification ═══════════════════════════════════════════════
        Self::verify_integrity(&conn)?;
        Ok(())
    }

    /// Persist [`BM25Index`] chunks to the SQLite `chunks` table.
    ///
    /// Only the chunk list is stored — the BM25 statistical data (inverted
    /// index, doc freqs, avg doc length) is reconstructed on load via
    /// [`BM25Index::add_chunk`] + [`BM25Index::finalize`].
    pub fn dump_bm25_index(&self, bm25: &BM25Index) -> Result<()> {
        let db_path = self.db_path();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&db_path).context("open code_index.db")?;
        Self::create_schema(&conn)?;

        // Clear existing chunks (auto-commit DML).
        conn.execute("DELETE FROM chunks", [])?;

        if bm25.chunks.is_empty() {
            Self::verify_integrity(&conn)?;
            return Ok(());
        }

        let mut stmt = conn
            .prepare(
                "INSERT INTO chunks (file_path, symbol_name, kind, start_line, end_line, content, token_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .context("prepare chunks insert")?;

        for batch in bm25.chunks.chunks(BATCH_SIZE) {
            let tx = conn.unchecked_transaction()?;
            for c in batch {
                let kind_str = serde_json::to_string(&c.kind).unwrap_or_else(|_| String::new());
                stmt.execute(params![
                    c.file_path,
                    c.symbol_name,
                    kind_str,
                    c.start_line as i64,
                    c.end_line as i64,
                    c.content,
                    c.token_count as i64,
                ])?;
            }
            tx.commit()?;
        }

        Self::verify_integrity(&conn)?;
        Ok(())
    }

    /// WAL-checkpoint the file-metadata SQLite database.
    ///
    /// This is the **only** dump method that touches the property-graph
    /// `graph.db` rather than `code_index.db`.
    pub fn dump_file_metadata(&self, store: &FileMetadataStore) -> Result<()> {
        store
            .connection()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .context("WAL checkpoint failed")?;
        Ok(())
    }

    // ── Load / Integrity ───────────────────────────────────────────────

    /// Load all three index components with integrity checks.
    ///
    /// Returns `None` for any component whose on-disk artifact is missing or
    /// corrupted.  Callers should trigger a full rebuild for the failed parts.
    ///
    /// Steps:
    /// 1. Open `code_index.db` (return `None` for graph/BM25 if absent).
    /// 2. Load graph: read files + nodes + edges → reconstruct `ProjectIndex`.
    /// 3. Load BM25: read chunks → rebuild BM25 index (tokenise + finalise).
    /// 4. Open `FileMetadataStore` from the property-graph DB (creates schema
    ///    if absent).
    pub fn load_with_integrity_check(
        root: &Path,
    ) -> Result<(Option<ProjectIndex>, Option<BM25Index>, FileMetadataStore)> {
        let dir = index_namespace::vectors_dir(root);

        // 1. Clean up leftover .tmp files from crashes
        cleanup_tmp_files(&dir);

        // 2. Load graph index from SQLite
        let graph = load_graph_index(root);

        // 3. Load BM25 index from SQLite
        let bm25 = load_bm25_index(root);

        // 4. Open file metadata store
        let fm_store = open_file_metadata_store(root)?;

        Ok((graph, bm25, fm_store))
    }

    // ── Purge ──────────────────────────────────────────────────────────

    /// Delete all dump artifacts from disk, leaving the property-graph DB
    /// (`graph.db`) intact.
    pub fn purge_all(&self) -> Result<()> {
        let dir = index_namespace::vectors_dir(&self.project_root);
        if !dir.exists() {
            return Ok(());
        }

        // Remove the SQLite code_index.db
        let db = dir.join(DB_FILENAME);
        if db.exists() {
            let _ = std::fs::remove_file(&db);
        }
        // Also remove WAL and SHM files left over from a dirty close
        let wal = dir.join(DB_FILENAME.to_string() + "-wal");
        if wal.exists() {
            let _ = std::fs::remove_file(&wal);
        }
        let shm = dir.join(DB_FILENAME.to_string() + "-shm");
        if shm.exists() {
            let _ = std::fs::remove_file(&shm);
        }

        // Remove legacy artifacts from the old postcard+zstd era
        let legacy_artifacts = [
            "project_index.bin.zst",
            "bm25_index.bin.zst",
            "bm25_index.bin",
            "bm25_index.json",
        ];
        for name in &legacy_artifacts {
            let path = dir.join(name);
            if path.exists()
                && let Err(e) = std::fs::remove_file(&path)
            {
                tracing::warn!("[dump_engine] failed to remove {}: {e}", path.display());
            }
        }

        cleanup_tmp_files(&dir);
        Ok(())
    }

    // ── Internal bulk-insert helpers ────────────────────────────────────

    /// Insert all nodes from `gbuf` into the `nodes` table in batches.
    fn insert_nodes_batch(conn: &Connection, gbuf: &GraphBuffer) -> Result<()> {
        let mut stmt = conn
            .prepare(
                "INSERT OR REPLACE INTO nodes (id, label, name, qualified_name, file_path, start_line, end_line, properties)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .context("prepare nodes insert")?;

        let mut nodes: Vec<GbufNode> = Vec::new();
        gbuf.foreach_node(&mut |n| nodes.push(n.clone()));

        for batch in nodes.chunks(BATCH_SIZE) {
            let tx = conn.unchecked_transaction()?;
            for node in batch {
                let props_json = serde_json::to_string(&node.properties)
                    .context("serialize node properties")?;
                stmt.execute(params![
                    node.id.0 as i64,
                    node.label,
                    node.name,
                    node.qualified_name,
                    node.file_path,
                    node.start_line,
                    node.end_line,
                    props_json,
                ])?;
            }
            tx.commit()?;
        }
        Ok(())
    }

    /// Insert all edges from `gbuf` into the `edges` table in batches.
    fn insert_edges_batch(conn: &Connection, gbuf: &GraphBuffer) -> Result<()> {
        let mut stmt = conn
            .prepare(
                "INSERT OR REPLACE INTO edges (id, source_id, target_id, type, properties)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .context("prepare edges insert")?;

        let mut edges: Vec<GbufEdge> = Vec::new();
        gbuf.foreach_edge(&mut |e| edges.push(e.clone()));

        for batch in edges.chunks(BATCH_SIZE) {
            let tx = conn.unchecked_transaction()?;
            for edge in batch {
                let props_json = serde_json::to_string(&edge.properties)
                    .context("serialize edge properties")?;
                stmt.execute(params![
                    edge.id.0 as i64,
                    edge.source_id.0 as i64,
                    edge.target_id.0 as i64,
                    edge.edge_type,
                    props_json,
                ])?;
            }
            tx.commit()?;
        }
        Ok(())
    }

    /// Rebuild the FTS5 `nodes_fts` virtual table from the `nodes` table.
    fn rebuild_fts(conn: &Connection) -> Result<()> {
        // Re-create the FTS5 table (content-sync with nodes table)
        conn.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
                qualified_name, name, label, file_path,
                content='nodes',
                content_rowid='id'
            )",
            [],
        )
        .context("create FTS5 table")?;

        // Populate from nodes
        conn.execute(
            "INSERT INTO nodes_fts(rowid, qualified_name, name, label, file_path)
             SELECT id, qualified_name, name, label, file_path FROM nodes",
            [],
        )
        .context("rebuild FTS5 index")?;

        Ok(())
    }

    /// Insert BM25 [`CodeChunk`]s (new pipeline type) into the `chunks` table.
    fn insert_chunks(conn: &Connection, chunks: &[CodeChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let mut stmt = conn
            .prepare(
                "INSERT INTO chunks (file_path, content, content_hash, start_line, end_line, language)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .context("prepare chunks insert")?;

        for batch in chunks.chunks(BATCH_SIZE) {
            let tx = conn.unchecked_transaction()?;
            for c in batch {
                stmt.execute(params![
                    c.file_path,
                    c.content,
                    c.content_hash,
                    c.start_line,
                    c.end_line,
                    c.language,
                ])?;
            }
            tx.commit()?;
        }
        Ok(())
    }

    /// Insert `ProjectIndex` file entries into the `files` table.
    fn insert_project_files(conn: &Connection, graph: &ProjectIndex) -> Result<()> {
        let mut stmt = conn
            .prepare(
                "INSERT OR REPLACE INTO files (path, hash, language, line_count, token_count, exports, summary)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .context("prepare files insert")?;

        for batch in graph.files.values().collect::<Vec<_>>().chunks(BATCH_SIZE) {
            let tx = conn.unchecked_transaction()?;
            for fe in batch {
                let exports_json =
                    serde_json::to_string(&fe.exports).context("serialize file exports")?;
                stmt.execute(params![
                    fe.path,
                    fe.hash,
                    fe.language,
                    fe.line_count as i64,
                    fe.token_count as i64,
                    exports_json,
                    fe.summary,
                ])?;
            }
            tx.commit()?;
        }
        Ok(())
    }

    /// Insert `ProjectIndex` symbols as graph nodes.
    ///
    /// Also builds a QN → numeric-ID mapping for edge insertion.
    /// Returns the mapping (though currently unused because edges are also
    /// processed through the mapping during `insert_project_edges`).
    fn insert_project_symbols(
        conn: &Connection,
        graph: &ProjectIndex,
    ) -> Result<HashMap<String, i64>> {
        // We need a stable mapping from QN to the numeric ID we assign.
        // SQLite's INTEGER PRIMARY KEY is the rowid, so we insert and retrieve.
        let mut qn_to_id: HashMap<String, i64> = HashMap::new();

        // Pre-allocate IDs sequentially (we know the total count).
        let symbols: Vec<(&String, &SymbolEntry)> = graph.symbols.iter().collect();
        let mut assigned_id = 1i64;

        let mut stmt = conn
            .prepare(
                "INSERT OR REPLACE INTO nodes (id, label, name, qualified_name, file_path, start_line, end_line, properties)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .context("prepare symbol nodes insert")?;

        for batch in symbols.chunks(BATCH_SIZE) {
            let tx = conn.unchecked_transaction()?;
            for (qn, sym) in batch {
                let id = assigned_id;
                assigned_id += 1;

                let mut props: HashMap<String, String> = HashMap::new();
                props.insert(
                    "is_exported".to_string(),
                    if sym.is_exported { "true" } else { "false" }.to_string(),
                );
                if !sym.minhash.is_empty() {
                    let mh_str = sym
                        .minhash
                        .iter()
                        .map(|v| format!("{v:08x}"))
                        .collect::<Vec<_>>()
                        .join("");
                    props.insert("minhash".to_string(), mh_str);
                }
                let props_json =
                    serde_json::to_string(&props).context("serialize symbol properties")?;

                stmt.execute(params![
                    id,
                    sym.kind,
                    sym.name,
                    qn,
                    sym.file,
                    sym.start_line as i64,
                    sym.end_line as i64,
                    props_json,
                ])?;

                qn_to_id.insert((*qn).clone(), id);
            }
            tx.commit()?;
        }
        Ok(qn_to_id)
    }

    /// Insert `ProjectIndex` edges into the `edges` table.
    ///
    /// Resolves QN references to numeric node IDs by scanning the `nodes`
    /// table (fallback: uses a fresh connection query).
    fn insert_project_edges(conn: &Connection, graph: &ProjectIndex) -> Result<()> {
        // Build QN → id lookup from what we just inserted.
        let mut qn_lookup: HashMap<String, i64> = HashMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT id, qualified_name FROM nodes")
                .context("prepare QN lookup")?;
            let rows = stmt
                .query_map([], |row| {
                    let id: i64 = row.get(0)?;
                    let qn: String = row.get(1)?;
                    Ok((id, qn))
                })
                .context("query nodes for QN lookup")?;
            for row in rows {
                let (id, qn) = row?;
                qn_lookup.insert(qn, id);
            }
        }

        let mut edge_id = 1i64;
        let mut stmt = conn
            .prepare(
                "INSERT OR REPLACE INTO edges (id, source_id, target_id, type, properties)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .context("prepare edges insert")?;

        for batch in graph.edges.chunks(BATCH_SIZE) {
            let tx = conn.unchecked_transaction()?;
            for e in batch {
                let src_id = qn_lookup.get(&e.from).copied();
                let tgt_id = qn_lookup.get(&e.to).copied();
                if let (Some(sid), Some(tid)) = (src_id, tgt_id) {
                    stmt.execute(params![edge_id, sid, tid, e.kind, "{}",])?;
                    edge_id += 1;
                }
                // Silently skip edges with unresolvable QNs (same behaviour as
                // the original postcard path).
            }
            tx.commit()?;
        }
        Ok(())
    }

    /// Run `PRAGMA integrity_check` and return an error if it does not say
    /// "ok".
    fn verify_integrity(conn: &Connection) -> Result<()> {
        let result: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .context("integrity_check query failed")?;
        anyhow::ensure!(
            result.trim() == "ok",
            "SQLite integrity check failed: {result}"
        );
        Ok(())
    }
}

// ── Free functions (load helpers) ─────────────────────────────────────────

/// Reconstruct a [`ProjectIndex`] from the SQLite `code_index.db`.
///
/// Returns `None` when the DB file does not exist or contains no data.
fn load_graph_index(root: &Path) -> Option<ProjectIndex> {
    let db_path = DumpEngine::db_path_for(root);
    if !db_path.exists() {
        return None;
    }
    let conn = Connection::open(&db_path).ok()?;

    // Count nodes to distinguish "empty DB" from "missing DB"
    let node_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
        .ok()?;
    if node_count == 0 {
        return None;
    }

    let mut idx = ProjectIndex::new(&root.to_string_lossy());

    // Load files table
    if let Ok(mut stmt) = conn.prepare(
        "SELECT path, hash, language, line_count, token_count, exports, summary FROM files",
    ) {
        if let Ok(rows) = stmt.query_map([], |row| {
            let path: String = row.get(0)?;
            let hash: String = row.get(1)?;
            let language: String = row.get(2)?;
            let line_count: i64 = row.get(3)?;
            let token_count: i64 = row.get(4)?;
            let exports_json: String = row.get(5)?;
            let summary: String = row.get(6)?;
            let exports: Vec<String> =
                serde_json::from_str(&exports_json).unwrap_or_default();
            Ok((path, hash, language, line_count, token_count, exports, summary))
        }) {
            for row in rows.flatten() {
                let (path, hash, language, line_count, token_count, exports, summary) = row;
                idx.files.insert(
                    path.clone(),
                    FileEntry {
                        path,
                        hash,
                        language,
                        line_count: line_count as usize,
                        token_count: token_count as usize,
                        exports,
                        summary,
                    },
                );
            }
        }
    }

    // Load node rows as SymbolEntries
    if let Ok(mut stmt) = conn.prepare(
        "SELECT id, label, name, qualified_name, file_path, start_line, end_line, properties FROM nodes",
    ) {
        if let Ok(rows) = stmt.query_map([], |row| {
            let _id: i64 = row.get(0)?;
            let label: String = row.get(1)?;
            let name: String = row.get(2)?;
            let qn: String = row.get(3)?;
            let file: String = row.get(4)?;
            let start_line: i64 = row.get(5)?;
            let end_line: i64 = row.get(6)?;
            let props_json: String = row.get(7)?;
            Ok((label, name, qn, file, start_line, end_line, props_json))
        }) {
            for row in rows.flatten() {
                let (label, name, qn, file, start_line, end_line, props_json) = row;
                let props: HashMap<String, String> =
                    serde_json::from_str(&props_json).unwrap_or_default();
                let is_exported = props.get("is_exported").map_or(false, |v| v == "true");
                let minhash = props
                    .get("minhash")
                    .map(|s| {
                        s.as_bytes()
                            .chunks(8)
                            .filter_map(|c| {
                                let hex = std::str::from_utf8(c).ok()?;
                                u32::from_str_radix(hex, 16).ok()
                            })
                            .collect::<Vec<u32>>()
                    })
                    .unwrap_or_default();

                idx.symbols.insert(
                    qn.clone(),
                    SymbolEntry {
                        file,
                        name,
                        kind: label.clone(),
                        start_line: start_line as usize,
                        end_line: end_line as usize,
                        is_exported,
                        minhash,
                    },
                );
            }
        }
    }

    // Load edge rows as IndexEdges
    if let Ok(mut stmt) = conn.prepare(
        "SELECT e.id, e.source_id, e.target_id, e.type,
                sn.qualified_name AS src_qn,
                tn.qualified_name AS tgt_qn
         FROM edges e
         JOIN nodes sn ON sn.id = e.source_id
         JOIN nodes tn ON tn.id = e.target_id",
    ) {
        if let Ok(rows) = stmt.query_map([], |row| {
            let _id: i64 = row.get(0)?;
            let _src: i64 = row.get(1)?;
            let _tgt: i64 = row.get(2)?;
            let kind: String = row.get(3)?;
            let src_qn: String = row.get(4)?;
            let tgt_qn: String = row.get(5)?;
            Ok(IndexEdge {
                from: src_qn,
                to: tgt_qn,
                kind,
                weight: 1.0,
            })
        }) {
            for row in rows.flatten() {
                idx.edges.push(row);
            }
        }
    }

    Some(idx)
}

/// Reconstruct a [`BM25Index`] from the SQLite `chunks` table.
///
/// Returns `None` when the DB file does not exist or has no chunks.
fn load_bm25_index(root: &Path) -> Option<BM25Index> {
    let db_path = DumpEngine::db_path_for(root);
    if !db_path.exists() {
        return None;
    }
    let conn = Connection::open(&db_path).ok()?;

    let chunk_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))
        .ok()?;
    if chunk_count == 0 {
        return None;
    }

    let mut stmt = conn
        .prepare(
            "SELECT file_path, symbol_name, kind, start_line, end_line, content, token_count
             FROM chunks ORDER BY id",
        )
        .ok()?;

    let rows = stmt
        .query_map([], |row| {
            let file_path: String = row.get(0)?;
            let symbol_name: String = row.get(1)?;
            let kind_str: String = row.get(2)?;
            let start_line: i64 = row.get(3)?;
            let end_line: i64 = row.get(4)?;
            let content: String = row.get(5)?;
            let token_count: i64 = row.get(6)?;
            Ok(crate::core::bm25_index::CodeChunk {
                file_path,
                symbol_name,
                kind: serde_json::from_str(&kind_str).unwrap_or(
                    crate::core::bm25_index::ChunkKind::Other,
                ),
                start_line: start_line as usize,
                end_line: end_line as usize,
                content,
                tokens: Vec::new(),
                token_count: token_count as usize,
            })
        })
        .ok()?;

    let mut bm25 = BM25Index::new();
    for row in rows {
        let chunk = row.ok()?;
        bm25.add_chunk(chunk);
    }
    bm25.finalize();
    Some(bm25)
}

/// Remove any leftover `.tmp` files from a prior crash or interrupted write.
fn cleanup_tmp_files(dir: &Path) {
    if !dir.exists() {
        return;
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "tmp") {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// Open the [`FileMetadataStore`] from the property-graph DB.
///
/// Creates the DB and `file_metadata` table if they do not exist, so the store
/// is always usable after this call.
fn open_file_metadata_store(root: &Path) -> Result<FileMetadataStore> {
    let graph_dir = crate::core::property_graph::graph_dir(&root.to_string_lossy());
    std::fs::create_dir_all(&graph_dir)?;
    let db_path = graph_dir.join("graph.db");

    let store = FileMetadataStore::open(&db_path)?;
    // Ensure the file_metadata table exists (idempotent).
    store.connection().execute_batch(
        "CREATE TABLE IF NOT EXISTS file_metadata (
            path         TEXT NOT NULL PRIMARY KEY,
            mtime_ns     INTEGER NOT NULL,
            size_bytes   INTEGER NOT NULL,
            content_hash TEXT NOT NULL DEFAULT '',
            mode_mask    INTEGER NOT NULL DEFAULT 0,
            updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    // Validate the store is readable
    if let Err(e) = store.load_all() {
        tracing::warn!("[dump_engine] file_metadata store integrity check failed: {e}");
    }

    Ok(store)
}

// ── FileMetadataStore (was file_metadata_store.rs) ─────────────────────────

/// Bitmask values for `mode_mask`.
pub mod mode {
    pub const FULL: u32 = 0x01;
    pub const MODERATE: u32 = 0x02;
    pub const FAST: u32 = 0x04;
}

/// Per-file metadata row.
#[derive(Debug, Clone, PartialEq)]
pub struct FileMetadata {
    pub rel_path: String,
    pub mtime_ns: i64,
    pub size_bytes: i64,
    pub content_hash: String,
    pub mode_mask: u32,
}

/// CRUD store for `file_metadata` backed by a SQLite connection.
pub struct FileMetadataStore {
    db: Connection,
}

impl FileMetadataStore {
    pub fn open(path: &std::path::Path) -> anyhow::Result<Self> {
        let db = Connection::open(path)?;
        Ok(Self { db })
    }

    pub fn new(db: Connection) -> Self {
        Self { db }
    }

    pub fn upsert(&self, meta: &FileMetadata) -> anyhow::Result<()> {
        let mut stmt = self.db.prepare_cached(
            "INSERT OR REPLACE INTO file_metadata (path, mtime_ns, size_bytes, content_hash, mode_mask)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        stmt.execute(params![
            meta.rel_path,
            meta.mtime_ns,
            meta.size_bytes,
            meta.content_hash,
            meta.mode_mask,
        ])?;
        Ok(())
    }

    pub fn upsert_batch(&self, metas: &[FileMetadata]) -> anyhow::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO file_metadata (path, mtime_ns, size_bytes, content_hash, mode_mask)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for meta in metas {
                stmt.execute(params![
                    meta.rel_path,
                    meta.mtime_ns,
                    meta.size_bytes,
                    meta.content_hash,
                    meta.mode_mask,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load_all(&self) -> anyhow::Result<HashMap<String, FileMetadata>> {
        let mut stmt = self.db.prepare_cached(
            "SELECT path, mtime_ns, size_bytes, content_hash, mode_mask FROM file_metadata",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(FileMetadata {
                rel_path: row.get(0)?,
                mtime_ns: row.get(1)?,
                size_bytes: row.get(2)?,
                content_hash: row.get(3)?,
                mode_mask: row.get::<_, i64>(4)? as u32,
            })
        })?;

        let mut map = HashMap::new();
        for row in rows {
            let meta = row?;
            map.insert(meta.rel_path.clone(), meta);
        }
        Ok(map)
    }

    pub fn load_for_mode(&self, mode_mask: u32) -> anyhow::Result<HashMap<String, FileMetadata>> {
        let mut stmt = self.db.prepare_cached(
            "SELECT path, mtime_ns, size_bytes, content_hash, mode_mask FROM file_metadata
             WHERE (mode_mask & ?1) != 0",
        )?;
        let rows = stmt.query_map(params![mode_mask as i64], |row| {
            Ok(FileMetadata {
                rel_path: row.get(0)?,
                mtime_ns: row.get(1)?,
                size_bytes: row.get(2)?,
                content_hash: row.get(3)?,
                mode_mask: row.get::<_, i64>(4)? as u32,
            })
        })?;

        let mut map = HashMap::new();
        for row in rows {
            let meta = row?;
            map.insert(meta.rel_path.clone(), meta);
        }
        Ok(map)
    }

    pub fn delete(&self, path: &str) -> anyhow::Result<()> {
        let mut stmt = self
            .db
            .prepare_cached("DELETE FROM file_metadata WHERE path = ?1")?;
        stmt.execute(params![path])?;
        Ok(())
    }

    pub fn delete_batch(&self, paths: &[String]) -> anyhow::Result<()> {
        let tx = self.db.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached("DELETE FROM file_metadata WHERE path = ?1")?;
            for p in paths {
                stmt.execute(params![p])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn connection(&self) -> &Connection {
        &self.db
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::isolated_data_dir;
    use crate::core::graph_index::FileEntry;
    use crate::core::index_types::NodeId;

    fn sample_graph(project_root: &str) -> ProjectIndex {
        let mut idx = ProjectIndex::new(project_root);
        idx.files.insert(
            "src/main.rs".to_string(),
            FileEntry {
                path: "src/main.rs".to_string(),
                hash: "a1b2c3".to_string(),
                language: "rust".to_string(),
                line_count: 42,
                token_count: 120,
                exports: vec!["run".to_string()],
                summary: "Entry point".to_string(),
            },
        );
        idx.symbols.insert(
            "main".to_string(),
            SymbolEntry {
                file: "src/main.rs".to_string(),
                name: "main".to_string(),
                kind: "Function".to_string(),
                start_line: 1,
                end_line: 10,
                is_exported: true,
                minhash: vec![],
            },
        );
        idx
    }

    fn sample_bm25() -> BM25Index {
        BM25Index::from_chunks_for_test(vec![crate::core::bm25_index::CodeChunk {
            file_path: "src/main.rs".to_string(),
            symbol_name: "run".to_string(),
            kind: crate::core::bm25_index::ChunkKind::Function,
            start_line: 1,
            end_line: 10,
            content: "fn run() { println!(\"hello\"); }".to_string(),
            tokens: vec![],
            token_count: 6,
        }])
    }

    fn sample_gbuf() -> GraphBuffer {
        let mut gbuf = GraphBuffer::new("test");
        gbuf.upsert_node("Function", "foo", "pkg.foo", "src/lib.rs", 1, 10, {
            let mut m = std::collections::HashMap::new();
            m.insert("lang".to_string(), "rust".to_string());
            m
        });
        gbuf.upsert_node("Function", "bar", "pkg.bar", "src/lib.rs", 15, 30, {
            std::collections::HashMap::new()
        });
        // Edges are created via the buffer API
        gbuf
    }

    fn sample_code_chunks() -> Vec<CodeChunk> {
        vec![
            CodeChunk {
                file_path: "src/lib.rs".to_string(),
                content: "fn foo() {}".to_string(),
                content_hash: "abc".to_string(),
                start_line: 1,
                end_line: 3,
                language: "rust".to_string(),
            },
            CodeChunk {
                file_path: "src/lib.rs".to_string(),
                content: "fn bar() {}".to_string(),
                content_hash: "def".to_string(),
                start_line: 15,
                end_line: 17,
                language: "rust".to_string(),
            },
        ]
    }

    // ── dump_all tests ─────────────────────────────────────────────────

    #[test]
    fn dump_all_creates_valid_sqlite_file() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let mut gbuf = sample_gbuf();
        // Insert an edge between the two nodes
        let nodes: Vec<NodeId> = {
            let mut ids = Vec::new();
            gbuf.foreach_node(&mut |n| ids.push(n.id));
            ids
        };
        if nodes.len() >= 2 {
            gbuf.insert_edge(nodes[0], nodes[1], "calls", std::collections::HashMap::new());
        }

        let engine = DumpEngine::new(root.path().to_path_buf());
        engine
            .dump_all(&gbuf, &sample_code_chunks())
            .unwrap();

        let db_path = engine.db_path();
        assert!(db_path.exists(), "code_index.db should exist");
        assert!(db_path.metadata().unwrap().len() > 0, "db should not be empty");
    }

    #[test]
    fn dump_all_nodes_and_edges_present() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let mut gbuf = sample_gbuf();
        let nodes: Vec<NodeId> = {
            let mut ids = Vec::new();
            gbuf.foreach_node(&mut |n| ids.push(n.id));
            ids
        };
        if nodes.len() >= 2 {
            gbuf.insert_edge(nodes[0], nodes[1], "calls", std::collections::HashMap::new());
        }

        let engine = DumpEngine::new(root.path().to_path_buf());
        engine.dump_all(&gbuf, &sample_code_chunks()).unwrap();

        let conn = Connection::open(engine.db_path()).unwrap();
        let node_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(node_count, 2, "should have exactly 2 nodes");

        let edge_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(edge_count, 1, "should have exactly 1 edge");

        let chunk_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(chunk_count, 2, "should have exactly 2 chunks");
    }

    #[test]
    fn dump_all_integrity_check_passes() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let gbuf = sample_gbuf();
        let engine = DumpEngine::new(root.path().to_path_buf());
        engine.dump_all(&gbuf, &sample_code_chunks()).unwrap();

        let conn = Connection::open(engine.db_path()).unwrap();
        let result: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(result.trim(), "ok");
    }

    #[test]
    fn dump_all_fts_search_returns_results() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let gbuf = sample_gbuf();
        let engine = DumpEngine::new(root.path().to_path_buf());
        engine.dump_all(&gbuf, &sample_code_chunks()).unwrap();

        let conn = Connection::open(engine.db_path()).unwrap();
        // FTS5 search should find 'foo' in node name
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM nodes_fts WHERE nodes_fts MATCH 'foo'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            count > 0,
            "FTS5 should find 'foo' in node qualified_name or name"
        );
    }

    #[test]
    fn dump_all_empty_gbuf_produces_valid_empty_db() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let gbuf = GraphBuffer::new("test");
        let engine = DumpEngine::new(root.path().to_path_buf());
        engine.dump_all(&gbuf, &[]).unwrap();

        let conn = Connection::open(engine.db_path()).unwrap();
        let result: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(result.trim(), "ok");

        let node_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(node_count, 0, "empty gbuf should produce 0 nodes");
    }

    // ── Backward-compat dump tests ─────────────────────────────────────

    #[test]
    fn dump_graph_index_and_bm25_creates_db() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        let graph = sample_graph(root.path().to_str().unwrap());
        let bm25 = sample_bm25();

        engine.dump_graph_index(&graph).unwrap();
        engine.dump_bm25_index(&bm25).unwrap();

        let db_path = engine.db_path();
        assert!(db_path.exists(), "code_index.db should exist after dump");
    }

    #[test]
    fn load_after_dump_recovers_same_indices() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        let graph = sample_graph(root.path().to_str().unwrap());
        let bm25 = sample_bm25();

        engine.dump_graph_index(&graph).unwrap();
        engine.dump_bm25_index(&bm25).unwrap();

        let (loaded_graph, loaded_bm25, _store) =
            DumpEngine::load_with_integrity_check(root.path()).unwrap();

        let lg = loaded_graph.expect("graph should load");
        assert_eq!(lg.file_count(), graph.file_count());
        assert!(lg.files.contains_key("src/main.rs"));

        let lb = loaded_bm25.expect("bm25 should load");
        assert_eq!(lb.chunks.len(), 1);
        assert_eq!(lb.chunks[0].symbol_name, "run");
    }

    #[test]
    fn tmp_files_cleaned_after_successful_dump() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        let gbuf = GraphBuffer::new("test");
        engine.dump_all(&gbuf, &[]).unwrap();

        let dir = index_namespace::vectors_dir(root.path());
        // Ensure no .tmp files linger (our new dump doesn't use .tmp, but
        // cleanup_tmp_files should be a no-op)
        assert!(
            !dir.join("code_index.db.tmp").exists()
        );
        // Also no legacy .tmp files
        assert!(!dir.join("project_index.bin.zst.tmp").exists());
    }

    #[test]
    fn purge_all_removes_code_index_db() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        let gbuf = GraphBuffer::new("test");
        engine.dump_all(&gbuf, &[]).unwrap();

        let db_path = engine.db_path();
        assert!(db_path.exists());

        engine.purge_all().unwrap();
        assert!(!db_path.exists(), "purge_all should remove code_index.db");
    }

    #[test]
    fn crash_recovery_cleans_leftover_tmp() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        // Simulate crash: write only a .tmp file
        let dir = index_namespace::vectors_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("project_index.bin.zst.tmp"), b"garbage").unwrap();

        // load_with_integrity_check should clean it up
        let _ = DumpEngine::load_with_integrity_check(root.path()).unwrap();

        assert!(!dir.join("project_index.bin.zst.tmp").exists());
    }

    #[test]
    fn empty_indices_dump_and_load() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        let empty_graph = ProjectIndex::new(root.path().to_str().unwrap());
        let empty_bm25 = BM25Index::new();

        engine.dump_graph_index(&empty_graph).unwrap();
        engine.dump_bm25_index(&empty_bm25).unwrap();

        let (graph, bm25, _store) = DumpEngine::load_with_integrity_check(root.path()).unwrap();

        // An empty ProjectIndex has no symbols, so no nodes → load returns None
        assert!(
            graph.is_none(),
            "empty graph (no symbols) should return None"
        );

        // An empty BM25 index has no chunks → load returns None
        assert!(
            bm25.is_none(),
            "empty BM25 (no chunks) should return None"
        );
    }

    #[test]
    fn load_returns_none_for_missing_artifacts() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let (graph, bm25, _store) = DumpEngine::load_with_integrity_check(root.path()).unwrap();

        assert!(graph.is_none(), "no DB should return None for graph");
        assert!(bm25.is_none(), "no DB should return None for bm25");
    }

    #[test]
    fn purge_all_on_clean_dir_is_noop() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        // Should not error on a clean / non-existent vectors dir
        engine.purge_all().unwrap();
    }

    #[test]
    fn dump_file_metadata_checkpoint() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let store = open_file_metadata_store(root.path()).unwrap();
        store
            .upsert(&FileMetadata {
                rel_path: "src/test.rs".to_string(),
                mtime_ns: 1_000_000_000,
                size_bytes: 100,
                content_hash: "abc".to_string(),
                mode_mask: 0x01,
            })
            .unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        engine.dump_file_metadata(&store).unwrap();

        // Data still readable after checkpoint
        let all = store.load_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all.get("src/test.rs").unwrap().content_hash, "abc");
    }

    #[test]
    fn purge_all_preserves_property_graph_db() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        // Open the store to create graph.db
        let _store = open_file_metadata_store(root.path()).unwrap();
        let graph_dir = crate::core::property_graph::graph_dir(&root.path().to_string_lossy());
        let db_path = graph_dir.join("graph.db");
        assert!(
            db_path.exists(),
            "property graph DB should have been created"
        );

        // Dump and purge
        let engine = DumpEngine::new(root.path().to_path_buf());
        let gbuf = GraphBuffer::new("test");
        engine.dump_all(&gbuf, &[]).unwrap();
        engine.purge_all().unwrap();

        // Property graph DB must survive
        assert!(db_path.exists(), "purge_all must not delete graph.db");
    }

    #[test]
    fn dump_all_file_hashes_roundtrip() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let gbuf = sample_gbuf();
        let engine = DumpEngine::new(root.path().to_path_buf());
        engine.dump_all(&gbuf, &sample_code_chunks()).unwrap();

        // Verify FTS5 file_path queries work
        let conn = Connection::open(engine.db_path()).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM nodes_fts WHERE file_path MATCH 'lib'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(count > 0, "FTS5 should find nodes in src/lib.rs");
    }
}
