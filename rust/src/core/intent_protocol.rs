use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentSource {
    Inferred,
    Explicit,
}

impl IntentSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Inferred => "inferred",
            Self::Explicit => "explicit",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentType {
    Task,
    Execute,
    WorkflowTransition,
    KnowledgeFact,
    KnowledgeRecall,
    Setup,
    Unknown,
}

impl IntentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Execute => "execute",
            Self::WorkflowTransition => "workflow_transition",
            Self::KnowledgeFact => "knowledge_fact",
            Self::KnowledgeRecall => "knowledge_recall",
            Self::Setup => "setup",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntentSubject {
    Project {
        root: Option<String>,
    },
    Command {
        command: String,
    },
    Workflow {
        action: String,
    },
    KnowledgeFact {
        category: String,
        key: String,
        value: String,
    },
    KnowledgeQuery {
        category: Option<String>,
        query: Option<String>,
    },
    Tool {
        name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentRecord {
    pub id: String,
    pub source: IntentSource,
    pub intent_type: IntentType,
    pub subject: IntentSubject,
    pub assertion: String,
    pub confidence: f32,
    #[serde(default)]
    pub evidence_keys: Vec<String>,
    #[serde(default)]
    pub occurrences: u32,
    pub timestamp: DateTime<Utc>,
}

impl IntentRecord {
    pub fn fingerprint(&self) -> (IntentSource, IntentType, String, String) {
        (
            self.source.clone(),
            self.intent_type.clone(),
            format!("{:?}", self.subject),
            self.assertion.clone(),
        )
    }
}

pub fn infer_from_tool_call(
    tool: &str,
    action: Option<&str>,
    args: &serde_json::Map<String, Value>,
    project_root: Option<&str>,
) -> Option<IntentRecord> {
    match tool {
        "ctx_execute" => {
            let cmd = get_str(args, "command")
                .unwrap_or_default()
                .trim()
                .to_string();
            if cmd.is_empty() {
                return None;
            }
            Some(IntentRecord {
                id: stable_id(tool, action, &cmd),
                source: IntentSource::Inferred,
                intent_type: IntentType::Execute,
                subject: IntentSubject::Command {
                    command: cmd.clone(),
                },
                assertion: truncate_one_line(&cmd, 180),
                confidence: 0.9,
                evidence_keys: evidence_keys_for(tool, action),
                occurrences: 1,
                timestamp: Utc::now(),
            })
        }
        "ctx_workflow" => {
            let a = action
                .or_else(|| get_str(args, "action"))
                .unwrap_or("unknown");
            Some(IntentRecord {
                id: stable_id(tool, Some(a), a),
                source: IntentSource::Inferred,
                intent_type: IntentType::WorkflowTransition,
                subject: IntentSubject::Workflow {
                    action: a.to_string(),
                },
                assertion: truncate_one_line(a, 180),
                confidence: 0.75,
                evidence_keys: evidence_keys_for(tool, Some(a)),
                occurrences: 1,
                timestamp: Utc::now(),
            })
        }
        "ctx_knowledge" => {
            let a = action
                .or_else(|| get_str(args, "action"))
                .unwrap_or("unknown");
            match a {
                "remember" => {
                    let category = get_str(args, "category")?.to_string();
                    let key = get_str(args, "key")?.to_string();
                    let value = get_str(args, "value")?.to_string();
                    Some(IntentRecord {
                        id: stable_id(tool, Some(a), &format!("{category}/{key}")),
                        source: IntentSource::Inferred,
                        intent_type: IntentType::KnowledgeFact,
                        subject: IntentSubject::KnowledgeFact {
                            category: category.clone(),
                            key: key.clone(),
                            value: value.clone(),
                        },
                        assertion: truncate_one_line(&format!("{category}:{key}={value}"), 180),
                        confidence: 0.9,
                        evidence_keys: evidence_keys_for(tool, Some(a)),
                        occurrences: 1,
                        timestamp: Utc::now(),
                    })
                }
                "recall" => Some(IntentRecord {
                    id: stable_id(tool, Some(a), get_str(args, "query").unwrap_or("")),
                    source: IntentSource::Inferred,
                    intent_type: IntentType::KnowledgeRecall,
                    subject: IntentSubject::KnowledgeQuery {
                        category: get_str(args, "category").map(std::string::ToString::to_string),
                        query: get_str(args, "query").map(std::string::ToString::to_string),
                    },
                    assertion: truncate_one_line(get_str(args, "query").unwrap_or(""), 180),
                    confidence: 0.7,
                    evidence_keys: evidence_keys_for(tool, Some(a)),
                    occurrences: 1,
                    timestamp: Utc::now(),
                }),
                _ => None,
            }
        }
        "ctx_intent" => {
            let query = get_str(args, "query").unwrap_or_default();
            Some(intent_from_query(query, project_root))
        }
        "ctx_session" => {
            let a = action
                .or_else(|| get_str(args, "action"))
                .unwrap_or("unknown");
            if a != "task" {
                return None;
            }
            let v = get_str(args, "value").unwrap_or("").trim().to_string();
            if v.is_empty() {
                return None;
            }
            Some(IntentRecord {
                id: stable_id(tool, Some(a), &v),
                source: IntentSource::Inferred,
                intent_type: IntentType::Task,
                subject: IntentSubject::Project {
                    root: project_root.map(std::string::ToString::to_string),
                },
                assertion: truncate_one_line(&v, 220),
                confidence: 0.8,
                evidence_keys: evidence_keys_for(tool, Some(a)),
                occurrences: 1,
                timestamp: Utc::now(),
            })
        }
        "setup" | "doctor" | "bootstrap" => Some(IntentRecord {
            id: stable_id(tool, action, tool),
            source: IntentSource::Inferred,
            intent_type: IntentType::Setup,
            subject: IntentSubject::Tool {
                name: tool.to_string(),
            },
            assertion: tool.to_string(),
            confidence: 0.8,
            evidence_keys: evidence_keys_for(tool, action),
            occurrences: 1,
            timestamp: Utc::now(),
        }),
        _ => None,
    }
}

pub fn intent_from_query(query: &str, project_root: Option<&str>) -> IntentRecord {
    let now = Utc::now();
    let q = query.trim();
    if let Ok(v) = serde_json::from_str::<Value>(q) {
        if let Some(obj) = v.as_object() {
            if let Some(intent_type) = obj.get("intent_type").and_then(|v| v.as_str()) {
                if let Some(intent) = intent_from_json(intent_type, obj, project_root, now) {
                    return intent;
                }
            }
        }
    }

    // Plain text → deterministic intent classification (short, no reads).
    let multi = crate::core::intent_engine::detect_multi_intent(q);
    let primary = multi.first();
    let (intent_type, confidence) = if let Some(p) = primary {
        (
            IntentType::Task,
            (p.confidence as f32).clamp(0.0, 1.0).max(0.6),
        )
    } else {
        (IntentType::Task, 0.6)
    };

    let assertion = truncate_one_line(q, 220);
    IntentRecord {
        id: stable_id("ctx_intent", Some("query"), &assertion),
        source: IntentSource::Explicit,
        intent_type,
        subject: IntentSubject::Project {
            root: project_root.map(std::string::ToString::to_string),
        },
        assertion,
        confidence,
        evidence_keys: evidence_keys_for("ctx_intent", Some("query")),
        occurrences: 1,
        timestamp: now,
    }
}

pub fn apply_side_effects(
    intent: &IntentRecord,
    project_root: Option<&str>,
    session_id: &str,
) -> Result<(), String> {
    let Some(root) = project_root else {
        return Ok(());
    };

    let IntentSubject::KnowledgeFact {
        category,
        key,
        value,
    } = &intent.subject
    else {
        return Ok(());
    };

    let policy = crate::core::config::Config::load()
        .memory_policy_effective()
        .map_err(|e| format!("invalid memory policy: {e}"))?;
    let mut knowledge = crate::core::knowledge::ProjectKnowledge::load(root)
        .unwrap_or_else(|| crate::core::knowledge::ProjectKnowledge::new(root));
    let _ = knowledge.remember(
        category,
        key,
        value,
        session_id,
        intent.confidence.clamp(0.0, 1.0),
        &policy,
    );
    let _ = knowledge.run_memory_lifecycle(&policy);
    let _ = knowledge.save();
    Ok(())
}

fn intent_from_json(
    intent_type: &str,
    obj: &serde_json::Map<String, Value>,
    project_root: Option<&str>,
    now: DateTime<Utc>,
) -> Option<IntentRecord> {
    match intent_type {
        "knowledge_fact" => {
            let category = obj.get("category")?.as_str()?.to_string();
            let key = obj.get("key")?.as_str()?.to_string();
            let value = obj.get("value")?.as_str()?.to_string();
            let assertion = truncate_one_line(&format!("{category}:{key}={value}"), 220);
            Some(IntentRecord {
                id: stable_id(
                    "ctx_intent",
                    Some("knowledge_fact"),
                    &format!("{category}/{key}"),
                ),
                source: IntentSource::Explicit,
                intent_type: IntentType::KnowledgeFact,
                subject: IntentSubject::KnowledgeFact {
                    category,
                    key,
                    value,
                },
                assertion,
                confidence: obj
                    .get("confidence")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.8)
                    .clamp(0.0, 1.0) as f32,
                evidence_keys: evidence_keys_for("ctx_intent", Some("knowledge_fact")),
                occurrences: 1,
                timestamp: now,
            })
        }
        "task" => {
            let assertion = obj
                .get("assertion")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if assertion.trim().is_empty() {
                return None;
            }
            Some(IntentRecord {
                id: stable_id("ctx_intent", Some("task"), &assertion),
                source: IntentSource::Explicit,
                intent_type: IntentType::Task,
                subject: IntentSubject::Project {
                    root: project_root.map(std::string::ToString::to_string),
                },
                assertion: truncate_one_line(&assertion, 220),
                confidence: obj
                    .get("confidence")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.75)
                    .clamp(0.0, 1.0) as f32,
                evidence_keys: evidence_keys_for("ctx_intent", Some("task")),
                occurrences: 1,
                timestamp: now,
            })
        }
        _ => None,
    }
}

