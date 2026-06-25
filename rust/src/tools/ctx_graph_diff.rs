//! `ctx_graph action=diff` — what changed since a git ref, crossed with the
//! dependency graph. For every changed file we report its **blast radius** (the
//! transitive set of files that depend on it) and flag changes that touch
//! god-nodes or bridges, so a reviewer immediately sees which commits are
//! structurally risky. graphify-style "graph diff", grounded in real git data.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::Duration;

use crate::core::graph_analysis::dependency_edges;
use crate::core::graph_index;
use crate::core::graph_provider::{self, EdgeInfo};
use crate::core::protocol::shorten_path;
use crate::core::tokens::count_tokens;

/// Reverse dependency index: `file → files that (transitively) depend on it`.
struct BlastIndex {
    rev: HashMap<String, Vec<String>>,
}

impl BlastIndex {
    fn build(edges: &[EdgeInfo]) -> Self {
        let mut rev: HashMap<String, Vec<String>> = HashMap::new();
        for (from, to) in dependency_edges(edges) {
            rev.entry(to.to_string())
                .or_default()
                .push(from.to_string());
        }
        Self { rev }
    }

    /// Number of files transitively depending on `start` (BFS, `start` excluded),
    /// bounded by `cap` to stay cheap on pathological graphs.
    fn transitive_dependents(&self, start: &str, cap: usize) -> usize {
        let mut visited: HashSet<&str> = HashSet::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        visited.insert(start);
        queue.push_back(start);
        while let Some(cur) = queue.pop_front() {
            if visited.len() > cap {
                break;
            }
            if let Some(deps) = self.rev.get(cur) {
                for d in deps {
                    if visited.insert(d.as_str()) {
                        queue.push_back(d.as_str());
                    }
                }
            }
        }
        visited.len().saturating_sub(1)
    }

    fn direct_dependents(&self, file: &str) -> usize {
        self.rev.get(file).map_or(0, Vec::len)
    }
}

/// One changed file with its graph impact.
struct DiffEntry {
    status: char,
    path: String,
    in_graph: bool,
    direct: usize,
    blast: usize,
    is_god: bool,
    is_bridge: bool,
}

/// Parse `git diff --name-status` output into `(status_char, repo_path)` pairs.
/// Renames/copies (`R…`, `C…`) are attributed to their destination path.
fn parse_name_status(raw: &str) -> Vec<(char, String)> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let mut cols = line.split('\t');
        let Some(status) = cols.next() else { continue };
        let code = status.chars().next().unwrap_or('?');
        let path = if matches!(code, 'R' | 'C') {
            // "Rxxx\told\tnew" — take the new path.
            cols.nth(1)
        } else {
            cols.next()
        };
        if let Some(p) = path.map(str::trim).filter(|p| !p.is_empty()) {
            out.push((code, p.to_string()));
        }
    }
    out
}

