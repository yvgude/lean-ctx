//! Import facts from local AI coding-session histories.

pub mod claude_code;
pub mod codex;
pub mod cursor;
pub mod opencode;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::Value;

use crate::core::knowledge::KnowledgeFact;

const MAX_FACT_VALUE_CHARS: usize = 500;
const MAX_ERRORS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSource {
    ClaudeCode,
    Codex,
    Cursor,
    OpenCode,
}

impl ImportSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImportResult {
    pub sessions_found: usize,
    pub facts_extracted: usize,
    pub files_touched: usize,
    pub errors: Vec<String>,
    /// Facts remain available to the CLI until it either persists or displays them.
    pub facts: Vec<KnowledgeFact>,
}

impl ImportResult {
    pub(crate) fn merge(&mut self, mut other: Self) {
        self.sessions_found += other.sessions_found;
        self.facts_extracted += other.facts_extracted;
        self.files_touched += other.files_touched;
        self.errors.append(&mut other.errors);
        self.facts.append(&mut other.facts);
    }
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub(crate) fn read_jsonl_files(source: ImportSource, files: Vec<PathBuf>) -> ImportResult {
    let mut result = ImportResult {
        sessions_found: files.len(),
        ..ImportResult::default()
    };
    let mut touched = HashSet::new();
    let mut seen = HashSet::new();

    for path in files {
        let session = session_id(source, &path);
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                for (line_number, line) in content.lines().enumerate() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Value>(line) {
                        Ok(value) => process_value(
                            source,
                            &session,
                            &value,
                            &mut result,
                            &mut touched,
                            &mut seen,
                        ),
                        Err(error) => push_error(
                            &mut result,
                            format!(
                                "{}:{}: invalid JSONL: {error}",
                                path.display(),
                                line_number + 1
                            ),
                        ),
                    }
                }
            }
            Err(error) => push_error(&mut result, format!("{}: {error}", path.display())),
        }
    }

    result.files_touched = touched.len();
    result.facts_extracted = result.facts.len();
    result
}

pub(crate) fn read_json_file(source: ImportSource, path: &Path) -> ImportResult {
    let mut result = ImportResult {
        sessions_found: 1,
        ..ImportResult::default()
    };
    let mut touched = HashSet::new();
    let mut seen = HashSet::new();
    let session = session_id(source, path);

    match std::fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str::<Value>(&content) {
            Ok(value) => process_value(
                source,
                &session,
                &value,
                &mut result,
                &mut touched,
                &mut seen,
            ),
            Err(error) => push_error(
                &mut result,
                format!("{}: invalid JSON: {error}", path.display()),
            ),
        },
        Err(error) => push_error(&mut result, format!("{}: {error}", path.display())),
    }

    result.files_touched = touched.len();
    result.facts_extracted = result.facts.len();
    result
}

pub(crate) fn process_value(
    source: ImportSource,
    session: &str,
    value: &Value,
    result: &mut ImportResult,
    touched: &mut HashSet<String>,
    seen: &mut HashSet<(String, String)>,
) {
    let mut paths = Vec::new();
    collect_file_paths(value, &mut paths);
    for path in paths {
        if touched.insert(path.clone()) {
            push_fact(
                result,
                seen,
                source,
                session,
                "imported-observation",
                &format!("Touched file: {path}"),
                0.6,
            );
        }
    }

    let mut strings = Vec::new();
    collect_strings(value, &mut strings);
    let assistant = contains_assistant_message(value);
    for text in strings {
        if assistant {
            for decision in matching_snippets(&text, is_decision) {
                push_fact(
                    result,
                    seen,
                    source,
                    session,
                    "imported-decision",
                    &decision,
                    0.65,
                );
            }
        }
        for error in matching_snippets(&text, is_error) {
            push_fact(
                result,
                seen,
                source,
                session,
                "imported-observation",
                &format!("Observed error: {error}"),
                0.55,
            );
        }
    }
}

pub(crate) fn finish_result(result: &mut ImportResult, touched: &HashSet<String>) {
    result.files_touched = touched.len();
    result.facts_extracted = result.facts.len();
}

pub(crate) fn push_error(result: &mut ImportResult, error: String) {
    if result.errors.len() < MAX_ERRORS {
        result.errors.push(error);
    }
}

