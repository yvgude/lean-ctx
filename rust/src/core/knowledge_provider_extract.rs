//! Knowledge auto-extraction from provider data (issues, PRs, DB schemas).
//!
//! Converts external `ContentChunk`s into knowledge facts that flow into the
//! `ProjectKnowledge` system. This implements the "Sleep Replay" pattern from
//! hippocampal consolidation: raw episodic data is transformed into semantic
//! long-term knowledge.
//!
//! Extraction rules:
//!   - Issues with labels → `known_bugs`, `known_features`, `known_issues`
//!   - PRs → `recent_changes`, `active_branches`
//!   - DB schemas → `data_model` facts
//!   - Wiki pages → `documentation` facts

use crate::core::content_chunk::ContentChunk;

/// A knowledge fact extracted from provider data, ready for `ProjectKnowledge.remember()`.
#[derive(Debug, Clone)]
pub struct ExtractedFact {
    pub category: String,
    pub key: String,
    pub value: String,
    pub confidence: f32,
}

/// Extract knowledge facts from a set of `ContentChunks`.
#[must_use]
pub fn extract_facts(chunks: &[ContentChunk]) -> Vec<ExtractedFact> {
    let mut facts = Vec::new();

    for chunk in chunks {
        if !chunk.is_external() {
            continue;
        }

        let provider = chunk.provider_id().unwrap_or("unknown");
        match chunk.kind {
            crate::core::chunk_data::ChunkKind::Issue
            | crate::core::chunk_data::ChunkKind::Ticket => {
                extract_issue_facts(chunk, provider, &mut facts);
            }
            crate::core::chunk_data::ChunkKind::PullRequest => {
                extract_pr_facts(chunk, provider, &mut facts);
            }
            crate::core::chunk_data::ChunkKind::WikiPage => {
                extract_wiki_facts(chunk, provider, &mut facts);
            }
            crate::core::chunk_data::ChunkKind::DbSchema => {
                extract_db_facts(chunk, provider, &mut facts);
            }
            _ => {}
        }
    }

    facts
}