fn evidence_keys_for(tool: &str, action: Option<&str>) -> Vec<String> {
    let mut keys = vec![format!("tool:{tool}")];
    if let Some(a) = action {
        if !a.is_empty() {
            keys.push(format!("tool:{tool}:{a}"));
        }
    }
    keys
}

fn stable_id(tool: &str, action: Option<&str>, seed: &str) -> String {
    let mut hasher = DefaultHasher::new();
    tool.hash(&mut hasher);
    action.unwrap_or("").hash(&mut hasher);
    seed.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn get_str<'a>(m: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    m.get(key).and_then(|v| v.as_str())
}

fn truncate_one_line(s: &str, max: usize) -> String {
    let mut t = s.replace(['\n', '\r'], " ").replace('`', "");
    while t.contains("  ") {
        t = t.replace("  ", " ");
    }
    let t = t.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    let mut out = String::new();
    for (i, ch) in t.chars().enumerate() {
        if i + 1 >= max {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_execute() {
        let mut args = serde_json::Map::new();
        args.insert(
            "command".to_string(),
            Value::String("cargo test".to_string()),
        );
        let i = infer_from_tool_call("ctx_execute", None, &args, Some(".")).expect("intent");
        assert_eq!(i.intent_type, IntentType::Execute);
    }
}
