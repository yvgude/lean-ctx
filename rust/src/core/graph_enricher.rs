//! Unified Graph Enricher — indexes Git history, tests, and knowledge into the PropertyGraph.
//!
//! Three enrichment passes:
//! 1. **Git commits**: `git log` → Commit nodes + `changed_in` edges
//! 2. **Test files**: naming/annotation heuristics → Test nodes + `tested_by` edges
//! 3. **Knowledge bridge**: `ctx_knowledge` facts → Knowledge nodes + `mentioned_in` edges

use crate::core::property_graph::{CodeGraph, Edge, EdgeKind, Node};
use std::collections::HashSet;
use std::path::Path;

// ---------------------------------------------------------------------------
// Git History Indexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub date: String,
    pub message: String,
    pub files_changed: Vec<String>,
}

pub fn index_git_history(
    graph: &CodeGraph,
    project_root: &Path,
    max_commits: usize,
) -> anyhow::Result<EnrichmentStats> {
    let mut stats = EnrichmentStats::default();

    let output = std::process::Command::new("git")
        .args([
            "log",
            &format!("-{max_commits}"),
            "--format=%H%n%h%n%an%n%ai%n%s",
            "--name-only",
        ])
        .current_dir(project_root)
        .output();

    let output = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return Ok(stats),
    };

    let commits = parse_git_log(&output);
    for commit in &commits {
        let commit_node =
            Node::commit(&commit.short_hash, &commit.message).with_metadata(&format!(
                "{{\"author\":\"{}\",\"date\":\"{}\",\"hash\":\"{}\"}}",
                commit.author, commit.date, commit.hash
            ));

        let commit_id = graph.upsert_node(&commit_node)?;
        stats.commits_indexed += 1;

        for file in &commit.files_changed {
            if let Some(file_node) = graph.get_node_by_path(file)? {
                if let Some(file_id) = file_node.id {
                    graph.upsert_edge(&Edge::new(file_id, commit_id, EdgeKind::ChangedIn))?;
                    stats.edges_created += 1;
                }
            }
        }
    }

    Ok(stats)
}

fn parse_git_log(output: &str) -> Vec<CommitInfo> {
    let mut commits = Vec::new();
    let mut lines = output.lines().peekable();

    while lines.peek().is_some() {
        let hash = match lines.next() {
            Some(h) if !h.is_empty() && h.len() >= 7 => h.to_string(),
            _ => {
                lines.next();
                continue;
            }
        };

        let short_hash = match lines.next() {
            Some(s) => s.to_string(),
            None => break,
        };
        let author = match lines.next() {
            Some(a) => a.to_string(),
            None => break,
        };
        let date = match lines.next() {
            Some(d) => d.to_string(),
            None => break,
        };
        let message = match lines.next() {
            Some(m) => m.to_string(),
            None => break,
        };

        let mut files_changed = Vec::new();
        while let Some(line) = lines.peek() {
            if line.is_empty() {
                lines.next();
                break;
            }
            files_changed.push(line.to_string());
            lines.next();
        }

        commits.push(CommitInfo {
            hash,
            short_hash,
            author,
            date,
            message,
            files_changed,
        });
    }

    commits
}

// ---------------------------------------------------------------------------
// Test Indexer
// ---------------------------------------------------------------------------

const TEST_PATTERNS: &[&str] = &[
    "_test.",
    "test_",
    ".test.",
    ".spec.",
    "_spec.",
    "tests/",
    "__tests__/",
];

