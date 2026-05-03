use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::knowledge::ProjectKnowledge;
use crate::core::session::SessionState;
use crate::core::workflow::WorkflowRun;
use crate::tools::ToolCallRecord;

const SCHEMA_VERSION: u32 = 1;
const MAX_KNOWLEDGE_FACTS: usize = 50;
const MAX_CURATED_REFS: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffLedgerV1 {
    pub schema_version: u32,
    pub created_at: String,
    pub content_md5: String,
    pub manifest_md5: String,
    pub project_root: Option<String>,
    pub agent_id: Option<String>,
    pub client_name: Option<String>,
    pub workflow: Option<WorkflowRun>,
    pub session_snapshot: String,
    pub session: SessionExcerpt,
    pub tool_calls: ToolCallsSummary,
    pub evidence_keys: Vec<String>,
    pub knowledge: KnowledgeExcerpt,
    pub curated_refs: Vec<CuratedRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionExcerpt {
    pub id: String,
    pub task: Option<String>,
    pub decisions: Vec<String>,
    pub findings: Vec<String>,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolCallsSummary {
    pub total: usize,
    pub by_tool: BTreeMap<String, u64>,
    pub by_ctx_read_mode: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KnowledgeExcerpt {
    pub project_hash: Option<String>,
    pub facts: Vec<KnowledgeFactMini>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeFactMini {
    pub category: String,
    pub key: String,
    pub value: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratedRef {
    pub path: String,
    pub mode: String,
    pub content_md5: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct CreateLedgerInput {
    pub agent_id: Option<String>,
    pub client_name: Option<String>,
    pub project_root: Option<String>,
    pub session: SessionState,
    pub tool_calls: Vec<ToolCallRecord>,
    pub workflow: Option<WorkflowRun>,
    pub curated_refs: Vec<(String, String)>, // (abs_path, signatures_text)
}

pub fn create_ledger(input: CreateLedgerInput) -> Result<(HandoffLedgerV1, PathBuf), String> {
    let manifest_md5 = manifest_md5();

    let mut evidence_keys: BTreeSet<String> = BTreeSet::new();
    for ev in &input.session.evidence {
        evidence_keys.insert(ev.key.clone());
    }

    let mut by_tool: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_mode: BTreeMap<String, u64> = BTreeMap::new();
    for call in &input.tool_calls {
        *by_tool.entry(call.tool.clone()).or_insert(0) += 1;
        if call.tool == "ctx_read" {
            if let Some(m) = call.mode.as_deref() {
                *by_mode.entry(m.to_string()).or_insert(0) += 1;
            }
        }
    }

    let session_excerpt = SessionExcerpt {
        id: input.session.id.clone(),
        task: input.session.task.as_ref().map(|t| t.description.clone()),
        decisions: input
            .session
            .decisions
            .iter()
            .rev()
            .take(10)
            .map(|d| d.summary.clone())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
        findings: input
            .session
            .findings
            .iter()
            .rev()
            .take(20)
            .map(|f| f.summary.clone())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
        next_steps: input.session.next_steps.iter().take(20).cloned().collect(),
    };

    let knowledge_excerpt = build_knowledge_excerpt(input.project_root.as_deref());

    let mut curated = Vec::new();
    for (p, text) in input.curated_refs.into_iter().take(MAX_CURATED_REFS) {
        let md5 = md5_hex(text.as_bytes());
        curated.push(CuratedRef {
            path: p,
            mode: "signatures".to_string(),
            content_md5: md5,
            content: text,
        });
    }

    let mut ledger = HandoffLedgerV1 {
        schema_version: SCHEMA_VERSION,
        created_at: chrono::Local::now().to_rfc3339(),
        content_md5: String::new(),
        manifest_md5,
        project_root: input.project_root,
        agent_id: input.agent_id,
        client_name: input.client_name,
        workflow: input.workflow,
        session_snapshot: input.session.build_compaction_snapshot(),
        session: session_excerpt,
        tool_calls: ToolCallsSummary {
            total: input.tool_calls.len(),
            by_tool,
            by_ctx_read_mode: by_mode,
        },
        evidence_keys: evidence_keys.into_iter().collect(),
        knowledge: knowledge_excerpt,
        curated_refs: curated,
    };

    let md5 = ledger_content_md5(&ledger);
    ledger.content_md5.clone_from(&md5);

    let path = ledger_path(&ledger.created_at, &md5)?;
    let json = serde_json::to_string_pretty(&ledger).map_err(|e| format!("serialize: {e}"))?;
    crate::config_io::write_atomic_with_backup(&path, &(json + "\n"))
        .map_err(|e| format!("write {}: {e}", path.display()))?;

    Ok((ledger, path))
}

pub fn list_ledgers() -> Vec<PathBuf> {
    let dir = handoffs_dir().ok();
    let Some(dir) = dir else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut items: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    items.sort();
    items.reverse();
    items
}

pub fn load_ledger(path: &Path) -> Result<HandoffLedgerV1, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("parse {}: {e}", path.display()))
}

pub fn clear_ledgers() -> Result<u32, String> {
    let dir = handoffs_dir()?;
    let mut removed = 0u32;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if std::fs::remove_file(&p).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

fn build_knowledge_excerpt(project_root: Option<&str>) -> KnowledgeExcerpt {
    let Some(root) = project_root else {
        return KnowledgeExcerpt::default();
    };
    let Some(knowledge) = ProjectKnowledge::load(root) else {
        return KnowledgeExcerpt::default();
    };

    let mut facts = Vec::new();
    for f in knowledge.facts.iter().filter(|f| f.is_current()) {
        facts.push(KnowledgeFactMini {
            category: f.category.clone(),
            key: f.key.clone(),
            value: f.value.clone(),
            confidence: f.confidence,
        });
        if facts.len() >= MAX_KNOWLEDGE_FACTS {
            break;
        }
    }

    KnowledgeExcerpt {
        project_hash: Some(knowledge.project_hash.clone()),
        facts,
    }
}

fn ledger_path(created_at: &str, md5: &str) -> Result<PathBuf, String> {
    let dir = handoffs_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create_dir_all {}: {e}", dir.display()))?;
    let ts = created_at
        .chars()
        .filter(char::is_ascii_digit)
        .take(14)
        .collect::<String>();
    let name = format!("{ts}-{md5}.json");
    Ok(dir.join(name))
}

fn handoffs_dir() -> Result<PathBuf, String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()
        .map_err(|e| e.clone())?
        .join("handoffs");
    Ok(dir)
}

fn manifest_md5() -> String {
    let v = crate::core::mcp_manifest::manifest_value();
    let canon = canonicalize_json(&v);
    md5_hex(canon.to_string().as_bytes())
}

fn ledger_content_md5(ledger: &HandoffLedgerV1) -> String {
    let mut tmp = ledger.clone();
    tmp.content_md5.clear();
    let v = serde_json::to_value(&tmp).unwrap_or(Value::Null);
    let canon = canonicalize_json(&v);
    md5_hex(canon.to_string().as_bytes())
}

fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffPackage {
    pub ledger: HandoffLedgerV1,
    pub intent: Option<IntentSnapshot>,
    pub context_snapshot: Option<ContextSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentSnapshot {
    pub task_type: String,
    pub scope: String,
    pub targets: Vec<String>,
    pub keywords: Vec<String>,
    pub language_hint: Option<String>,
    pub urgency: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub window_size: usize,
    pub tokens_used: usize,
    pub tokens_saved: usize,
    pub files_loaded: Vec<LoadedFileInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedFileInfo {
    pub path: String,
    pub mode: String,
    pub tokens: usize,
}

impl HandoffPackage {
    pub fn build(
        ledger: HandoffLedgerV1,
        intent: Option<&super::intent_engine::StructuredIntent>,
        context: Option<&crate::core::context_ledger::ContextLedger>,
    ) -> Self {
        let intent_snap = intent.map(|i| IntentSnapshot {
            task_type: i.task_type.as_str().to_string(),
            scope: match i.scope {
                super::intent_engine::IntentScope::SingleFile => "single_file",
                super::intent_engine::IntentScope::MultiFile => "multi_file",
                super::intent_engine::IntentScope::CrossModule => "cross_module",
                super::intent_engine::IntentScope::ProjectWide => "project_wide",
            }
            .to_string(),
            targets: i.targets.clone(),
            keywords: i.keywords.clone(),
            language_hint: i.language_hint.clone(),
            urgency: i.urgency,
        });

        let ctx_snap = context.map(|c| ContextSnapshot {
            window_size: c.window_size,
            tokens_used: c.total_tokens_sent,
            tokens_saved: c.total_tokens_saved,
            files_loaded: c
                .entries
                .iter()
                .map(|e| LoadedFileInfo {
                    path: e.path.clone(),
                    mode: e.mode.clone(),
                    tokens: e.sent_tokens,
                })
                .collect(),
        });

        HandoffPackage {
            ledger,
            intent: intent_snap,
            context_snapshot: ctx_snap,
        }
    }

    pub fn format_compact(&self) -> String {
        let mut out = String::new();

        out.push_str("--- HANDOFF ---\n");
        if let Some(ref intent) = self.intent {
            out.push_str(&format!(
                "TASK: {} (scope: {}, conf: {})\n",
                intent.task_type,
                intent.scope,
                if intent.urgency > 0.5 {
                    "URGENT"
                } else {
                    "normal"
                }
            ));
            if !intent.targets.is_empty() {
                out.push_str(&format!("TARGETS: {}\n", intent.targets.join(", ")));
            }
            if let Some(ref lang) = intent.language_hint {
                out.push_str(&format!("LANG: {lang}\n"));
            }
        }

        if let Some(ref ctx) = self.context_snapshot {
            out.push_str(&format!(
                "CTX: {}/{} tokens, {} files, saved {}\n",
                ctx.tokens_used,
                ctx.window_size,
                ctx.files_loaded.len(),
                ctx.tokens_saved,
            ));
        }

        if !self.ledger.session.decisions.is_empty() {
            out.push_str("DECISIONS:\n");
            for d in &self.ledger.session.decisions {
                out.push_str(&format!("  - {d}\n"));
            }
        }

        if !self.ledger.session.findings.is_empty() {
            out.push_str("FINDINGS:\n");
            for f in self.ledger.session.findings.iter().take(5) {
                out.push_str(&format!("  - {f}\n"));
            }
        }

        if !self.ledger.session.next_steps.is_empty() {
            out.push_str("NEXT:\n");
            for s in &self.ledger.session.next_steps {
                out.push_str(&format!("  - {s}\n"));
            }
        }

        out.push_str("---\n");
        out
    }
}

fn canonicalize_json(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                if let Some(val) = map.get(k) {
                    out.insert(k.clone(), canonicalize_json(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(canonicalize_json).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::data_dir::test_env_lock;
    use tempfile::tempdir;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(ref previous) = self.previous {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn sample_session() -> SessionState {
        let mut session = SessionState::new();
        session.set_task("stabilize handoff ledger tests", None);
        session.add_decision("Keep ledger deterministic", None);
        session.add_finding(
            Some("rust/src/core/handoff_ledger.rs"),
            Some(42),
            "Edge case coverage extended",
        );
        session.record_manual_evidence("tool:ctx_read", Some("handoff_ledger.rs"));
        session
    }

    fn sample_tool_calls() -> Vec<ToolCallRecord> {
        vec![
            ToolCallRecord {
                tool: "ctx_read".to_string(),
                original_tokens: 900,
                saved_tokens: 700,
                mode: Some("map".to_string()),
                duration_ms: 12,
                timestamp: chrono::Utc::now().to_rfc3339(),
            },
            ToolCallRecord {
                tool: "ctx_shell".to_string(),
                original_tokens: 120,
                saved_tokens: 60,
                mode: None,
                duration_ms: 8,
                timestamp: chrono::Utc::now().to_rfc3339(),
            },
        ]
    }

    fn make_minimal_ledger() -> HandoffLedgerV1 {
        HandoffLedgerV1 {
            schema_version: SCHEMA_VERSION,
            created_at: "20260429T000000".to_string(),
            content_md5: String::new(),
            manifest_md5: "manifest".to_string(),
            project_root: None,
            agent_id: Some("agent-test".to_string()),
            client_name: Some("cursor".to_string()),
            workflow: None,
            session_snapshot: "snapshot".to_string(),
            session: SessionExcerpt::default(),
            tool_calls: ToolCallsSummary::default(),
            evidence_keys: Vec::new(),
            knowledge: KnowledgeExcerpt::default(),
            curated_refs: Vec::new(),
        }
    }

    #[test]
    fn create_load_list_clear_ledger_roundtrip() {
        let _env_lock = test_env_lock();
        let tmp = tempdir().expect("tempdir");
        let data_dir = tmp.path().join("leanctx-data");
        let _env = EnvVarGuard::set(
            "LEAN_CTX_DATA_DIR",
            data_dir.to_str().expect("data dir utf8"),
        );

        let input = CreateLedgerInput {
            agent_id: Some("agent-1".to_string()),
            client_name: Some("cursor".to_string()),
            project_root: Some(tmp.path().to_string_lossy().to_string()),
            session: sample_session(),
            tool_calls: sample_tool_calls(),
            workflow: None,
            curated_refs: vec![("src/lib.rs".to_string(), "fn alpha() {}".to_string())],
        };

        let (ledger, path) = create_ledger(input).expect("create ledger");
        assert!(path.exists(), "ledger file should be created");
        assert_eq!(ledger.tool_calls.total, 2);
        assert_eq!(ledger.tool_calls.by_tool.get("ctx_read"), Some(&1));
        assert_eq!(ledger.tool_calls.by_ctx_read_mode.get("map"), Some(&1));
        assert_eq!(ledger.curated_refs.len(), 1);
        assert!(!ledger.content_md5.is_empty());

        let listed = list_ledgers();
        assert!(listed.iter().any(|p| p == &path));

        let loaded = load_ledger(&path).expect("load ledger");
        assert_eq!(loaded.content_md5, ledger.content_md5);
        assert_eq!(loaded.session.decisions.len(), 1);
        assert!(loaded.evidence_keys.iter().any(|k| k == "tool:ctx_read"));

        let removed = clear_ledgers().expect("clear ledgers");
        assert!(removed >= 1);
        assert!(list_ledgers().is_empty());
    }

    #[test]
    fn create_ledger_truncates_curated_refs_to_max() {
        let _env_lock = test_env_lock();
        let tmp = tempdir().expect("tempdir");
        let data_dir = tmp.path().join("leanctx-data");
        let _env = EnvVarGuard::set(
            "LEAN_CTX_DATA_DIR",
            data_dir.to_str().expect("data dir utf8"),
        );

        let curated_refs: Vec<(String, String)> = (0..(MAX_CURATED_REFS + 5))
            .map(|idx| (format!("src/file_{idx}.rs"), format!("fn f_{idx}() {{}}")))
            .collect();

        let input = CreateLedgerInput {
            agent_id: Some("agent-1".to_string()),
            client_name: Some("cursor".to_string()),
            project_root: Some(tmp.path().to_string_lossy().to_string()),
            session: sample_session(),
            tool_calls: sample_tool_calls(),
            workflow: None,
            curated_refs,
        };

        let (ledger, _) = create_ledger(input).expect("create ledger");
        assert_eq!(ledger.curated_refs.len(), MAX_CURATED_REFS);
    }

    #[test]
    fn canonicalization_and_content_md5_are_deterministic() {
        let value = serde_json::json!({"b": 1, "a": {"d": 2, "c": 3}});
        let canonical = canonicalize_json(&value);
        assert_eq!(canonical.to_string(), r#"{"a":{"c":3,"d":2},"b":1}"#);

        let mut ledger = make_minimal_ledger();
        ledger.content_md5 = "old-value".to_string();
        let md5_a = ledger_content_md5(&ledger);
        ledger.content_md5 = "new-value".to_string();
        let md5_b = ledger_content_md5(&ledger);
        assert_eq!(md5_a, md5_b);
    }
}
