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
//! - **nodes** — graph nodes with label, name, `qualified_name`, `file_path`,
//!   line range, and JSON properties.
//! - **edges** — directed edges with source/target node FK, type, and JSON
//!   properties.
//! - **`nodes_fts`** — FTS5 virtual table over `nodes` (enables `search_graph`).
//! - **`file_hashes`** — per-file content hashes for incremental rebuild support.
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
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::core::db::WalConnection;
use crate::core::graph_buffer::GraphBuffer;
use crate::core::graph_index::{FileEntry, IndexEdge, ProjectIndex, SymbolEntry};
use crate::core::index_namespace;
use crate::core::index_pipeline::discovery::DiscoveredFile;
// File metadata now lives in code_index.db's file_hashes table (no separate store).
use crate::core::index_types::{CodeChunk, FileHash, GbufEdge, GbufNode};

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

    #[must_use]
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }

    // ── Path helpers ────────────────────────────────────────────────────

    /// Full path to the SQLite database file.
    pub(crate) fn db_path(&self) -> PathBuf {
        let dir = index_namespace::vectors_dir(&self.project_root);
        dir.join(DB_FILENAME)
    }

    /// Full path to the SQLite database file for a given root (static variant).
    pub(crate) fn db_path_for(root: &Path) -> PathBuf {
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
                project   TEXT    NOT NULL,
                rel_path  TEXT    NOT NULL,
                mtime_ns  INTEGER DEFAULT 0,
                size      INTEGER DEFAULT 0,
                sha256    TEXT    DEFAULT '',
                PRIMARY KEY (project, rel_path)
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

            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
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
        let conn = WalConnection::open(&db_path).context("open code_index.db")?;

        // ══ DDL phase (auto-commit) ════════════════════════════════════
        // SQLite auto-commits before DDL, so never wrap these in BEGIN/COMMIT.

        // 1. Drop FTS5 virtual tables (fast rebuild)
        conn.execute("DROP TABLE IF EXISTS nodes_fts", [])?;
        conn.execute("DROP TABLE IF EXISTS chunks_fts", [])?;

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

        // 8. Rebuild chunks_fts FTS5 index (DDL — auto-commit)
        Self::rebuild_chunks_fts(&conn)?;

        // ══ Verification ═══════════════════════════════════════════════

        // 8. Integrity check
        Self::verify_integrity(&conn)?;

        // 9. Write schema version
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![crate::core::db::SCHEMA_VERSION],
        )?;

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
        let conn = WalConnection::open(&db_path).context("open code_index.db")?;

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

    // ── Load / Integrity ───────────────────────────────────────────────

    /// Load all three index components with integrity checks.
    ///
    /// Returns `None` for any component whose on-disk artifact is missing or
    /// corrupted.  Callers should trigger a full rebuild for the failed parts.
    ///
    /// Steps:
    /// 1. Open `code_index.db` (return `None` for graph/chunks if absent).
    /// 2. Load graph: read files + nodes + edges → reconstruct `ProjectIndex`.
    /// 3. Load chunks: read chunks table as raw data.
    ///
    pub fn load_with_integrity_check(
        root: &Path,
    ) -> Result<(
        Option<ProjectIndex>,
        Vec<crate::core::bm25_index::CodeChunk>,
    )> {
        let dir = index_namespace::vectors_dir(root);

        // 1. Clean up leftover .tmp files from crashes
        cleanup_tmp_files(&dir);

        // 2. Load graph index from SQLite
        let graph = load_graph_index(root);

        // 3. Load chunks from SQLite (raw data, no BM25 index built)
        let chunks = load_chunks(root);

        Ok((graph, chunks))
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
                let props_json =
                    serde_json::to_string(&node.properties).context("serialize node properties")?;
                stmt.execute(params![
                    i64::from(node.id.0),
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
                let props_json =
                    serde_json::to_string(&edge.properties).context("serialize edge properties")?;
                stmt.execute(params![
                    i64::from(edge.id.0),
                    i64::from(edge.source_id.0),
                    i64::from(edge.target_id.0),
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
        .context("create nodes_fts")?;

        // Populate from nodes
        conn.execute(
            "INSERT INTO nodes_fts(rowid, qualified_name, name, label, file_path)
             SELECT id, qualified_name, name, label, file_path FROM nodes",
            [],
        )
        .context("rebuild nodes_fts")?;

        Ok(())
    }

    /// Rebuild the FTS5 `chunks_fts` virtual table from the `chunks` table.
    fn rebuild_chunks_fts(conn: &Connection) -> Result<()> {
        // Re-create the FTS5 table (content-sync with chunks table)
        conn.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
                content, file_path, symbol_name,
                content='chunks',
                content_rowid='id'
            )",
            [],
        )
        .context("create chunks_fts")?;

        // Populate from chunks
        conn.execute(
            "INSERT INTO chunks_fts(rowid, content, file_path, symbol_name)
             SELECT id, content, file_path, symbol_name FROM chunks",
            [],
        )
        .context("rebuild chunks_fts")?;

        Ok(())
    }

    /// Insert BM25 [`CodeChunk`]s (new pipeline type) into the `chunks` table.
    fn insert_chunks(conn: &Connection, chunks: &[CodeChunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let mut stmt = conn
            .prepare(
                "INSERT INTO chunks (file_path, content, content_hash, start_line, end_line, language, symbol_name, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
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
                    c.symbol_name,
                    c.kind,
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
                    let mut mh_str = String::with_capacity(sym.minhash.len() * 8);
                    for v in &sym.minhash {
                        let _ = write!(mh_str, "{v:08x}");
                    }
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

    // ── File hashes (incremental rebuild support) ──────────────────────────

    /// Derive a project identifier from `project_root` for the `file_hashes`
    /// table.
    fn project_name(&self) -> String {
        self.project_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// Persist [`DiscoveredFile`] metadata into the `file_hashes` table.
    ///
    /// Each row stores `(project, rel_path, mtime_ns, size, sha256)` where
    /// `sha256` is left as an empty string (callers that want content hashing
    /// can fill it separately).
    pub fn insert_file_hashes(&self, files: &[DiscoveredFile]) -> Result<()> {
        let db_path = self.db_path();
        let wal_conn =
            WalConnection::open(&db_path).context("open code_index.db for file_hashes")?;
        let conn = wal_conn.connection();
        let project = self.project_name();

        let mut stmt = conn
            .prepare(
                "INSERT OR REPLACE INTO file_hashes (project, rel_path, mtime_ns, size, sha256)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .context("prepare file_hashes insert")?;

        for batch in files.chunks(BATCH_SIZE) {
            let tx = conn.unchecked_transaction()?;
            for f in batch {
                let mtime_ns = f
                    .mtime
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos() as i64);
                stmt.execute(params![
                    project,
                    f.rel_path,
                    mtime_ns,
                    f.size as i64,
                    "", // sha256 — empty by default
                ])?;
            }
            tx.commit()?;
        }
        Ok(())
    }

    /// Load all [`FileHash`] rows for a given project.
    ///
    /// Returns an empty `Vec` when the database does not exist or contains no
    /// matching rows.
    pub fn load_file_hashes(&self, project: &str) -> Result<Vec<FileHash>> {
        let db_path = self.db_path();
        if !db_path.exists() {
            return Ok(Vec::new());
        }
        let conn = Connection::open(&db_path).context("open code_index.db for load_file_hashes")?;

        // Re-create schema so the table exists even if dump_all was not called.
        Self::create_schema(&conn)?;

        let mut stmt = conn
            .prepare("SELECT project, rel_path, mtime_ns, size FROM file_hashes WHERE project = ?1")
            .context("prepare file_hashes select")?;

        let rows = stmt
            .query_map(params![project], |row| {
                Ok(FileHash {
                    project: row.get(0)?,
                    rel_path: row.get(1)?,
                    mtime_ns: row.get(2)?,
                    size: row.get(3)?,
                    sha256: String::new(),
                })
            })
            .context("query file_hashes")?;

        let mut hashes = Vec::new();
        for row in rows {
            hashes.push(row?);
        }
        Ok(hashes)
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
    let conn = WalConnection::open(&db_path).ok()?;

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
    ) && let Ok(rows) = stmt.query_map([], |row| {
        let path: String = row.get(0)?;
        let hash: String = row.get(1)?;
        let language: String = row.get(2)?;
        let line_count: i64 = row.get(3)?;
        let token_count: i64 = row.get(4)?;
        let exports_json: String = row.get(5)?;
        let summary: String = row.get(6)?;
        let exports: Vec<String> = serde_json::from_str(&exports_json).unwrap_or_default();
        Ok((
            path,
            hash,
            language,
            line_count,
            token_count,
            exports,
            summary,
        ))
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

    // Load node rows as SymbolEntries
    if let Ok(mut stmt) = conn.prepare(
        "SELECT id, label, name, qualified_name, file_path, start_line, end_line, properties FROM nodes",
    )
        && let Ok(rows) = stmt.query_map([], |row| {
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
                let is_exported = props.get("is_exported").is_some_and(|v| v == "true");
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

    // Load edge rows as IndexEdges
    if let Ok(mut stmt) = conn.prepare(
        "SELECT e.id, e.source_id, e.target_id, e.type,
                sn.qualified_name AS src_qn,
                tn.qualified_name AS tgt_qn
         FROM edges e
         JOIN nodes sn ON sn.id = e.source_id
         JOIN nodes tn ON tn.id = e.target_id",
    ) && let Ok(rows) = stmt.query_map([], |row| {
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

    Some(idx)
}

/// Load chunks from the SQLite `chunks` table as raw data.
///
/// Returns `None` when the DB file does not exist or has no chunks.
fn load_chunks(root: &Path) -> Vec<crate::core::bm25_index::CodeChunk> {
    let db_path = DumpEngine::db_path_for(root);
    if !db_path.exists() {
        return Vec::new();
    }
    let Ok(conn) = WalConnection::open(&db_path) else {
        return Vec::new();
    };

    let Ok(chunk_count) = conn.query_row("SELECT COUNT(*) FROM chunks", [], |row| {
        row.get::<_, i64>(0)
    }) else {
        return Vec::new();
    };
    if chunk_count == 0 {
        return Vec::new();
    }

    let Ok(mut stmt) = conn.prepare(
        "SELECT file_path, symbol_name, kind, start_line, end_line, content, token_count
         FROM chunks ORDER BY id",
    ) else {
        return Vec::new();
    };

    let Ok(rows) = stmt.query_map([], |row| {
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
            kind: serde_json::from_str(&kind_str)
                .unwrap_or(crate::core::bm25_index::ChunkKind::Other),
            start_line: start_line as usize,
            end_line: end_line as usize,
            content,
            tokens: Vec::new(),
            token_count: token_count as usize,
        })
    }) else {
        return Vec::new();
    };

    let mut chunks: Vec<crate::core::bm25_index::CodeChunk> = Vec::new();
    for chunk in rows.flatten() {
        chunks.push(chunk);
    }
    chunks
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

    fn sample_chunks() -> Vec<crate::core::bm25_index::CodeChunk> {
        vec![crate::core::bm25_index::CodeChunk {
            file_path: "src/main.rs".to_string(),
            symbol_name: "run".to_string(),
            kind: crate::core::bm25_index::ChunkKind::Function,
            start_line: 1,
            end_line: 10,
            content: "fn run() { println!(\"hello\"); }".to_string(),
            tokens: vec![],
            token_count: 6,
        }]
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
                symbol_name: "foo".to_string(),
                kind: "\"Function\"".to_string(),
            },
            CodeChunk {
                file_path: "src/lib.rs".to_string(),
                content: "fn bar() {}".to_string(),
                content_hash: "def".to_string(),
                start_line: 15,
                end_line: 17,
                language: "rust".to_string(),
                symbol_name: "bar".to_string(),
                kind: "\"Function\"".to_string(),
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
            gbuf.insert_edge(
                nodes[0],
                nodes[1],
                "calls",
                std::collections::HashMap::new(),
            );
        }

        let engine = DumpEngine::new(root.path().to_path_buf());
        engine.dump_all(&gbuf, &sample_code_chunks()).unwrap();

        let db_path = engine.db_path();
        assert!(db_path.exists(), "code_index.db should exist");
        assert!(
            db_path.metadata().unwrap().len() > 0,
            "db should not be empty"
        );
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
            gbuf.insert_edge(
                nodes[0],
                nodes[1],
                "calls",
                std::collections::HashMap::new(),
            );
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
    fn dump_graph_index_creates_db() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        let graph = sample_graph(root.path().to_str().unwrap());

        engine.dump_graph_index(&graph).unwrap();

        let db_path = engine.db_path();
        assert!(db_path.exists(), "code_index.db should exist after dump");
    }

    #[test]
    fn load_after_dump_recovers_same_indices() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let engine = DumpEngine::new(root.path().to_path_buf());
        let _graph = sample_graph(root.path().to_str().unwrap());
        let chunks = sample_chunks();

        // dump_all (which uses the new gbuf-based path) should produce a DB
        // that load_with_integrity_check can read back.
        // Convert bm25_index::CodeChunk to index_types::CodeChunk for dump_all.
        let code_chunks: Vec<crate::core::index_types::CodeChunk> = chunks
            .iter()
            .map(|c| crate::core::index_types::CodeChunk {
                file_path: c.file_path.clone(),
                content: c.content.clone(),
                content_hash: String::new(),
                start_line: c.start_line as u32,
                end_line: c.end_line as u32,
                language: String::new(),
                symbol_name: c.symbol_name.clone(),
                kind: serde_json::to_string(&c.kind).unwrap_or_default(),
            })
            .collect();
        engine.dump_all(&sample_gbuf(), &code_chunks).unwrap();

        let (loaded_graph, loaded_chunks) =
            DumpEngine::load_with_integrity_check(root.path()).unwrap();

        // dump_all writes nodes/edges to the nodes/edges tables but does not
        // populate the legacy files table — that path is covered by the
        // dump_graph_index test above. Here we verify nodes and chunks.
        let lg = loaded_graph.expect("graph should load");
        assert!(
            lg.symbols.len() >= 2,
            "dump_all must persist Function nodes as symbols"
        );

        assert!(!loaded_chunks.is_empty(), "chunks should load");
        assert_eq!(loaded_chunks.len(), chunks.len());
        assert_eq!(loaded_chunks[0].file_path, "src/main.rs");
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
        assert!(!dir.join("code_index.db.tmp").exists());
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

        engine.dump_graph_index(&empty_graph).unwrap();

        let (graph, chunks) = DumpEngine::load_with_integrity_check(root.path()).unwrap();

        // An empty ProjectIndex has no symbols, so no nodes → load returns None
        assert!(
            graph.is_none(),
            "empty graph (no symbols) should return None"
        );

        // No chunks written → load returns empty
        assert!(chunks.is_empty(), "no chunks should return empty");
    }

    #[test]
    fn load_returns_none_for_missing_artifacts() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let (graph, chunks) = DumpEngine::load_with_integrity_check(root.path()).unwrap();

        assert!(graph.is_none(), "no DB should return None for graph");
        assert!(chunks.is_empty(), "no DB should return empty for chunks");
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
    fn dump_all_file_hashes_roundtrip() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        // Create test files on disk so DiscoveredFile instances have real metadata.
        let test_dir = root.path().join("test_project_root");
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(test_dir.join("a.rs"), "fn a() {}").unwrap();
        std::fs::write(test_dir.join("b.rs"), "fn b() {}").unwrap();

        // Discover files so we get real mtime/size.
        let config = crate::core::index_pipeline::discovery::DiscoveryConfig {
            mode: crate::core::config::IndexingMode::Full,
            max_file_size: 10_000_000,
        };
        let files =
            crate::core::index_pipeline::discovery::discover_files(&test_dir, &config).unwrap();
        assert!(files.len() >= 2, "should discover at least a.rs and b.rs");

        // Dump (fresh schema, file_hashes table is created but empty).
        let gbuf = sample_gbuf();
        let engine = DumpEngine::new(test_dir.clone());
        engine.dump_all(&gbuf, &sample_code_chunks()).unwrap();

        // Persist file hashes.
        engine.insert_file_hashes(&files).unwrap();

        // Load and verify.
        let project = engine.project_name();
        let loaded = engine.load_file_hashes(&project).unwrap();

        assert_eq!(
            loaded.len(),
            files.len(),
            "should roundtrip all discovered files"
        );

        for f in &files {
            let mtime_ns = f
                .mtime
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as i64);
            let loaded_fh = loaded.iter().find(|h| h.rel_path == f.rel_path).unwrap();
            assert_eq!(loaded_fh.rel_path, f.rel_path);
            assert_eq!(loaded_fh.mtime_ns, mtime_ns);
            assert_eq!(loaded_fh.size, f.size as i64);
            assert_eq!(loaded_fh.project, project);
        }

        // Verify sha256 is empty string in DB.
        let conn = Connection::open(engine.db_path()).unwrap();
        let sha256: String = conn
            .query_row(
                "SELECT sha256 FROM file_hashes WHERE rel_path = ?1",
                params![files[0].rel_path],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sha256, "", "sha256 should default to empty string");
    }

    #[test]
    fn load_file_hashes_empty_db_returns_empty_vec() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();
        let engine = DumpEngine::new(root.path().to_path_buf());
        let loaded = engine.load_file_hashes("nonexistent").unwrap();
        assert!(loaded.is_empty(), "no DB should return empty vec");
    }

    // ── GraphBuffer roundtrip via SQLite ──────────────────────────

    #[test]
    fn graph_buffer_roundtrip_via_sqlite() {
        let _iso = isolated_data_dir();
        let root = tempfile::tempdir().unwrap();

        let mut original = GraphBuffer::new("test");
        let n1 = {
            let mut m = std::collections::HashMap::new();
            m.insert("lang".to_string(), "rust".to_string());
            original.upsert_node("Function", "foo", "pkg.foo", "src/lib.rs", 1, 10, m)
        };
        let n2 = original.upsert_node(
            "Function",
            "bar",
            "pkg.bar",
            "src/lib.rs",
            15,
            30,
            std::collections::HashMap::new(),
        );
        {
            let mut m = std::collections::HashMap::new();
            m.insert("inline".to_string(), "false".to_string());
            original.insert_edge(n1, n2, "calls", m);
        }

        // Dump to SQLite.
        let engine = DumpEngine::new(root.path().to_path_buf());
        engine.dump_all(&original, &[]).unwrap();

        // Load back.
        let db_path = engine.db_path();
        let loaded = GraphBuffer::load_from_db(&db_path, "test").unwrap();

        // Verify nodes.
        assert_eq!(loaded.node_count(), 2, "node count must match");
        let foo = loaded.find_by_qn("pkg.foo").expect("pkg.foo must exist");
        assert_eq!(foo.name, "foo");
        assert_eq!(foo.label, "Function");
        assert_eq!(foo.properties.get("lang").unwrap(), "rust");

        let bar = loaded.find_by_qn("pkg.bar").expect("pkg.bar must exist");
        assert_eq!(bar.name, "bar");

        // Verify edges.
        assert_eq!(loaded.edge_count(), 1, "edge count must match");
        let n1_loaded = foo.id;
        let n2_loaded = bar.id;
        assert!(
            loaded.edge_dedup_key(n1_loaded, n2_loaded, "calls"),
            "edge (pkg.foo) → (pkg.bar) must exist"
        );
    }

    #[test]
    fn graph_buffer_load_corrupt_db_returns_err() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("corrupt.db");
        // Write garbage that is not a valid SQLite database.
        std::fs::write(&db_path, b"this is not a valid SQLite database").unwrap();

        let result = GraphBuffer::load_from_db(&db_path, "test");
        assert!(result.is_err(), "loading a corrupt DB should return Err");
    }
}
