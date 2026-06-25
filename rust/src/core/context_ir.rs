use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const STORE_FILENAME: &str = "context_ir_v1.json";

// Hard bounds: IR is an observability artifact; keep it small and safe.
const MAX_ITEMS: usize = 128;
const MAX_ITEM_CONTENT_CHARS: usize = 4096;
const MAX_TOTAL_CONTENT_CHARS: usize = 65_536;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextIrV1 {
    pub schema_version: u32,
    pub created_at: String,
    pub updated_at: String,
    pub next_seq: u64,
    pub totals: ContextIrTotalsV1,
    pub items: Vec<ContextIrItemV1>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextIrTotalsV1 {
    pub items_recorded: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tokens_saved: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextIrSourceKindV1 {
    Read,
    Shell,
    Search,
    Provider,
    #[default]
    Other,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextIrSourceV1 {
    pub kind: ContextIrSourceKindV1,
    pub tool: String,
    pub client_name: Option<String>,
    pub agent_id: Option<String>,
    pub path: Option<String>,
    pub command: Option<String>,
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextIrSafetyV1 {
    /// True if redaction has been applied to any stored text fields.
    pub redacted: bool,
    /// Human hint for the boundary mode at the time of collection, if known.
    pub boundary_mode: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextIrVerificationV1 {
    pub content_md5: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextIrItemV1 {
    pub seq: u64,
    pub created_at: String,
    pub source: ContextIrSourceV1,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub duration_us: u64,
    pub compression_ratio: f64,
    pub content_excerpt: String,
    pub truncated: bool,
    pub safety: ContextIrSafetyV1,
    pub verification: ContextIrVerificationV1,
}

#[derive(Debug, Clone)]
pub struct RecordIrInput<'a> {
    pub kind: ContextIrSourceKindV1,
    pub tool: &'a str,
    pub client_name: Option<String>,
    pub agent_id: Option<String>,
    pub path: Option<&'a str>,
    pub command: Option<&'a str>,
    pub pattern: Option<&'a str>,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub duration: std::time::Duration,
    pub content_excerpt: &'a str,
}

impl ContextIrV1 {
    #[must_use]
    pub fn new() -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            schema_version: crate::core::contracts::CONTEXT_IR_V1_SCHEMA_VERSION,
            created_at: now.clone(),
            updated_at: now,
            next_seq: 1,
            totals: ContextIrTotalsV1::default(),
            items: Vec::new(),
        }
    }

    pub fn record(&mut self, input: RecordIrInput<'_>) {
        let now = chrono::Utc::now().to_rfc3339();

        let (content_excerpt, truncated) = bound_and_redact_excerpt(input.content_excerpt);
        let command = input.command.map(crate::core::redaction::redact_text);
        let pattern = input.pattern.map(crate::core::redaction::redact_text);

        let ratio = if input.input_tokens == 0 {
            1.0
        } else {
            input.output_tokens as f64 / input.input_tokens as f64
        };

        let content_md5 = if content_excerpt.trim().is_empty() {
            None
        } else {
            Some(crate::core::hasher::hash_str(&content_excerpt))
        };

        let item = ContextIrItemV1 {
            seq: self.next_seq,
            created_at: now.clone(),
            source: ContextIrSourceV1 {
                kind: input.kind,
                tool: input.tool.to_string(),
                client_name: input.client_name,
                agent_id: input.agent_id,
                path: input.path.map(std::string::ToString::to_string),
                command,
                pattern,
            },
            input_tokens: input.input_tokens,
            output_tokens: input.output_tokens,
            duration_us: input.duration.as_micros() as u64,
            compression_ratio: ratio,
            content_excerpt,
            truncated,
            safety: ContextIrSafetyV1 {
                redacted: true,
                boundary_mode: Some(format!(
                    "{:?}",
                    crate::core::io_boundary::boundary_mode_effective(
                        &crate::core::roles::active_role()
                    )
                )),
            },
            verification: ContextIrVerificationV1 { content_md5 },
        };

        self.next_seq = self.next_seq.saturating_add(1);
        self.updated_at = now;

        self.totals.items_recorded = self.totals.items_recorded.saturating_add(1);
        self.totals.input_tokens = self
            .totals
            .input_tokens
            .saturating_add(item.input_tokens as u64);
        self.totals.output_tokens = self
            .totals
            .output_tokens
            .saturating_add(item.output_tokens as u64);
        self.totals.tokens_saved = self
            .totals
            .tokens_saved
            .saturating_add(item.input_tokens.saturating_sub(item.output_tokens) as u64);

        self.items.push(item);
        self.prune_in_place();
    }

    fn prune_in_place(&mut self) {
        while self.items.len() > MAX_ITEMS
            || total_content_chars(&self.items) > MAX_TOTAL_CONTENT_CHARS
        {
            if self.items.is_empty() {
                break;
            }
            self.items.remove(0);
        }
    }

    pub fn save(&self) {
        if let Ok(dir) = crate::core::paths::cache_dir() {
            let path = dir.join(STORE_FILENAME);
            if let Ok(json) = serde_json::to_string_pretty(self) {
                let json = crate::core::redaction::redact_text(&json);
                let _ = std::fs::write(path, json);
            }
        }
    }

    #[must_use]
    pub fn load() -> Self {
        crate::core::paths::cache_dir()
            .ok()
            .map(|d| d.join(STORE_FILENAME))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}

impl Default for ContextIrV1 {
    fn default() -> Self {
        Self::new()
    }
}

pub fn write_project_context_ir(
    project_root: &Path,
    ir: &ContextIrV1,
    filename: Option<&str>,
) -> Result<PathBuf, String> {
    let proofs_dir = crate::core::pathutil::safe_project_data_dir(project_root)?.join("proofs");
    std::fs::create_dir_all(&proofs_dir).map_err(|e| e.to_string())?;

    let ts = chrono::Utc::now().format("%Y-%m-%d_%H%M%S");
    let name = filename.map_or_else(
        || format!("context-ir-v1_{ts}.json"),
        std::string::ToString::to_string,
    );
    let path = proofs_dir.join(name);

    let json = serde_json::to_string_pretty(ir).map_err(|e| e.to_string())?;
    let json = crate::core::redaction::redact_text(&json);
    crate::config_io::write_atomic(&path, &json)?;
    Ok(path)
}

fn bound_and_redact_excerpt(s: &str) -> (String, bool) {
    let redacted = crate::core::redaction::redact_text(s);
    let mut out = redacted;
    let truncated = out.chars().count() > MAX_ITEM_CONTENT_CHARS;
    if truncated {
        out = out.chars().take(MAX_ITEM_CONTENT_CHARS).collect();
    }
    (out, truncated)
}

fn total_content_chars(items: &[ContextIrItemV1]) -> usize {
    items
        .iter()
        .map(|i| i.content_excerpt.chars().count())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_is_bounded() {
        let mut ir = ContextIrV1::new();
        let big = "x".repeat(MAX_ITEM_CONTENT_CHARS + 10);
        for _ in 0..(MAX_ITEMS + 10) {
            ir.record(RecordIrInput {
                kind: ContextIrSourceKindV1::Read,
                tool: "ctx_read",
                client_name: None,
                agent_id: None,
                path: Some("src/lib.rs"),
                command: None,
                pattern: None,
                input_tokens: 100,
                output_tokens: 10,
                duration: std::time::Duration::from_millis(1),
                content_excerpt: &big,
            });
        }
        assert!(ir.items.len() <= MAX_ITEMS);
        assert!(total_content_chars(&ir.items) <= MAX_TOTAL_CONTENT_CHARS);
    }

    #[test]
    fn excerpt_is_truncated() {
        let (s, truncated) = bound_and_redact_excerpt(&"x".repeat(MAX_ITEM_CONTENT_CHARS + 1));
        assert!(truncated);
        assert!(s.chars().count() <= MAX_ITEM_CONTENT_CHARS);
    }
}