pub(crate) fn session_id(source: ImportSource, path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-session");
    format!("{}:{name}", source.as_str())
}

fn push_fact(
    result: &mut ImportResult,
    seen: &mut HashSet<(String, String)>,
    source: ImportSource,
    session: &str,
    category: &str,
    value: &str,
    confidence: f32,
) {
    let value = truncate(value);
    if value.is_empty() || !seen.insert((category.to_owned(), value.clone())) {
        return;
    }

    let now = Utc::now();
    result.facts.push(KnowledgeFact {
        category: category.to_owned(),
        key: String::new(),
        value,
        source_session: session.to_owned(),
        confidence,
        created_at: now,
        last_confirmed: now,
        retrieval_count: 0,
        last_retrieved: None,
        valid_from: None,
        valid_until: None,
        supersedes: None,
        confirmation_count: 0,
        feedback_up: 0,
        feedback_down: 0,
        last_feedback: None,
        privacy: Default::default(),
        sensitivity: Default::default(),
        imported_from: Some(source.as_str().to_owned()),
        archetype: Default::default(),
        fidelity: None,
        revision_count: 0,
    });
}

fn collect_file_paths(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_file_paths(value, out);
            }
        }
        Value::Object(map) => {
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "file_path" | "filePath" | "path" | "filename" | "file_name"
                ) {
                    if let Some(path) = value
                        .as_str()
                        .map(str::trim)
                        .filter(|path| !path.is_empty())
                    {
                        out.push(path.to_owned());
                    }
                }
                collect_file_paths(value, out);
            }
        }
        _ => {}
    }
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.to_owned()),
        Value::Array(values) => {
            for value in values {
                collect_strings(value, out);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_strings(value, out);
            }
        }
        _ => {}
    }
}

fn contains_assistant_message(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_assistant_message),
        Value::Object(map) => {
            map.get("role").and_then(Value::as_str) == Some("assistant")
                || matches!(
                    map.get("type").and_then(Value::as_str),
                    Some("assistant" | "assistant_message")
                )
                || map.values().any(contains_assistant_message)
        }
        _ => false,
    }
}

fn matching_snippets(text: &str, predicate: fn(&str) -> bool) -> Vec<String> {
    text.split(['\n', '.', '!', '?'])
        .map(str::trim)
        .filter(|snippet| snippet.len() >= 4 && predicate(snippet))
        .map(str::to_owned)
        .collect()
}

fn is_decision(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "decision",
        "decided",
        "we chose",
        "i chose",
        "chosen",
        "will use",
        "we'll use",
        "we will use",
        "going with",
        "approach is",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn is_error(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "error",
        "failed",
        "failure",
        "panic",
        "unable to",
        "cannot ",
        "can't ",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn truncate(value: &str) -> String {
    // The first char boundary *at or past* the cap can land one byte over it:
    // with 2-byte characters starting at an odd offset the boundaries are
    // 11, 13, … 499, 501, so a 500-byte cap yielded a 501-byte slice. Take the
    // last boundary at or below the cap instead — the same idiom already used
    // in `ctx_search` and `ctx_preload`.
    value[..value.floor_char_boundary(MAX_FACT_VALUE_CHARS)]
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_multibyte_facts_without_panicking() {
        let value = format!("We decided {}", "é".repeat(300));
        let truncated = truncate(&value);
        assert!(truncated.len() <= MAX_FACT_VALUE_CHARS);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn extracts_touches_decisions_and_errors() {
        let value: Value = serde_json::json!({
            "role": "assistant",
            "content": "We decided to use a cache. Build failed because input is invalid.",
            "tool_input": { "file_path": "rust/src/lib.rs" }
        });
        let mut result = ImportResult::default();
        let mut touched = HashSet::new();
        let mut seen = HashSet::new();

        process_value(
            ImportSource::Codex,
            "codex:test",
            &value,
            &mut result,
            &mut touched,
            &mut seen,
        );
        finish_result(&mut result, &touched);

        assert_eq!(result.files_touched, 1);
        assert!(
            result
                .facts
                .iter()
                .any(|fact| fact.category == "imported-decision")
        );
        assert!(
            result
                .facts
                .iter()
                .any(|fact| fact.value.starts_with("Observed error:"))
        );
    }
}
