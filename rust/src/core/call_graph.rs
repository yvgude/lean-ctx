use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::deep_queries;
use super::graph_index::{ProjectIndex, SymbolEntry};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallGraph {
    pub project_root: String,
    pub edges: Vec<CallEdge>,
    pub file_hashes: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallEdge {
    pub caller_file: String,
    pub caller_symbol: String,
    pub caller_line: usize,
    pub callee_name: String,
}

impl CallGraph {
    pub fn new(project_root: &str) -> Self {
        Self {
            project_root: project_root.to_string(),
            edges: Vec::new(),
            file_hashes: HashMap::new(),
        }
    }

    pub fn build(index: &ProjectIndex) -> Self {
        let project_root = &index.project_root;
        let mut graph = Self::new(project_root);

        let symbols_by_file = group_symbols_by_file(index);

        for rel_path in index.files.keys() {
            let abs_path = resolve_path(rel_path, project_root);
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let hash = simple_hash(&content);
            graph.file_hashes.insert(rel_path.clone(), hash);

            let ext = Path::new(rel_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");

            let analysis = deep_queries::analyze(&content, ext);
            let file_symbols = symbols_by_file.get(rel_path.as_str());

            for call in &analysis.calls {
                let caller_sym = find_enclosing_symbol(file_symbols, call.line + 1);
                graph.edges.push(CallEdge {
                    caller_file: rel_path.clone(),
                    caller_symbol: caller_sym,
                    caller_line: call.line + 1,
                    callee_name: call.callee.clone(),
                });
            }
        }

        graph
    }

    pub fn build_incremental(index: &ProjectIndex, previous: &CallGraph) -> Self {
        let project_root = &index.project_root;
        let mut graph = Self::new(project_root);
        let symbols_by_file = group_symbols_by_file(index);

        for rel_path in index.files.keys() {
            let abs_path = resolve_path(rel_path, project_root);
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let hash = simple_hash(&content);
            let changed = previous
                .file_hashes
                .get(rel_path)
                .map(|old| old != &hash)
                .unwrap_or(true);

            graph.file_hashes.insert(rel_path.clone(), hash);

            if !changed {
                let old_edges: Vec<_> = previous
                    .edges
                    .iter()
                    .filter(|e| e.caller_file == rel_path.as_str())
                    .cloned()
                    .collect();
                graph.edges.extend(old_edges);
                continue;
            }

            let ext = Path::new(rel_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");

            let analysis = deep_queries::analyze(&content, ext);
            let file_symbols = symbols_by_file.get(rel_path.as_str());

            for call in &analysis.calls {
                let caller_sym = find_enclosing_symbol(file_symbols, call.line + 1);
                graph.edges.push(CallEdge {
                    caller_file: rel_path.clone(),
                    caller_symbol: caller_sym,
                    caller_line: call.line + 1,
                    callee_name: call.callee.clone(),
                });
            }
        }

        graph
    }

    pub fn callers_of(&self, symbol: &str) -> Vec<&CallEdge> {
        let sym_lower = symbol.to_lowercase();
        self.edges
            .iter()
            .filter(|e| e.callee_name.to_lowercase() == sym_lower)
            .collect()
    }

    pub fn callees_of(&self, symbol: &str) -> Vec<&CallEdge> {
        let sym_lower = symbol.to_lowercase();
        self.edges
            .iter()
            .filter(|e| e.caller_symbol.to_lowercase() == sym_lower)
            .collect()
    }

    pub fn save(&self) -> Result<(), String> {
        let dir = call_graph_dir(&self.project_root)
            .ok_or_else(|| "Cannot determine home directory".to_string())?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string(self).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("call_graph.json"), json).map_err(|e| e.to_string())
    }

    pub fn load(project_root: &str) -> Option<Self> {
        let dir = call_graph_dir(project_root)?;
        let path = dir.join("call_graph.json");
        let content = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn load_or_build(project_root: &str, index: &ProjectIndex) -> Self {
        if let Some(previous) = Self::load(project_root) {
            Self::build_incremental(index, &previous)
        } else {
            Self::build(index)
        }
    }
}

fn call_graph_dir(project_root: &str) -> Option<std::path::PathBuf> {
    ProjectIndex::index_dir(project_root)
}

fn group_symbols_by_file(index: &ProjectIndex) -> HashMap<&str, Vec<&SymbolEntry>> {
    let mut map: HashMap<&str, Vec<&SymbolEntry>> = HashMap::new();
    for sym in index.symbols.values() {
        map.entry(sym.file.as_str()).or_default().push(sym);
    }
    for syms in map.values_mut() {
        syms.sort_by_key(|s| s.start_line);
    }
    map
}

fn find_enclosing_symbol(file_symbols: Option<&Vec<&SymbolEntry>>, line: usize) -> String {
    let syms = match file_symbols {
        Some(s) => s,
        None => return "<module>".to_string(),
    };

    let mut best: Option<&SymbolEntry> = None;
    for sym in syms {
        if line >= sym.start_line && line <= sym.end_line {
            match best {
                None => best = Some(sym),
                Some(prev) => {
                    let prev_span = prev.end_line - prev.start_line;
                    let cur_span = sym.end_line - sym.start_line;
                    if cur_span < prev_span {
                        best = Some(sym);
                    }
                }
            }
        }
    }

    best.map(|s| s.name.clone())
        .unwrap_or_else(|| "<module>".to_string())
}

fn resolve_path(relative: &str, project_root: &str) -> String {
    let p = Path::new(relative);
    if p.is_absolute() && p.exists() {
        return relative.to_string();
    }
    let relative = relative.trim_start_matches(['/', '\\']);
    let joined = Path::new(project_root).join(relative);
    joined.to_string_lossy().to_string()
}

fn simple_hash(content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callers_of_empty_graph() {
        let graph = CallGraph::new("/tmp");
        assert!(graph.callers_of("foo").is_empty());
    }

    #[test]
    fn callers_of_finds_edges() {
        let mut graph = CallGraph::new("/tmp");
        graph.edges.push(CallEdge {
            caller_file: "a.rs".to_string(),
            caller_symbol: "bar".to_string(),
            caller_line: 10,
            callee_name: "foo".to_string(),
        });
        graph.edges.push(CallEdge {
            caller_file: "b.rs".to_string(),
            caller_symbol: "baz".to_string(),
            caller_line: 20,
            callee_name: "foo".to_string(),
        });
        graph.edges.push(CallEdge {
            caller_file: "c.rs".to_string(),
            caller_symbol: "qux".to_string(),
            caller_line: 30,
            callee_name: "other".to_string(),
        });
        let callers = graph.callers_of("foo");
        assert_eq!(callers.len(), 2);
    }

    #[test]
    fn callees_of_finds_edges() {
        let mut graph = CallGraph::new("/tmp");
        graph.edges.push(CallEdge {
            caller_file: "a.rs".to_string(),
            caller_symbol: "main".to_string(),
            caller_line: 5,
            callee_name: "init".to_string(),
        });
        graph.edges.push(CallEdge {
            caller_file: "a.rs".to_string(),
            caller_symbol: "main".to_string(),
            caller_line: 6,
            callee_name: "run".to_string(),
        });
        graph.edges.push(CallEdge {
            caller_file: "a.rs".to_string(),
            caller_symbol: "other".to_string(),
            caller_line: 15,
            callee_name: "init".to_string(),
        });
        let callees = graph.callees_of("main");
        assert_eq!(callees.len(), 2);
    }

    #[test]
    fn find_enclosing_picks_narrowest() {
        let outer = SymbolEntry {
            file: "a.rs".to_string(),
            name: "Outer".to_string(),
            kind: "struct".to_string(),
            start_line: 1,
            end_line: 50,
            is_exported: true,
        };
        let inner = SymbolEntry {
            file: "a.rs".to_string(),
            name: "inner_fn".to_string(),
            kind: "fn".to_string(),
            start_line: 10,
            end_line: 20,
            is_exported: false,
        };
        let syms = vec![&outer, &inner];
        let result = find_enclosing_symbol(Some(&syms), 15);
        assert_eq!(result, "inner_fn");
    }

    #[test]
    fn find_enclosing_returns_module_when_no_match() {
        let sym = SymbolEntry {
            file: "a.rs".to_string(),
            name: "foo".to_string(),
            kind: "fn".to_string(),
            start_line: 10,
            end_line: 20,
            is_exported: false,
        };
        let syms = vec![&sym];
        let result = find_enclosing_symbol(Some(&syms), 5);
        assert_eq!(result, "<module>");
    }

    #[test]
    fn resolve_path_trims_rooted_relative_prefix() {
        let resolved = resolve_path(r"\src\main\kotlin\Example.kt", r"C:\repo");
        assert_eq!(
            resolved,
            Path::new(r"C:\repo")
                .join(r"src\main\kotlin\Example.kt")
                .to_string_lossy()
                .to_string()
        );
    }
}
