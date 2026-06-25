use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_ITEMS: usize = 500;
const MAX_KEY_CHARS: usize = 128;
const MAX_VALUE_EXCERPT_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceItemKindV1 {
    ToolReceipt,
    Manual,
    ProofArtifact,
    CiReceipt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceItemV1 {
    pub id: String,
    pub kind: EvidenceItemKindV1,
    pub key: String,
    #[serde(default)]
    pub value_md5: Option<String>,
    #[serde(default)]
    pub value_excerpt: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub input_md5: Option<String>,
    #[serde(default)]
    pub output_md5: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub client_name: Option<String>,
    /// For referenced evidence payloads, store the **basename only** to avoid leaking full paths.
    #[serde(default)]
    pub artifact_name: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceLedgerV1 {
    pub schema_version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub items: Vec<EvidenceItemV1>,
}

impl Default for EvidenceLedgerV1 {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            schema_version: crate::core::contracts::WORKFLOW_EVIDENCE_LEDGER_V1_SCHEMA_VERSION,
            created_at: now,
            updated_at: now,
            items: Vec::new(),
        }
    }
}

fn ledger_path() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("workflows").join("evidence-ledger-v1.json"))
}

impl EvidenceLedgerV1 {
    #[must_use]
    pub fn load() -> Self {
        let Some(path) = ledger_path() else {
            return Self::default();
        };
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        serde_json::from_str::<Self>(&content).unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let Some(path) = ledger_path() else {
            return Err("no data dir".to_string());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic_with_backup(&path, &json)?;
        Ok(())
    }

    #[must_use]
    pub fn has_key(&self, key: &str) -> bool {
        self.items.iter().any(|i| i.key == key)
    }

    pub fn record_tool_receipt(
        &mut self,
        tool: &str,
        action: Option<&str>,
        input_md5: &str,
        output_md5: &str,
        agent_id: Option<&str>,
        client_name: Option<&str>,
        created_at: DateTime<Utc>,
    ) {
        let r = ToolReceiptRecord {
            tool,
            action,
            input_md5,
            output_md5,
            agent_id,
            client_name,
            ts: created_at,
        };
        self.record_tool_receipt_key(&format!("tool:{tool}"), &r);
        if let Some(a) = action.filter(|a| !a.trim().is_empty()) {
            let r = ToolReceiptRecord {
                action: Some(a),
                ..r
            };
            self.record_tool_receipt_key(&format!("tool:{tool}:{a}"), &r);
        }
        self.prune_in_place();
    }

    fn record_tool_receipt_key(&mut self, key: &str, r: &ToolReceiptRecord<'_>) {
        let key = truncate(key, MAX_KEY_CHARS);
        let id = crate::core::hasher::hash_str(&format!(
            "tool_receipt|{key}|{}|{}|{}|{}",
            r.tool,
            r.action.unwrap_or(""),
            r.input_md5,
            r.output_md5
        ));
        self.upsert_item(EvidenceItemV1 {
            id,
            kind: EvidenceItemKindV1::ToolReceipt,
            key,
            value_md5: None,
            value_excerpt: None,
            tool: Some(r.tool.to_string()),
            action: r.action.map(std::string::ToString::to_string),
            input_md5: Some(r.input_md5.to_string()),
            output_md5: Some(r.output_md5.to_string()),
            agent_id: r.agent_id.map(std::string::ToString::to_string),
            client_name: r.client_name.map(std::string::ToString::to_string),
            artifact_name: None,
            timestamp: r.ts,
        });
    }

    pub fn record_manual(&mut self, key: &str, value: Option<&str>, created_at: DateTime<Utc>) {
        let key = truncate(key, MAX_KEY_CHARS);
        let value_redacted = value.map(crate::core::redaction::redact_text);
        let value_md5 = value_redacted.as_deref().map(crate::core::hasher::hash_str);
        let value_excerpt = value_redacted
            .as_deref()
            .map(|v| truncate(v, MAX_VALUE_EXCERPT_CHARS));
        let id = crate::core::hasher::hash_str(&format!(
            "manual|{key}|{}",
            value_md5.as_deref().unwrap_or("")
        ));
        self.upsert_item(EvidenceItemV1 {
            id,
            kind: EvidenceItemKindV1::Manual,
            key,
            value_md5,
            value_excerpt,
            tool: None,
            action: None,
            input_md5: None,
            output_md5: None,
            agent_id: None,
            client_name: None,
            artifact_name: None,
            timestamp: created_at,
        });
        self.prune_in_place();
    }

    pub fn record_artifact_file(
        &mut self,
        key: &str,
        path: &Path,
        created_at: DateTime<Utc>,
    ) -> Result<(), String> {
        let key = truncate(key, MAX_KEY_CHARS);
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let value_md5 = Some(crate::core::hasher::hash_hex(&bytes));
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let id = crate::core::hasher::hash_str(&format!(
            "artifact|{key}|{}|{}",
            name,
            value_md5.as_deref().unwrap_or("")
        ));
        self.upsert_item(EvidenceItemV1 {
            id,
            kind: EvidenceItemKindV1::ProofArtifact,
            key,
            value_md5,
            value_excerpt: None,
            tool: None,
            action: None,
            input_md5: None,
            output_md5: None,
            agent_id: None,
            client_name: None,
            artifact_name: Some(name),
            timestamp: created_at,
        });
        self.prune_in_place();
        Ok(())
    }

    fn upsert_item(&mut self, item: EvidenceItemV1) {
        if let Some(existing) = self.items.iter_mut().find(|i| i.id == item.id) {
            existing.timestamp = item.timestamp;
            existing.key = item.key;
            existing.value_md5 = item.value_md5;
            existing.value_excerpt = item.value_excerpt;
            existing.tool = item.tool;
            existing.action = item.action;
            existing.input_md5 = item.input_md5;
            existing.output_md5 = item.output_md5;
            existing.agent_id = item.agent_id;
            existing.client_name = item.client_name;
            existing.artifact_name = item.artifact_name;
            self.updated_at = Utc::now();
            return;
        }
        self.items.push(item);
        self.updated_at = Utc::now();
    }

    fn prune_in_place(&mut self) {
        if self.items.len() <= MAX_ITEMS {
            return;
        }
        self.items.sort_by_key(|i| i.timestamp.timestamp_millis());
        while self.items.len() > MAX_ITEMS {
            self.items.remove(0);
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let mut out = s[..max].to_string();
    out.push('…');
    out
}

#[derive(Clone, Copy)]
struct ToolReceiptRecord<'a> {
    tool: &'a str,
    action: Option<&'a str>,
    input_md5: &'a str,
    output_md5: &'a str,
    agent_id: Option<&'a str>,
    client_name: Option<&'a str>,
    ts: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_receipt_id_is_deterministic() {
        let ts = Utc::now();
        let mut a = EvidenceLedgerV1::default();
        a.record_tool_receipt(
            "ctx_read",
            Some("full"),
            "in",
            "out",
            Some("agent"),
            Some("cursor"),
            ts,
        );
        let id1 = a.items[0].id.clone();

        let mut b = EvidenceLedgerV1::default();
        b.record_tool_receipt(
            "ctx_read",
            Some("full"),
            "in",
            "out",
            Some("agent"),
            Some("cursor"),
            ts,
        );
        let id2 = b.items[0].id.clone();
        assert_eq!(id1, id2);
    }

    #[test]
    fn prunes_to_max_items() {
        let ts = Utc::now();
        let mut l = EvidenceLedgerV1::default();
        for i in 0..(MAX_ITEMS + 50) {
            l.record_manual(&format!("k{i}"), None, ts);
        }
        assert!(l.items.len() <= MAX_ITEMS);
    }
}
