use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader};

use crate::core::data_dir::lean_ctx_data_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FactPrivacy {
    #[default]
    ProjectOnly,
    LinkedProjects,
    Team,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BoundaryPolicy {
    pub cross_project_search: bool,
    pub cross_project_import: bool,
    pub audit_cross_access: bool,
    /// Controls whether universal (cross-project) gotchas are loaded.
    /// When false, only project-scoped gotchas are used.
    pub universal_gotchas_enabled: bool,
}

impl Default for BoundaryPolicy {
    fn default() -> Self {
        Self {
            cross_project_search: false,
            cross_project_import: false,
            audit_cross_access: true,
            universal_gotchas_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossProjectEventType {
    Search,
    Import,
    Recall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossProjectAuditEvent {
    pub timestamp: String,
    pub event_type: CrossProjectEventType,
    pub source_project_hash: String,
    pub target_project_hash: String,
    pub tool: String,
    pub action: String,
    pub facts_accessed: usize,
    pub allowed: bool,
    pub policy_reason: String,
}

#[must_use]
pub fn check_boundary(
    source_hash: &str,
    target_hash: &str,
    policy: &BoundaryPolicy,
    event_type: &CrossProjectEventType,
) -> bool {
    if is_same_project_identity(source_hash, target_hash) {
        return true;
    }
    match event_type {
        CrossProjectEventType::Import => policy.cross_project_import,
        CrossProjectEventType::Search | CrossProjectEventType::Recall => {
            policy.cross_project_search
        }
    }
}

#[must_use]
pub fn is_same_project_identity(hash_a: &str, hash_b: &str) -> bool {
    !hash_a.is_empty() && !hash_b.is_empty() && hash_a == hash_b
}

/// Cap for the operational cross-project audit mirror. Kept >= the largest limit served
/// by the /v1/audit/events endpoint (capped at 1000) so no reader observes truncation.
/// The hash-chained compliance trail is separate (`core::audit_trail`) and not rotated here.
const MAX_AUDIT_LINES: usize = 2000;

pub fn record_audit_event(event: &CrossProjectAuditEvent) {
    let dir = match lean_ctx_data_dir() {
        Ok(d) => d.join("audit"),
        Err(e) => {
            tracing::warn!("cannot resolve data dir for audit: {e}");
            return;
        }
    };
    if let Err(e) = fs::create_dir_all(&dir) {
        tracing::warn!("cannot create audit dir {}: {e}", dir.display());
        return;
    }
    let path = dir.join("cross-project.jsonl");
    let line = match serde_json::to_string(event) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("cannot serialize audit event: {e}");
            return;
        }
    };
    // Rotating cap (mirrors server_metrics::append_tool_call_log): bound the jsonl to
    // MAX_AUDIT_LINES so disk growth is finite. This is the best-effort operational
    // mirror — the tamper-evident, hash-chained compliance trail lives in
    // core::audit_trail and is intentionally not rotated here.
    let mut lines: Vec<String> = fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .map(std::string::ToString::to_string)
        .collect();
    lines.push(line);
    if lines.len() > MAX_AUDIT_LINES {
        let excess = lines.len() - MAX_AUDIT_LINES;
        lines.drain(0..excess);
    }
    if let Err(e) = fs::write(&path, lines.join("\n") + "\n") {
        tracing::warn!("cannot write audit log {}: {e}", path.display());
    }
}

#[must_use]
pub fn load_audit_events(limit: usize) -> Vec<CrossProjectAuditEvent> {
    let path = match lean_ctx_data_dir() {
        Ok(d) => d.join("audit").join("cross-project.jsonl"),
        Err(_) => return Vec::new(),
    };
    let Ok(file) = fs::File::open(&path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut events: Vec<CrossProjectAuditEvent> = reader
        .lines()
        .filter_map(|line| {
            let line = line.ok()?;
            serde_json::from_str(&line).ok()
        })
        .collect();
    if events.len() > limit {
        events = events.split_off(events.len() - limit);
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_check_same_project_always_allowed() {
        let policy = BoundaryPolicy::default();
        assert!(check_boundary(
            "abc123",
            "abc123",
            &policy,
            &CrossProjectEventType::Search,
        ));
        assert!(check_boundary(
            "abc123",
            "abc123",
            &policy,
            &CrossProjectEventType::Import,
        ));
    }

    #[test]
    fn boundary_check_cross_project_respects_policy() {
        let deny_all = BoundaryPolicy::default();
        assert!(!check_boundary(
            "proj_a",
            "proj_b",
            &deny_all,
            &CrossProjectEventType::Search,
        ));
        assert!(!check_boundary(
            "proj_a",
            "proj_b",
            &deny_all,
            &CrossProjectEventType::Import,
        ));

        let allow_search = BoundaryPolicy {
            cross_project_search: true,
            ..Default::default()
        };
        assert!(check_boundary(
            "proj_a",
            "proj_b",
            &allow_search,
            &CrossProjectEventType::Search,
        ));
        assert!(!check_boundary(
            "proj_a",
            "proj_b",
            &allow_search,
            &CrossProjectEventType::Import,
        ));
    }

    #[test]
    fn same_identity_detection() {
        assert!(is_same_project_identity("hash1", "hash1"));
        assert!(!is_same_project_identity("hash1", "hash2"));
        assert!(!is_same_project_identity("", ""));
        assert!(!is_same_project_identity("hash1", ""));
    }

    #[test]
    fn audit_event_roundtrip() {
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());

        let event = CrossProjectAuditEvent {
            timestamp: chrono::Utc::now().to_rfc3339(),
            event_type: CrossProjectEventType::Search,
            source_project_hash: "src_hash".into(),
            target_project_hash: "tgt_hash".into(),
            tool: "ctx_knowledge".into(),
            action: "recall".into(),
            facts_accessed: 3,
            allowed: false,
            policy_reason: "cross_project_search disabled".into(),
        };

        record_audit_event(&event);
        record_audit_event(&event);

        let loaded = load_audit_events(10);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].source_project_hash, "src_hash");
        assert_eq!(loaded[0].facts_accessed, 3);
        assert!(!loaded[0].allowed);

        let limited = load_audit_events(1);
        assert_eq!(limited.len(), 1);

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