pub fn index_tests(graph: &CodeGraph, project_root: &Path) -> anyhow::Result<EnrichmentStats> {
    let mut stats = EnrichmentStats::default();

    let output = std::process::Command::new("git")
        .args(["ls-files"])
        .current_dir(project_root)
        .output();

    let files: Vec<String> = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(ToString::to_string)
            .collect(),
        _ => return Ok(stats),
    };

    for file in &files {
        if !is_test_file(file) {
            continue;
        }

        let test_node = Node::test(file, file);
        let test_id = graph.upsert_node(&test_node)?;
        stats.tests_indexed += 1;

        let tested_file = infer_tested_file(file);
        if let Some(ref tested) = tested_file {
            if files.contains(tested) {
                let target_node = graph.get_node_by_path(tested)?;
                if let Some(target) = target_node {
                    if let Some(target_id) = target.id {
                        graph.upsert_edge(&Edge::new(target_id, test_id, EdgeKind::TestedBy))?;
                        stats.edges_created += 1;
                    }
                } else {
                    let file_id = graph.upsert_node(&Node::file(tested))?;
                    graph.upsert_edge(&Edge::new(file_id, test_id, EdgeKind::TestedBy))?;
                    stats.edges_created += 1;
                }
            }
        }
    }

    Ok(stats)
}

fn is_test_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    TEST_PATTERNS.iter().any(|p| lower.contains(p))
}

fn infer_tested_file(test_path: &str) -> Option<String> {
    let name = Path::new(test_path).file_name()?.to_str()?;

    for pattern in &["_test.", ".test.", "_spec.", ".spec."] {
        if let Some(pos) = name.find(pattern) {
            let base = &name[..pos];
            let ext = &name[pos + pattern.len() - 1..];
            let parent = Path::new(test_path).parent()?;

            let candidate = parent.join(format!("{base}{ext}"));
            if let Some(s) = candidate.to_str() {
                return Some(s.replace('\\', "/"));
            }

            if let Some(pp) = parent.parent() {
                let src_candidate = pp.join("src").join(format!("{base}{ext}"));
                if let Some(s) = src_candidate.to_str() {
                    return Some(s.replace('\\', "/"));
                }
            }
        }
    }

    if let Some(base) = name.strip_prefix("test_") {
        let parent = Path::new(test_path).parent()?;
        let candidate = parent.join(base);
        return candidate.to_str().map(|s| s.replace('\\', "/"));
    }

    None
}

// ---------------------------------------------------------------------------
// Knowledge Bridge
// ---------------------------------------------------------------------------

pub fn index_knowledge(graph: &CodeGraph, project_root: &str) -> anyhow::Result<EnrichmentStats> {
    let mut stats = EnrichmentStats::default();

    let knowledge = crate::core::knowledge::ProjectKnowledge::load(project_root);
    let Some(knowledge) = knowledge else {
        return Ok(stats);
    };

    let mut mentioned_files: HashSet<String> = HashSet::new();

    for fact in &knowledge.facts {
        let node = Node::knowledge(&fact.key, &format!("[{}] {}", fact.category, fact.value));
        let knowledge_id = graph.upsert_node(&node)?;
        stats.knowledge_indexed += 1;

        for file_ref in extract_file_refs(&fact.value) {
            if mentioned_files.insert(format!("{}:{}", fact.key, file_ref)) {
                if let Some(file_node) = graph.get_node_by_path(&file_ref)? {
                    if let Some(file_id) = file_node.id {
                        graph.upsert_edge(&Edge::new(
                            file_id,
                            knowledge_id,
                            EdgeKind::MentionedIn,
                        ))?;
                        stats.edges_created += 1;
                    }
                }
            }
        }
    }

    Ok(stats)
}

fn extract_file_refs(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    for word in text.split_whitespace() {
        let cleaned = word.trim_matches(|c: char| c == '`' || c == '\'' || c == '"' || c == ',');
        if looks_like_file_path(cleaned) {
            refs.push(cleaned.to_string());
        }
    }
    refs
}