fn extract_issue_facts(chunk: &ContentChunk, provider: &str, facts: &mut Vec<ExtractedFact>) {
    let state = chunk
        .metadata
        .as_ref()
        .and_then(|m| m["state"].as_str())
        .unwrap_or("unknown");

    let labels: Vec<&str> = chunk
        .metadata
        .as_ref()
        .and_then(|m| m["labels"].as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let category = if labels.iter().any(|l| {
        let lower = l.to_lowercase();
        lower.contains("bug") || lower.contains("defect")
    }) {
        "known_bugs"
    } else if labels.iter().any(|l| {
        let lower = l.to_lowercase();
        lower.contains("feature") || lower.contains("enhancement")
    }) {
        "known_features"
    } else {
        "known_issues"
    };

    let issue_id = chunk
        .file_path
        .rsplit('/')
        .next()
        .unwrap_or(&chunk.file_path);

    facts.push(ExtractedFact {
        category: category.to_string(),
        key: format!("{provider}#{issue_id}"),
        value: format!("{} [{}]", chunk.symbol_name, state),
        confidence: if state == "open" { 0.9 } else { 0.7 },
    });

    for ref_path in &chunk.references {
        facts.push(ExtractedFact {
            category: "file_mentions".to_string(),
            key: ref_path.clone(),
            value: format!(
                "Referenced in {} {provider}#{issue_id}: {}",
                category, chunk.symbol_name
            ),
            confidence: 0.85,
        });
    }
}

fn extract_pr_facts(chunk: &ContentChunk, provider: &str, facts: &mut Vec<ExtractedFact>) {
    let state = chunk
        .metadata
        .as_ref()
        .and_then(|m| m["state"].as_str())
        .unwrap_or("unknown");

    let pr_id = chunk
        .file_path
        .rsplit('/')
        .next()
        .unwrap_or(&chunk.file_path);

    facts.push(ExtractedFact {
        category: "recent_changes".to_string(),
        key: format!("{provider}#PR{pr_id}"),
        value: format!("{} [{}]", chunk.symbol_name, state),
        confidence: if state == "open" { 0.95 } else { 0.8 },
    });

    for ref_path in &chunk.references {
        facts.push(ExtractedFact {
            category: "changed_files".to_string(),
            key: ref_path.clone(),
            value: format!("Changed in PR {provider}#{pr_id}: {}", chunk.symbol_name),
            confidence: 0.9,
        });
    }
}

fn extract_wiki_facts(chunk: &ContentChunk, provider: &str, facts: &mut Vec<ExtractedFact>) {
    let page_id = chunk
        .file_path
        .rsplit('/')
        .next()
        .unwrap_or(&chunk.file_path);

    facts.push(ExtractedFact {
        category: "documentation".to_string(),
        key: format!("{provider}#{page_id}"),
        value: chunk.symbol_name.clone(),
        confidence: 0.85,
    });

    for ref_path in &chunk.references {
        facts.push(ExtractedFact {
            category: "documented_files".to_string(),
            key: ref_path.clone(),
            value: format!("Documented in {provider}#{page_id}: {}", chunk.symbol_name),
            confidence: 0.8,
        });
    }
}

fn extract_db_facts(chunk: &ContentChunk, provider: &str, facts: &mut Vec<ExtractedFact>) {
    let table_id = chunk
        .file_path
        .rsplit('/')
        .next()
        .unwrap_or(&chunk.file_path);

    facts.push(ExtractedFact {
        category: "data_model".to_string(),
        key: format!("{provider}#{table_id}"),
        value: chunk.symbol_name.clone(),
        confidence: 0.95,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::chunk_data::ChunkKind;
    use crate::core::content_chunk::ContentChunk;

    fn issue_with_labels(id: &str, title: &str, labels: &[&str], refs: Vec<&str>) -> ContentChunk {
        ContentChunk::from_provider(
            "github",
            "issues",
            id,
            title,
            ChunkKind::Issue,
            format!("Body of {title}"),
            refs.into_iter().map(String::from).collect(),
            Some(serde_json::json!({
                "state": "open",
                "labels": labels,
            })),
        )
    }

    #[test]
    fn bug_label_creates_known_bugs_fact() {
        let chunk = issue_with_labels("42", "Auth crash", &["bug", "p1"], vec!["src/auth.rs"]);
        let facts = extract_facts(&[chunk]);

        let bug_fact = facts.iter().find(|f| f.category == "known_bugs");
        assert!(bug_fact.is_some());
        assert!(bug_fact.unwrap().key.contains("42"));
        assert!(bug_fact.unwrap().value.contains("Auth crash"));
        assert!(bug_fact.unwrap().value.contains("[open]"));
    }

    #[test]
    fn feature_label_creates_known_features_fact() {
        let chunk = issue_with_labels("10", "Dark mode", &["enhancement"], vec![]);
        let facts = extract_facts(&[chunk]);

        assert!(facts.iter().any(|f| f.category == "known_features"));
    }

    #[test]
    fn generic_issue_creates_known_issues_fact() {
        let chunk = issue_with_labels("5", "Question about API", &["question"], vec![]);
        let facts = extract_facts(&[chunk]);

        assert!(facts.iter().any(|f| f.category == "known_issues"));
    }

    #[test]
    fn issue_with_refs_creates_file_mentions() {
        let chunk = issue_with_labels(
            "42",
            "Auth crash",
            &["bug"],
            vec!["src/auth.rs", "src/db.rs"],
        );
        let facts = extract_facts(&[chunk]);

        let mentions: Vec<_> = facts
            .iter()
            .filter(|f| f.category == "file_mentions")
            .collect();
        assert_eq!(mentions.len(), 2);
        assert!(mentions.iter().any(|f| f.key == "src/auth.rs"));
        assert!(mentions.iter().any(|f| f.key == "src/db.rs"));
    }

    #[test]
    fn pr_creates_recent_changes_and_changed_files() {
        let chunk = ContentChunk::from_provider(
            "github",
            "pull_requests",
            "100",
            "Fix auth token expiry",
            ChunkKind::PullRequest,
            "Fixes token expiry".into(),
            vec!["src/auth.rs".into()],
            Some(serde_json::json!({"state": "open"})),
        );

        let facts = extract_facts(&[chunk]);
        assert!(facts.iter().any(|f| f.category == "recent_changes"));
        assert!(
            facts
                .iter()
                .any(|f| f.category == "changed_files" && f.key == "src/auth.rs")
        );
    }

    #[test]
    fn wiki_creates_documentation_facts() {
        let chunk = ContentChunk::from_provider(
            "confluence",
            "wikis",
            "auth-guide",
            "Authentication Guide",
            ChunkKind::WikiPage,
            "How auth works".into(),
            vec!["src/auth/mod.rs".into()],
            None,
        );

        let facts = extract_facts(&[chunk]);
        assert!(facts.iter().any(|f| f.category == "documentation"));
        assert!(facts.iter().any(|f| f.category == "documented_files"));
    }

    #[test]
    fn db_creates_data_model_facts() {
        let chunk = ContentChunk::from_provider(
            "postgres",
            "schemas",
            "users",
            "public.users",
            ChunkKind::DbSchema,
            "CREATE TABLE users (id serial, email varchar)".into(),
            vec![],
            None,
        );

        let facts = extract_facts(&[chunk]);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].category, "data_model");
        assert_eq!(facts[0].confidence, 0.95);
    }

    #[test]
    fn code_chunks_are_skipped() {
        let chunk = ContentChunk::from(crate::core::chunk_data::CodeChunk {
            file_path: "src/main.rs".into(),
            symbol_name: "main".into(),
            kind: ChunkKind::Function,
            start_line: 1,
            end_line: 5,
            content: "fn main() {}".into(),
            tokens: vec![],
            token_count: 0,
        });

        let facts = extract_facts(&[chunk]);
        assert!(facts.is_empty());
    }

    #[test]
    fn closed_issues_have_lower_confidence() {
        let chunk = ContentChunk::from_provider(
            "github",
            "issues",
            "99",
            "Old bug",
            ChunkKind::Issue,
            "Fixed".into(),
            vec![],
            Some(serde_json::json!({"state": "closed", "labels": ["bug"]})),
        );

        let facts = extract_facts(&[chunk]);
        let fact = facts.iter().find(|f| f.category == "known_bugs").unwrap();
        assert!(fact.confidence < 0.9);
    }
}