pub fn diff(since: Option<&str>, root: &str, format: Option<&str>) -> String {
    let base = since
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("HEAD~1");

    if !crate::core::git::git_available() {
        return "git is not available — cannot compute a graph diff.".to_string();
    }
    let root_path = Path::new(root);

    let name_status = match crate::core::git::run_git(
        &["diff", "--name-status", &format!("{base}..HEAD")],
        root_path,
        Duration::from_secs(30),
        &[],
    ) {
        Ok(o) if o.success => o.stdout,
        Ok(o) => {
            let msg = o.stderr.trim();
            return format!(
                "git diff failed: {}\nIs '{base}' a valid commit/ref reachable from HEAD?",
                if msg.is_empty() { "unknown error" } else { msg }
            );
        }
        Err(e) => return format!("git diff failed: {e}"),
    };

    let changes = parse_name_status(&name_status);
    if changes.is_empty() {
        return format!("No file changes between {base} and HEAD.");
    }

    let Some(open) = graph_provider::open_or_build(root) else {
        return "No graph index found. Run ctx_graph with action='build' first.".to_string();
    };
    let gp = &open.provider;
    let edges = gp.edges();
    let blast = BlastIndex::build(&edges);
    let node_set: HashSet<String> = gp.file_paths().into_iter().collect();
    let god: HashSet<String> = crate::core::graph_analysis::compute_god_nodes(&edges, 25)
        .into_iter()
        .map(|g| g.path)
        .collect();
    let bridge: HashSet<String> = crate::core::graph_analysis::compute_bridge_nodes(&edges, 25)
        .into_iter()
        .map(|b| b.path)
        .collect();

    let mut entries: Vec<DiffEntry> = changes
        .into_iter()
        .map(|(status, path)| {
            let key = graph_index::graph_match_key(&path);
            let in_graph = node_set.contains(&key);
            let (direct, blast_n) = if in_graph {
                (
                    blast.direct_dependents(&key),
                    blast.transitive_dependents(&key, 5000),
                )
            } else {
                (0, 0)
            };
            DiffEntry {
                status,
                in_graph,
                direct,
                blast: blast_n,
                is_god: god.contains(&key),
                is_bridge: bridge.contains(&key),
                path: key,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        b.blast
            .cmp(&a.blast)
            .then_with(|| b.direct.cmp(&a.direct))
            .then_with(|| a.path.cmp(&b.path))
    });

    if matches!(format, Some(f) if f.eq_ignore_ascii_case("json")) {
        return render_json(base, &entries, &edges);
    }
    render_text(base, &entries)
}

fn counts(entries: &[DiffEntry]) -> (usize, usize, usize, usize, usize) {
    let mut added = 0;
    let mut modified = 0;
    let mut deleted = 0;
    let mut renamed = 0;
    for e in entries {
        match e.status {
            'A' => added += 1,
            'M' => modified += 1,
            'D' => deleted += 1,
            'R' | 'C' => renamed += 1,
            _ => {}
        }
    }
    let in_graph = entries.iter().filter(|e| e.in_graph).count();
    (added, modified, deleted, renamed, in_graph)
}

fn render_text(base: &str, entries: &[DiffEntry]) -> String {
    let (added, modified, deleted, renamed, in_graph) = counts(entries);
    let mut out = format!("Graph diff: {base}..HEAD\n");
    out.push_str(&format!(
        "{} files changed (A:{added} M:{modified} D:{deleted} R:{renamed}) · {in_graph} in graph\n",
        entries.len()
    ));

    let high: Vec<&DiffEntry> = entries
        .iter()
        .filter(|e| e.in_graph && (e.blast > 0 || e.is_god || e.is_bridge))
        .collect();
    if !high.is_empty() {
        out.push_str("\nHigh-impact changes (by blast radius):\n");
        for e in &high {
            out.push_str(&format!(
                "  [{}] {:<46} blast {:<5} direct {}{}\n",
                e.status,
                shorten_path(&e.path),
                e.blast,
                e.direct,
                flags(e)
            ));
        }
    }

    let low: Vec<&DiffEntry> = entries
        .iter()
        .filter(|e| e.in_graph && e.blast == 0 && !e.is_god && !e.is_bridge)
        .collect();
    if !low.is_empty() {
        out.push_str("\nOther changed files in graph (no known dependents):\n");
        for e in low.iter().take(40) {
            out.push_str(&format!("  [{}] {}\n", e.status, shorten_path(&e.path)));
        }
        if low.len() > 40 {
            out.push_str(&format!("  … and {} more\n", low.len() - 40));
        }
    }

    let off: Vec<&DiffEntry> = entries.iter().filter(|e| !e.in_graph).collect();
    if !off.is_empty() {
        out.push_str(&format!(
            "\nChanged but not in graph ({}, e.g. docs/config/assets):\n",
            off.len()
        ));
        for e in off.iter().take(20) {
            out.push_str(&format!("  [{}] {}\n", e.status, shorten_path(&e.path)));
        }
        if off.len() > 20 {
            out.push_str(&format!("  … and {} more\n", off.len() - 20));
        }
    }

    let tokens = count_tokens(&out);
    format!("{out}[ctx_graph diff: {tokens} tok]")
}

fn flags(e: &DiffEntry) -> String {
    let mut tags = Vec::new();
    if e.is_god {
        tags.push("god-node");
    }
    if e.is_bridge {
        tags.push("bridge");
    }
    if tags.is_empty() {
        String::new()
    } else {
        format!("  ({})", tags.join(", "))
    }
}

fn render_json(base: &str, entries: &[DiffEntry], all_edges: &[EdgeInfo]) -> String {
    let (added, modified, deleted, renamed, in_graph) = counts(entries);

    // Nodes: all changed files that are in the graph.
    let nodes_json: Vec<_> = entries
        .iter()
        .filter(|e| e.in_graph)
        .map(|e| {
            let name = e.path.rsplit('/').next().unwrap_or(&e.path);
            serde_json::json!({ "name": name, "file": e.path })
        })
        .collect();

    // Edges: dependency edges between changed files (subgraph projection).
    let changed_set: HashSet<&str> = entries
        .iter()
        .filter(|e| e.in_graph)
        .map(|e| e.path.as_str())
        .collect();
    let edges_json: Vec<_> = all_edges
        .iter()
        .filter(|e| changed_set.contains(e.from.as_str()) && changed_set.contains(e.to.as_str()))
        .map(|e| serde_json::json!({ "source": e.from, "target": e.to, "type": e.kind }))
        .collect();

    let items: Vec<_> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "status": e.status.to_string(),
                "path": e.path,
                "in_graph": e.in_graph,
                "direct_dependents": e.direct,
                "blast_radius": e.blast,
                "is_god_node": e.is_god,
                "is_bridge": e.is_bridge,
            })
        })
        .collect();
    let val = serde_json::json!({
        "nodes": nodes_json,
        "edges": edges_json,
        "base": base,
        "summary": {
            "total": entries.len(),
            "added": added,
            "modified": modified,
            "deleted": deleted,
            "renamed": renamed,
            "in_graph": in_graph,
        },
        "changes": items,
    });
    serde_json::to_string_pretty(&val).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_statuses() {
        let raw = "M\tsrc/a.rs\nA\tsrc/b.rs\nD\tsrc/c.rs\n";
        let parsed = parse_name_status(raw);
        assert_eq!(
            parsed,
            vec![
                ('M', "src/a.rs".to_string()),
                ('A', "src/b.rs".to_string()),
                ('D', "src/c.rs".to_string()),
            ]
        );
    }

    #[test]
    fn rename_uses_destination_path() {
        let raw = "R096\tsrc/old.rs\tsrc/new.rs\n";
        let parsed = parse_name_status(raw);
        assert_eq!(parsed, vec![('R', "src/new.rs".to_string())]);
    }

    #[test]
    fn ignores_blank_lines() {
        assert!(parse_name_status("\n\n").is_empty());
    }

    #[test]
    fn blast_radius_is_transitive() {
        // a -> b -> c (a imports b, b imports c). c's dependents = {a, b}.
        let edges = vec![
            EdgeInfo {
                from: "a.rs".into(),
                to: "b.rs".into(),
                kind: "import".into(),
                weight: 1.0,
            },
            EdgeInfo {
                from: "b.rs".into(),
                to: "c.rs".into(),
                kind: "import".into(),
                weight: 1.0,
            },
        ];
        let idx = BlastIndex::build(&edges);
        assert_eq!(idx.transitive_dependents("c.rs", 100), 2);
        assert_eq!(idx.direct_dependents("c.rs"), 1);
        assert_eq!(idx.transitive_dependents("a.rs", 100), 0);
    }

    #[test]
    fn blast_radius_ignores_heuristic_edges() {
        // sibling is a co-location heuristic, not a dependency.
        let edges = vec![EdgeInfo {
            from: "a.rs".into(),
            to: "b.rs".into(),
            kind: "sibling".into(),
            weight: 1.0,
        }];
        let idx = BlastIndex::build(&edges);
        assert_eq!(idx.transitive_dependents("b.rs", 100), 0);
    }
}
