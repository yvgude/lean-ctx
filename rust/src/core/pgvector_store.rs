//! Optional pgvector (PostgreSQL) backend for dense (embedding) search.
//!
//! This module is behind the `pgvector` feature flag. It mirrors the
//! `qdrant_store` API (namespaced per-project tables, md5-derived point ids,
//! delete-by-file + upsert incremental sync) but talks to a self-hosted
//! PostgreSQL instance with the `vector` extension installed.
//!
//! Deliberately dependency-free: SQL is executed through the `psql` CLI —
//! the same pattern the postgres context provider uses — so no async runtime
//! or native driver enters the dependency tree. Row output is read as
//! one JSON object per line (`json_build_object(...)::text`), which is robust
//! against arbitrary characters in file paths and symbol names.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::Path;

use serde::Deserialize;

use crate::core::bm25_index::{BM25Index, CodeChunk};

#[derive(Debug, Clone)]
pub struct PgvectorConfig {
    /// PostgreSQL connection string (postgres://user:pass@host:port/db).
    pub url: String,
    /// Connect timeout for each psql invocation (seconds).
    pub timeout_secs: u64,
    /// Table name prefix; the project namespace hash and dimensions are appended.
    pub table_prefix: String,
}

impl PgvectorConfig {
    pub fn from_env() -> Result<Self, String> {
        let url = std::env::var("LEANCTX_PGVECTOR_URL")
            .map_err(|_| "LEANCTX_PGVECTOR_URL is required for pgvector backend".to_string())?;
        let url = url.trim().to_string();
        if url.is_empty() {
            return Err("LEANCTX_PGVECTOR_URL is required for pgvector backend".to_string());
        }

        let timeout_secs = std::env::var("LEANCTX_PGVECTOR_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(10);

        let table_prefix = std::env::var("LEANCTX_PGVECTOR_TABLE_PREFIX")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "lctx_code_".to_string());
        validate_pg_identifier_prefix(&table_prefix)?;

        Ok(Self {
            url,
            timeout_secs,
            table_prefix,
        })
    }
}

#[derive(Debug, Clone)]
pub struct PgvectorStore {
    cfg: PgvectorConfig,
}

#[derive(Debug, Clone)]
pub struct PgvectorHit {
    pub score: f32,
    pub file_path: String,
    pub symbol_name: String,
    pub kind: crate::core::bm25_index::ChunkKind,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Deserialize)]
struct PgRow {
    score: f32,
    file_path: String,
    symbol_name: String,
    kind: String,
    start_line: usize,
    end_line: usize,
}

impl PgvectorStore {
    pub fn from_env() -> Result<Self, String> {
        let cfg = PgvectorConfig::from_env()?;
        Ok(Self { cfg })
    }

    /// Namespaced table name for a project root at given dimensionality.
    /// Mirrors `QdrantStore::collection_name` (prefix + namespace hash + dims).
    pub fn table_name(&self, root: &Path, dimensions: usize) -> Result<String, String> {
        let ns = crate::core::index_namespace::namespace_hash(root);
        let name = format!("{}{}_d{}", self.cfg.table_prefix, ns, dimensions);
        if name.len() > 63 {
            return Err(format!(
                "pgvector table name exceeds PostgreSQL's 63-byte identifier limit: {name}"
            ));
        }
        Ok(name)
    }

    /// Ensure the extension + table exist. Returns `true` if the table was created.
    pub fn ensure_table(&self, table: &str, dimensions: usize) -> Result<bool, String> {
        let existed = self.table_exists(table)?;
        if existed {
            return Ok(false);
        }
        let sql = format!(
            "CREATE EXTENSION IF NOT EXISTS vector;\n\
             CREATE TABLE IF NOT EXISTS {table} (\n\
               id BIGINT PRIMARY KEY,\n\
               file_path TEXT NOT NULL,\n\
               symbol_name TEXT NOT NULL,\n\
               kind TEXT NOT NULL,\n\
               start_line BIGINT NOT NULL,\n\
               end_line BIGINT NOT NULL,\n\
               embedding vector({dimensions}) NOT NULL\n\
             );\n\
             CREATE INDEX IF NOT EXISTS {table}_file_idx ON {table} (file_path);"
        );
        self.run_sql(&sql)?;
        Ok(true)
    }

