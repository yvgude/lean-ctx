use regex::Regex;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedReference {
    pub ref_type: ReferenceType,
    pub identifier: String,
    pub source_id: String,
    pub confidence_milli: u16,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReferenceType {
    #[default]
    JiraIssue,
    GitHubPR,
    GitHubIssue,
    FilePath,
    Function,
    Url,
}
pub trait ReferenceResolver: std::fmt::Debug + Send + Sync {
    fn resolve(&self, text: &str) -> Vec<ResolvedReference>;
    fn name(&self) -> &'static str;
}
#[derive(Debug, Clone, Default)]
pub struct PatternReferenceResolver;

impl ReferenceResolver for PatternReferenceResolver {
    fn resolve(&self, text: &str) -> Vec<ResolvedReference> {
        let jira = Regex::new(r"\b[A-Z][A-Z0-9]+-[0-9]+\b").expect("valid Jira regex");
        let github = Regex::new(r"#[0-9]+\b").expect("valid GitHub regex");
        let files = Regex::new(r"\b(?:[A-Za-z0-9_.-]+/)*[A-Za-z0-9_.-]+\.(?:rs|ts|py)\b")
            .expect("valid file regex");
        let mut result = jira
            .find_iter(text)
            .map(|m| reference(ReferenceType::JiraIssue, m.as_str(), "jira", 950))
            .collect::<Vec<_>>();
        result.extend(
            github
                .find_iter(text)
                .map(|m| reference(ReferenceType::GitHubPR, m.as_str(), "github", 900)),
        );
        result.extend(
            files
                .find_iter(text)
                .filter(|m| Path::new(m.as_str()).exists())
                .map(|m| reference(ReferenceType::FilePath, m.as_str(), "local_files", 980)),
        );
        result
    }
    fn name(&self) -> &'static str {
        "pattern"
    }
}
fn reference(
    ref_type: ReferenceType,
    identifier: &str,
    source_id: &str,
    confidence_milli: u16,
) -> ResolvedReference {
    ResolvedReference {
        ref_type,
        identifier: identifier.into(),
        source_id: source_id.into(),
        confidence_milli,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_jira_keys() {
        assert_eq!(
            PatternReferenceResolver.resolve("fix LEAN-42")[0].ref_type,
            ReferenceType::JiraIssue
        );
    }
    #[test]
    fn detects_github_issues() {
        assert_eq!(
            PatternReferenceResolver.resolve("see #789")[0].identifier,
            "#789"
        );
    }
    #[test]
    fn detects_existing_rust_paths() {
        assert_eq!(
            PatternReferenceResolver.resolve("src/main.rs")[0].ref_type,
            ReferenceType::FilePath
        );
    }
    #[test]
    fn ignores_nonexistent_paths() {
        {
            let found = &PatternReferenceResolver.resolve("missing/file.rs");
            assert!(found.is_empty(), "{found:?}");
        }
    }
}
