use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::core::index_namespace;
use crate::core::pipeline_lock::{PipelineLock, PipelineLockError};

#[derive(Debug, Serialize, Default)]
pub struct DiskStatus {
    pub exists: bool,
    pub size_bytes: Option<u64>,
    pub file_count: Option<u64>,
    pub modified_at: Option<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct DiskStatusAll {
    pub graph_index: DiskStatus,
    pub bm25_index: DiskStatus,
    pub code_graph: DiskStatus,
    pub semantic_index: DiskStatus,
}

fn disk_status_for_graph(project_root: &str) -> DiskStatus {
    let root = Path::new(project_root);
    let db_path = index_namespace::vectors_dir(root).join("code_index.db");
    if !db_path.exists() {
        return DiskStatus::default();
    }
    let meta = std::fs::metadata(&db_path).ok();
    let file_count = rusqlite::Connection::open(&db_path)
        .ok()
        .and_then(|conn| {
            conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get::<_, i64>(0))
                .ok()
        })
        .map(|c| c as u64);
    DiskStatus {
        exists: true,
        size_bytes: meta.as_ref().map(std::fs::Metadata::len),
        file_count,
        modified_at: meta.and_then(|m| m.modified().ok()).map(format_time),
    }
}

fn disk_status_for_bm25(project_root: &str) -> DiskStatus {
    let root = Path::new(project_root);
    let db_path = index_namespace::vectors_dir(root).join("code_index.db");
    if !db_path.exists() {
        return DiskStatus::default();
    }
    let meta = std::fs::metadata(&db_path).ok();
    let chunk_count = rusqlite::Connection::open(&db_path)
        .ok()
        .and_then(|conn| {
            conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0))
                .ok()
        })
        .map(|c| c as u64);
    DiskStatus {
        exists: true,
        size_bytes: meta.as_ref().map(std::fs::Metadata::len),
        file_count: chunk_count,
        modified_at: meta.and_then(|m| m.modified().ok()).map(format_time),
    }
}

fn disk_status_for_code_graph(project_root: &str) -> DiskStatus {
    let dir = crate::core::property_graph::graph_dir(project_root);
    let db_path = dir.join("graph.db");
    if !db_path.exists() {
        return DiskStatus::default();
    }
    let meta = std::fs::metadata(&db_path).ok();
    let node_count = crate::core::property_graph::CodeGraph::open(project_root)
        .ok()
        .and_then(|g| {
            g.connection()
                .query_row("SELECT count(*) FROM nodes", [], |r| r.get::<_, i64>(0))
                .ok()
                .map(|c| c as u64)
        });
    DiskStatus {
        exists: true,
        size_bytes: meta.as_ref().map(std::fs::Metadata::len),
        file_count: node_count,
        modified_at: meta.and_then(|m| m.modified().ok()).map(format_time),
    }
}

fn format_time(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let dt = chrono::DateTime::from_timestamp(secs as i64, 0);
    dt.map_or_else(
        || format!("{secs}"),
        |d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
    )
}

pub fn disk_status_for_semantic(project_root: &str) -> DiskStatus {
    let root = Path::new(project_root);
    let dir = index_namespace::vectors_dir(root);
    let bin_path = dir.join("embeddings.bin");
    if !bin_path.exists() {
        return DiskStatus::default();
    }
    let meta = std::fs::metadata(&bin_path).ok();
    DiskStatus {
        exists: true,
        size_bytes: meta.as_ref().map(std::fs::Metadata::len),
        file_count: None,
        modified_at: meta.and_then(|m| m.modified().ok()).map(format_time),
    }
}

pub fn disk_status(project_root: &str) -> DiskStatusAll {
    DiskStatusAll {
        graph_index: disk_status_for_graph(project_root),
        bm25_index: disk_status_for_bm25(project_root),
        code_graph: disk_status_for_code_graph(project_root),
        semantic_index: disk_status_for_semantic(project_root),
    }
}

pub fn status_json(project_root: &str) -> String {
    serde_json::to_string(&disk_status(project_root)).unwrap_or_else(|_| "{}".to_string())
}

/// Returns `true` if any process currently holds the index-pipeline lock.
pub fn is_building() -> bool {
    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return false;
    };
    match PipelineLock::try_acquire(&data_dir) {
        Ok(_lock) => {
            // Acquired → no one was building. Lock drops at end of scope.
            false
        }
        Err(PipelineLockError::AlreadyLocked(_)) => true,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_json_is_valid_json() {
        let s = status_json("/tmp");
        let _: serde_json::Value = serde_json::from_str(&s).unwrap();
    }
}