    /// Same incremental semantics as the qdrant backend: fresh table gets a
    /// full upsert; otherwise changed files are replaced (delete + upsert).
    pub fn sync_index(
        &self,
        table: &str,
        index: &BM25Index,
        aligned_embeddings: &[Vec<f32>],
        changed_files: &[String],
        created_new: bool,
    ) -> Result<(), String> {
        if index.chunks.len() != aligned_embeddings.len() {
            return Err("embedding alignment length mismatch".to_string());
        }

        if created_new {
            return self.upsert_filtered(table, index, aligned_embeddings, None);
        }

        if changed_files.is_empty() {
            return Ok(());
        }

        let mut unique: Vec<String> = changed_files.to_vec();
        unique.sort();
        unique.dedup();

        for file in &unique {
            self.delete_by_file(table, file)?;
        }

        let changed_set: HashSet<&str> = unique.iter().map(String::as_str).collect();
        self.upsert_filtered(table, index, aligned_embeddings, Some(&changed_set))
    }

    pub fn search(
        &self,
        table: &str,
        query_vec: &[f32],
        limit: usize,
    ) -> Result<Vec<PgvectorHit>, String> {
        let vec_literal = vector_literal(query_vec);
        // Cosine distance operator `<=>`: similarity = 1 - distance.
        let sql = format!(
            "SELECT json_build_object(\
               'score', 1 - (embedding <=> '{vec_literal}'::vector), \
               'file_path', file_path, \
               'symbol_name', symbol_name, \
               'kind', kind, \
               'start_line', start_line, \
               'end_line', end_line\
             )::text \
             FROM {table} \
             ORDER BY embedding <=> '{vec_literal}'::vector \
             LIMIT {limit};"
        );
        let stdout = self.run_sql(&sql)?;

        let mut out = Vec::new();
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let row: PgRow = serde_json::from_str(line)
                .map_err(|e| format!("invalid pgvector row json: {e}"))?;
            out.push(PgvectorHit {
                score: row.score,
                file_path: row.file_path,
                symbol_name: row.symbol_name,
                kind: crate::core::dense_backend::kind_from_str(&row.kind),
                start_line: row.start_line,
                end_line: row.end_line,
            });
        }
        Ok(out)
    }

    fn table_exists(&self, table: &str) -> Result<bool, String> {
        let literal = sql_string_literal(table)?;
        let out = self.run_sql(&format!("SELECT to_regclass({literal}) IS NOT NULL;"))?;
        Ok(out.trim() == "t")
    }

    /// Upsert all chunks (or only those whose file is in `changed_set`),
    /// batched to keep individual statements bounded.
    fn upsert_filtered(
        &self,
        table: &str,
        index: &BM25Index,
        aligned_embeddings: &[Vec<f32>],
        changed_set: Option<&HashSet<&str>>,
    ) -> Result<(), String> {
        let mut batch: Vec<String> = Vec::new();
        for (i, chunk) in index.chunks.iter().enumerate() {
            if let Some(set) = changed_set
                && !set.contains(chunk.file_path.as_str())
            {
                continue;
            }
            let vec = aligned_embeddings
                .get(i)
                .ok_or_else(|| "embedding alignment missing".to_string())?;
            batch.push(values_row_for_chunk(chunk, vec)?);
            if batch.len() >= UPSERT_BATCH_ROWS {
                self.upsert_rows(table, &batch)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            self.upsert_rows(table, &batch)?;
        }
        Ok(())
    }

    fn upsert_rows(&self, table: &str, rows: &[String]) -> Result<(), String> {
        let sql = format!(
            "INSERT INTO {table} (id, file_path, symbol_name, kind, start_line, end_line, embedding)\n\
             VALUES\n{}\n\
             ON CONFLICT (id) DO UPDATE SET\n\
               file_path = EXCLUDED.file_path,\n\
               symbol_name = EXCLUDED.symbol_name,\n\
               kind = EXCLUDED.kind,\n\
               start_line = EXCLUDED.start_line,\n\
               end_line = EXCLUDED.end_line,\n\
               embedding = EXCLUDED.embedding;",
            rows.join(",\n")
        );
        self.run_sql(&sql).map(|_| ())
    }

    fn delete_by_file(&self, table: &str, file_path: &str) -> Result<(), String> {
        let literal = sql_string_literal(file_path)?;
        self.run_sql(&format!("DELETE FROM {table} WHERE file_path = {literal};"))
            .map(|_| ())
    }

    /// Run SQL through `psql` via a temp file (`-f`) so statement size is not
    /// limited by ARG_MAX. Returns stdout (tuples-only, unaligned).
    fn run_sql(&self, sql: &str) -> Result<String, String> {
        let mut tmp = tempfile::NamedTempFile::new()
            .map_err(|e| format!("pgvector: temp file failed: {e}"))?;
        tmp.write_all(sql.as_bytes())
            .map_err(|e| format!("pgvector: temp write failed: {e}"))?;
        tmp.flush()
            .map_err(|e| format!("pgvector: temp flush failed: {e}"))?;

        let output = std::process::Command::new("psql")
            .arg(&self.cfg.url)
            .args(["-X", "-q", "-v", "ON_ERROR_STOP=1", "-t", "-A", "-f"])
            .arg(tmp.path())
            .env("PGCONNECT_TIMEOUT", self.cfg.timeout_secs.to_string())
            .output()
            .map_err(|e| {
                format!("pgvector: failed to run psql (is the PostgreSQL client installed?): {e}")
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("pgvector: psql error: {}", stderr.trim()));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

const UPSERT_BATCH_ROWS: usize = 256;

/// One `(id, 'file', 'symbol', 'kind', start, end, '[...]'::vector)` row.
fn values_row_for_chunk(chunk: &CodeChunk, vector: &[f32]) -> Result<String, String> {
    let id = point_id_for_chunk(chunk) as i64; // BIGINT: deterministic wrap of the u64 hash
    let file = sql_string_literal(&chunk.file_path)?;
    let symbol = sql_string_literal(&chunk.symbol_name)?;
    let kind = sql_string_literal(crate::core::dense_backend::kind_to_str(&chunk.kind))?;
    Ok(format!(
        "({id}, {file}, {symbol}, {kind}, {}, {}, '{}'::vector)",
        chunk.start_line,
        chunk.end_line,
        vector_literal(vector)
    ))
}

/// Identical id scheme to `qdrant_store::point_id_for_chunk` so a project can
/// switch backends without changing point identity semantics.
fn point_id_for_chunk(chunk: &CodeChunk) -> u64 {
    use md5::{Digest, Md5};
    let mut h = Md5::new();
    h.update(chunk.file_path.as_bytes());
    h.update(chunk.start_line.to_le_bytes());
    h.update(chunk.end_line.to_le_bytes());
    h.update(chunk.symbol_name.as_bytes());
    h.update(crate::core::dense_backend::kind_to_str(&chunk.kind).as_bytes());
    let out = h.finalize();
    u64::from_le_bytes(out[0..8].try_into().unwrap_or([0u8; 8]))
}

/// pgvector input format: `[0.1,0.2,...]`.
fn vector_literal(vector: &[f32]) -> String {
    let mut s = String::with_capacity(vector.len() * 10 + 2);
    s.push('[');
    for (i, v) in vector.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        // `{v}` for f32 is locale-independent and round-trips through Postgres real parsing.
        s.push_str(&format!("{v}"));
    }
    s.push(']');
    s
}

/// Standard SQL single-quoted literal with `'` doubling. Rejects NUL bytes
/// (PostgreSQL cannot store them in TEXT anyway) and backslashes are safe
/// because standard_conforming_strings is on by default since PostgreSQL 9.1.
fn sql_string_literal(s: &str) -> Result<String, String> {
    if s.contains('\0') {
        return Err("pgvector: NUL byte in string".to_string());
    }
    Ok(format!("'{}'", s.replace('\'', "''")))
}

/// The table prefix is interpolated into SQL identifiers; enforce the same
/// whitelist the postgres provider uses for schema names.
fn validate_pg_identifier_prefix(name: &str) -> Result<(), String> {
    let valid_start = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let valid_rest = name
        .chars()
        .skip(1)
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if name.is_empty() || name.len() > 40 || !valid_start || !valid_rest {
        return Err(format!(
            "Invalid LEANCTX_PGVECTOR_TABLE_PREFIX: {name:?} (allowed: [A-Za-z_][A-Za-z0-9_$]*, max 40 chars)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bm25_index::ChunkKind;

    fn chunk(file: &str, name: &str, start: usize, end: usize, kind: ChunkKind) -> CodeChunk {
        CodeChunk {
            file_path: file.to_string(),
            symbol_name: name.to_string(),
            kind,
            start_line: start,
            end_line: end,
            content: "fn x() {}".to_string(),
            tokens: vec![],
            token_count: 0,
        }
    }

    #[test]
    fn point_id_matches_qdrant_scheme_and_is_stable() {
        let c = chunk("src/main.rs", "main", 1, 10, ChunkKind::Function);
        assert_eq!(point_id_for_chunk(&c), point_id_for_chunk(&c));
        let c2 = chunk("src/main.rs", "main", 2, 10, ChunkKind::Function);
        assert_ne!(point_id_for_chunk(&c), point_id_for_chunk(&c2));
    }

    #[test]
    fn sql_string_literal_escapes_quotes() {
        assert_eq!(sql_string_literal("a'b").unwrap(), "'a''b'");
        assert_eq!(sql_string_literal("plain").unwrap(), "'plain'");
        assert!(sql_string_literal("nul\0byte").is_err());
    }

    #[test]
    fn vector_literal_is_bracketed_csv() {
        assert_eq!(vector_literal(&[0.5, -1.0, 2.0]), "[0.5,-1,2]");
        assert_eq!(vector_literal(&[]), "[]");
    }

    #[test]
    fn values_row_contains_escaped_fields() {
        let c = chunk("src/a'b.rs", "fn'x", 3, 9, ChunkKind::Method);
        let row = values_row_for_chunk(&c, &[0.25, 0.75]).unwrap();
        assert!(row.contains("'src/a''b.rs'"));
        assert!(row.contains("'fn''x'"));
        assert!(row.contains("'Method'"));
        assert!(row.contains("'[0.25,0.75]'::vector"));
    }

    #[test]
    fn table_prefix_validation() {
        assert!(validate_pg_identifier_prefix("lctx_code_").is_ok());
        assert!(validate_pg_identifier_prefix("with space").is_err());
        assert!(validate_pg_identifier_prefix("1leading_digit").is_err());
        assert!(validate_pg_identifier_prefix("drop;table").is_err());
        assert!(validate_pg_identifier_prefix("").is_err());
    }

    #[test]
    fn config_requires_url() {
        let _env = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEANCTX_PGVECTOR_URL");
        assert!(PgvectorConfig::from_env().is_err());

        crate::test_env::set_var("LEANCTX_PGVECTOR_URL", "postgres://localhost/lctx");
        crate::test_env::remove_var("LEANCTX_PGVECTOR_TABLE_PREFIX");
        crate::test_env::remove_var("LEANCTX_PGVECTOR_TIMEOUT_SECS");
        let cfg = PgvectorConfig::from_env().unwrap();
        assert_eq!(cfg.url, "postgres://localhost/lctx");
        assert_eq!(cfg.table_prefix, "lctx_code_");
        assert_eq!(cfg.timeout_secs, 10);
        crate::test_env::remove_var("LEANCTX_PGVECTOR_URL");
    }

    /// Real round-trip against a live PostgreSQL+pgvector instance.
    /// Run explicitly (needs LEANCTX_PGVECTOR_URL + psql client on PATH):
    ///   LEANCTX_PGVECTOR_URL=postgres://... cargo test --lib pgvector_e2e -- --ignored
    #[test]
    #[ignore = "requires live PostgreSQL with pgvector extension (set LEANCTX_PGVECTOR_URL)"]
    fn pgvector_e2e_round_trip() {
        let store = PgvectorStore::from_env().expect("LEANCTX_PGVECTOR_URL must be set");
        let table = "lctx_e2e_round_trip_d3".to_string();
        let _ = store.run_sql(&format!("DROP TABLE IF EXISTS {table};"));

        // Fresh table + full upsert.
        assert!(
            store.ensure_table(&table, 3).unwrap(),
            "table should be new"
        );
        assert!(
            !store.ensure_table(&table, 3).unwrap(),
            "second call sees it"
        );

        let mut index = BM25Index::new();
        index
            .chunks
            .push(chunk("src/a.rs", "alpha", 1, 5, ChunkKind::Function));
        index
            .chunks
            .push(chunk("src/b.rs", "beta", 10, 20, ChunkKind::Struct));
        let embeddings = vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]];

        store
            .sync_index(&table, &index, &embeddings, &[], true)
            .unwrap();

        let hits = store.search(&table, &[1.0, 0.0, 0.0], 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].file_path, "src/a.rs");
        assert_eq!(hits[0].symbol_name, "alpha");
        assert!(hits[0].score > 0.99, "cosine sim of identical vec ~ 1.0");
        assert_eq!(hits[0].kind, ChunkKind::Function);

        // Incremental: b.rs changes (chunk moves), delete-by-file + re-upsert.
        index.chunks[1] = chunk("src/b.rs", "beta", 30, 40, ChunkKind::Struct);
        store
            .sync_index(
                &table,
                &index,
                &embeddings,
                &["src/b.rs".to_string()],
                false,
            )
            .unwrap();

        let hits = store.search(&table, &[0.0, 1.0, 0.0], 2).unwrap();
        assert_eq!(hits[0].file_path, "src/b.rs");
        assert_eq!(hits[0].start_line, 30, "stale row was replaced");
        assert_eq!(hits[0].end_line, 40);

        // Escaping survives the round trip.
        index
            .chunks
            .push(chunk("src/it's.rs", "q'uote", 2, 3, ChunkKind::Method));
        let embeddings = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![0.0, 0.0, 1.0],
        ];
        store
            .sync_index(
                &table,
                &index,
                &embeddings,
                &["src/it's.rs".to_string()],
                false,
            )
            .unwrap();
        let hits = store.search(&table, &[0.0, 0.0, 1.0], 1).unwrap();
        assert_eq!(hits[0].file_path, "src/it's.rs");
        assert_eq!(hits[0].symbol_name, "q'uote");

        store.run_sql(&format!("DROP TABLE {table};")).unwrap();
    }

    #[test]
    fn table_name_is_namespaced_and_bounded() {
        let _env = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEANCTX_PGVECTOR_URL", "postgres://localhost/lctx");
        let store = PgvectorStore::from_env().unwrap();
        let name = store
            .table_name(Path::new("/tmp/some-project"), 384)
            .unwrap();
        assert!(name.starts_with("lctx_code_"));
        assert!(name.ends_with("_d384"));
        assert!(name.len() <= 63);
        crate::test_env::remove_var("LEANCTX_PGVECTOR_URL");
    }
}