fn looks_like_file_path(s: &str) -> bool {
    if s.len() < 4 || s.len() > 200 {
        return false;
    }
    let path = Path::new(s);
    let has_sep = s.contains('/') || s.contains('\\');
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let ext_lower = ext.to_ascii_lowercase();
            has_sep
                || matches!(
                    ext_lower.as_str(),
                    "rs" | "ts"
                        | "py"
                        | "js"
                        | "go"
                        | "java"
                        | "tsx"
                        | "jsx"
                        | "rb"
                        | "c"
                        | "cpp"
                        | "h"
                        | "cs"
                        | "swift"
                        | "kt"
                )
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Full enrichment pipeline
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct EnrichmentStats {
    pub commits_indexed: usize,
    pub tests_indexed: usize,
    pub knowledge_indexed: usize,
    pub edges_created: usize,
}

impl EnrichmentStats {
    pub fn merge(&mut self, other: &Self) {
        self.commits_indexed += other.commits_indexed;
        self.tests_indexed += other.tests_indexed;
        self.knowledge_indexed += other.knowledge_indexed;
        self.edges_created += other.edges_created;
    }

    pub fn format_summary(&self) -> String {
        format!(
            "Graph enriched: {} commits, {} tests, {} knowledge entries, {} edges",
            self.commits_indexed, self.tests_indexed, self.knowledge_indexed, self.edges_created
        )
    }
}

pub fn enrich_graph(
    graph: &CodeGraph,
    project_root: &Path,
    max_commits: usize,
) -> anyhow::Result<EnrichmentStats> {
    let mut total = EnrichmentStats::default();

    let git_stats = index_git_history(graph, project_root, max_commits)?;
    total.merge(&git_stats);

    let test_stats = index_tests(graph, project_root)?;
    total.merge(&test_stats);

    if let Some(root_str) = project_root.to_str() {
        let knowledge_stats = index_knowledge(graph, root_str)?;
        total.merge(&knowledge_stats);
    }

    Ok(total)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property_graph::NodeKind;

    #[test]
    fn parse_git_log_basic() {
        let log = "abc1234567890abcdef1234567890abcdef12345678\nabc1234\nJohn Doe\n2026-04-28 12:00:00 +0200\nfeat: add feature\nsrc/main.rs\nsrc/lib.rs\n\n";
        let commits = parse_git_log(log);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].short_hash, "abc1234");
        assert_eq!(commits[0].author, "John Doe");
        assert_eq!(commits[0].files_changed.len(), 2);
    }

    #[test]
    fn parse_git_log_multiple() {
        let log = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2\na1b2c3d\nAlice\n2026-04-27\nfirst\nfile1.rs\n\nf6e5d4c3b2a1f6e5d4c3b2a1f6e5d4c3b2a1f6e5\nf6e5d4c\nBob\n2026-04-28\nsecond\nfile2.rs\nfile3.rs\n\n";
        let commits = parse_git_log(log);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[1].files_changed.len(), 2);
    }

    #[test]
    fn is_test_file_detection() {
        assert!(is_test_file("src/utils_test.rs"));
        assert!(is_test_file("tests/integration.rs"));
        assert!(is_test_file("src/component.test.ts"));
        assert!(is_test_file("src/component.spec.js"));
        assert!(is_test_file("__tests__/app.js"));
        assert!(!is_test_file("src/main.rs"));
        assert!(!is_test_file("src/utils.rs"));
    }

    #[test]
    fn infer_tested_file_from_test() {
        assert_eq!(
            infer_tested_file("src/utils_test.rs"),
            Some("src/utils.rs".to_string())
        );
        assert_eq!(
            infer_tested_file("src/component.test.ts"),
            Some("src/component.ts".to_string())
        );
        assert_eq!(
            infer_tested_file("src/app.spec.js"),
            Some("src/app.js".to_string())
        );
    }

    #[test]
    fn infer_tested_file_prefix() {
        assert_eq!(
            infer_tested_file("tests/test_parser.py"),
            Some("tests/parser.py".to_string())
        );
    }

    #[test]
    fn looks_like_file_path_detection() {
        assert!(looks_like_file_path("src/main.rs"));
        assert!(looks_like_file_path("core/utils.ts"));
        assert!(looks_like_file_path("main.py"));
        assert!(!looks_like_file_path("hello"));
        assert!(!looks_like_file_path("a.b"));
        assert!(!looks_like_file_path(".hidden"));
    }

    #[test]
    fn extract_file_refs_from_text() {
        let text = "Changed `src/main.rs` and core/utils.ts for the fix";
        let refs = extract_file_refs(text);
        assert!(refs.contains(&"src/main.rs".to_string()));
        assert!(refs.contains(&"core/utils.ts".to_string()));
    }

    #[test]
    fn enrichment_stats_merge() {
        let mut a = EnrichmentStats {
            commits_indexed: 5,
            tests_indexed: 3,
            knowledge_indexed: 2,
            edges_created: 10,
        };
        let b = EnrichmentStats {
            commits_indexed: 2,
            tests_indexed: 1,
            knowledge_indexed: 0,
            edges_created: 4,
        };
        a.merge(&b);
        assert_eq!(a.commits_indexed, 7);
        assert_eq!(a.edges_created, 14);
    }

    #[test]
    fn enrichment_stats_format() {
        let s = EnrichmentStats {
            commits_indexed: 10,
            tests_indexed: 5,
            knowledge_indexed: 3,
            edges_created: 20,
        };
        let fmt = s.format_summary();
        assert!(fmt.contains("10 commits"));
        assert!(fmt.contains("5 tests"));
    }

    #[test]
    fn commit_node_construction() {
        let node = Node::commit("abc1234", "feat: add feature");
        assert_eq!(node.kind, NodeKind::Commit);
        assert_eq!(node.name, "abc1234");
    }

    #[test]
    fn test_node_construction() {
        let node = Node::test("src/utils_test.rs", "src/utils_test.rs");
        assert_eq!(node.kind, NodeKind::Test);
        assert_eq!(node.file_path, "src/utils_test.rs");
    }

    #[test]
    fn knowledge_node_construction() {
        let node = Node::knowledge("k1", "Database uses PostgreSQL");
        assert_eq!(node.kind, NodeKind::Knowledge);
        assert!(node.metadata.unwrap().contains("PostgreSQL"));
    }

    #[test]
    fn graph_commit_and_edge() {
        let g = CodeGraph::open_in_memory().unwrap();
        let file_id = g.upsert_node(&Node::file("src/main.rs")).unwrap();
        let commit_id = g.upsert_node(&Node::commit("abc1234", "fix bug")).unwrap();
        g.upsert_edge(&Edge::new(file_id, commit_id, EdgeKind::ChangedIn))
            .unwrap();

        let edges = g.edges_from(file_id).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::ChangedIn);
    }

    #[test]
    fn graph_test_edge() {
        let g = CodeGraph::open_in_memory().unwrap();
        let code_id = g.upsert_node(&Node::file("src/utils.rs")).unwrap();
        let test_id = g
            .upsert_node(&Node::test("src/utils_test.rs", "test_parse"))
            .unwrap();
        g.upsert_edge(&Edge::new(code_id, test_id, EdgeKind::TestedBy))
            .unwrap();

        let edges = g.edges_from(code_id).unwrap();
        assert_eq!(edges[0].kind, EdgeKind::TestedBy);
    }

    #[test]
    fn graph_knowledge_edge() {
        let g = CodeGraph::open_in_memory().unwrap();
        let file_id = g.upsert_node(&Node::file("src/db.rs")).unwrap();
        let k_id = g
            .upsert_node(&Node::knowledge("db_type", "Uses PostgreSQL"))
            .unwrap();
        g.upsert_edge(&Edge::new(file_id, k_id, EdgeKind::MentionedIn))
            .unwrap();

        let edges = g.edges_from(file_id).unwrap();
        assert_eq!(edges[0].kind, EdgeKind::MentionedIn);
    }
}
