use std::path::Path;

use crate::core::graph_provider::{self, SymbolInfo};
use crate::core::protocol;
use crate::core::tokens::count_tokens;

pub fn handle(
    name: &str,
    file: Option<&str>,
    kind: Option<&str>,
    project_root: &str,
) -> (String, usize) {
    crate::core::policy::runtime::with_project_source_view(project_root, || {
        handle_in_view(name, file, kind, project_root)
    })
    .unwrap_or_else(|_| {
        (
            "Symbol output withheld: source authority changed or could not be verified."
                .to_string(),
            0,
        )
    })
}

fn handle_in_view(
    name: &str,
    file: Option<&str>,
    kind: Option<&str>,
    project_root: &str,
) -> (String, usize) {
    let Some(open) = graph_provider::open_best_effort(project_root) else {
        return (
            format!(
                "Symbol '{name}' not found (graph index is building in the background — \
                 retry in a few seconds). Try ctx_search(pattern=\"{name}\") for an immediate broader search.",
            ),
            0,
        );
    };
    let gp = &open.provider;

    let matches = gp.find_symbols(name, file, kind);

    render_admitted(&admit_symbols(&matches, project_root))
}

/// Render the body of the symbol named `name` that best matches the full task.
pub fn best_symbol_snippet_for_task(
    name: &str,
    task: &str,
    project_root: &str,
) -> Option<(String, usize)> {
    crate::core::policy::runtime::with_project_source_view(project_root, || {
        best_symbol_snippet_in_view(name, task, project_root)
    })
    .ok()
    .flatten()
}

fn best_symbol_snippet_in_view(
    name: &str,
    task: &str,
    project_root: &str,
) -> Option<(String, usize)> {
    let open = graph_provider::open_best_effort(project_root)?;
    let gp = &open.provider;
    let candidates = gp.find_symbols(name, None, None);
    let admitted = admit_symbols(&candidates, project_root);
    let scores: Vec<usize> = admitted
        .iter()
        .map(|candidate| symbol_task_score(&candidate.symbol, task))
        .collect();
    let index = first_highest_score(&scores)?;
    let (rendered, _) = render_content(&admitted[index].symbol, &admitted[index].content);
    let emitted_tokens = count_tokens(&rendered);
    Some((rendered, emitted_tokens))
}

pub fn best_symbol_snippet(name: &str, project_root: &str) -> Option<(String, usize)> {
    best_symbol_snippet_for_task(name, name, project_root)
}

/// Return the first index with the highest score so an uninformative task
/// preserves `find_symbols(...).next()` behaviour instead of selecting the
/// last equal candidate.
fn first_highest_score(scores: &[usize]) -> Option<usize> {
    let (mut best_index, mut best_score) = (0, *scores.first()?);
    for (index, &score) in scores.iter().enumerate().skip(1) {
        if score > best_score {
            best_index = index;
            best_score = score;
        }
    }
    Some(best_index)
}

fn symbol_task_score(symbol: &SymbolInfo, task: &str) -> usize {
    let task_terms: std::collections::HashSet<String> = task
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| term.len() >= 3)
        .map(str::to_ascii_lowercase)
        .collect();
    let path_terms: std::collections::HashSet<String> = symbol
        .file
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| term.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect();
    let path_matches = task_terms.intersection(&path_terms).count();
    let exact_name = usize::from(task_terms.contains(&symbol.name.to_ascii_lowercase()));
    let source_bonus = usize::from(
        Path::new(&symbol.file)
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|ext| !matches!(ext, "md" | "mdx" | "rst" | "txt")),
    );
    path_matches * 100 + exact_name * 25 + source_bonus * 5 + usize::from(symbol.is_exported)
}

/// Render one symbol resolved from a stable handle (`path#name@Lline`),
/// bypassing the fuzzy name lookup and the `>5 matches, narrow with file=/kind=`
/// disambiguation entirely. Returns `(rendered, full_file_tokens)`, or a clear,
/// actionable message (tokens = 0) when the handle is malformed, the graph is
/// unavailable, or nothing resolves.
pub fn render_by_handle(handle: &str, project_root: &str) -> (String, usize) {
    if crate::core::handle::SymbolHandle::parse(handle).is_none() {
        return render_handle_in_view(handle, project_root);
    }
    crate::core::policy::runtime::with_project_source_view(project_root, || {
        render_handle_in_view(handle, project_root)
    })
    .unwrap_or_else(|_| {
        (
            "Symbol output withheld: source authority changed or could not be verified."
                .to_string(),
            0,
        )
    })
}

fn render_handle_in_view(handle: &str, project_root: &str) -> (String, usize) {
    let Some(parsed) = crate::core::handle::SymbolHandle::parse(handle) else {
        return (
            format!(
                "Invalid handle '{handle}'. Expected path#name@Lline, \
                 e.g. src/lib.rs#Config::load@L22."
            ),
            0,
        );
    };
    let Some(open) = graph_provider::open_best_effort(project_root) else {
        return (
            format!("Handle '{handle}' not resolvable (no graph available)."),
            0,
        );
    };
    let gp = &open.provider;
    match gp.find_symbol_by_handle(&parsed) {
        Some(sym) => {
            let admitted = admit_symbols(&[sym], project_root);
            let closest = admitted.iter().min_by_key(|entry| {
                parsed
                    .line
                    .map_or(0, |line| entry.symbol.start_line.abs_diff(line))
            });
            closest.map_or_else(
                || render_admitted(&[]),
                |entry| render_content(&entry.symbol, &entry.content),
            )
        }
        None => (
            format!(
                "No symbol for handle '{handle}'. \
                 Try ctx_search(action=\"symbol\", name=\"{}\").",
                parsed.name
            ),
            0,
        ),
    }
}

struct AdmittedSymbol {
    symbol: SymbolInfo,
    content: std::sync::Arc<str>,
}

/// An index is a discovery hint, never authorization to reveal its metadata.
/// Reparse bounded current sources after policy evaluation, including before
/// disambiguation lists and stable-handle rendering. Do not persist this view.
fn admit_symbols(candidates: &[SymbolInfo], project_root: &str) -> Vec<AdmittedSymbol> {
    const MAX_FILES: usize = 32;
    const MAX_BYTES: usize = 8 * 1024 * 1024;
    let mut files = std::collections::BTreeSet::new();
    let identities: std::collections::HashSet<_> = candidates
        .iter()
        .take(256)
        .map(|candidate| {
            (
                candidate.file.as_str(),
                candidate.name.as_str(),
                candidate.kind.as_str(),
            )
        })
        .collect();
    let mut remaining_bytes = MAX_BYTES;
    let mut admitted = Vec::new();
    for candidate in candidates.iter().take(256) {
        if !files.insert(candidate.file.clone()) {
            continue;
        }
        if files.len() > MAX_FILES || remaining_bytes == 0 {
            break;
        }
        let path = resolve_file_path(&candidate.file, project_root);
        let Ok(content) = crate::tools::ctx_read::read_file_for_tool_rooted_budgeted(
            &path,
            project_root,
            "ctx_symbol",
            &mut remaining_bytes,
        ) else {
            continue;
        };
        let extension = Path::new(&candidate.file)
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("");
        let signatures = crate::core::signatures::extract_signatures(&content, extension);
        let content: std::sync::Arc<str> = content.into();
        for signature in signatures {
            if !identities.contains(&(
                candidate.file.as_str(),
                signature.name.as_str(),
                signature.kind,
            )) {
                continue;
            }
            let (start_line, end_line) = signature
                .start_line
                .zip(signature.end_line)
                .unwrap_or_else(|| {
                    crate::core::graph_index::find_symbol_range(&content, &signature)
                });
            admitted.push(AdmittedSymbol {
                symbol: SymbolInfo {
                    name: signature.name,
                    file: candidate.file.clone(),
                    kind: signature.kind.to_string(),
                    start_line,
                    end_line,
                    is_exported: signature.is_exported,
                },
                content: content.clone(),
            });
            if admitted.len() >= 256 {
                return admitted;
            }
        }
    }
    admitted
}

fn render_admitted(symbols: &[AdmittedSymbol]) -> (String, usize) {
    if symbols.is_empty() {
        return (
            "No matching symbol from an admissible current source.".to_string(),
            0,
        );
    }
    if symbols.len() > 5 {
        let mut out =
            "Matching symbols from admissible current sources; narrow with file= or kind=:\n"
                .to_string();
        for entry in symbols.iter().take(20) {
            let m = &entry.symbol;
            out.push_str(&format!(
                "  {}::{} ({}:L{}-{})\n",
                m.file, m.name, m.kind, m.start_line, m.end_line
            ));
        }
        return (out, 0);
    }
    let mut out = Vec::new();
    let mut original = 0;
    for entry in symbols {
        let (text, tokens) = render_content(&entry.symbol, &entry.content);
        out.push(text);
        original = original.max(tokens);
    }
    (out.join("\n---\n\n"), original)
}

fn render_content(sym: &SymbolInfo, content: &str) -> (String, usize) {
    let lines: Vec<&str> = content.lines().collect();
    let start = sym.start_line.saturating_sub(1).min(lines.len());
    let end = sym.end_line.min(lines.len());
    if start >= end {
        return (
            format!(
                "{}#{}@L{} — stale index (file is {}L, symbol indexed at L{}-{}). Reindexing.",
                sym.file,
                sym.name,
                sym.start_line,
                lines.len(),
                sym.start_line,
                sym.end_line
            ),
            0,
        );
    }
    let snippet: String = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>4}|{}", start + i + 1, line))
        .collect::<Vec<_>>()
        .join("\n");

    let full_tokens = count_tokens(content);
    let snippet_tokens = count_tokens(&snippet);

    let vis = if sym.is_exported { "+" } else { "-" };
    let cc_note = symbol_cc_note(content, &sym.file, &sym.name, sym.start_line);
    // Lead with the stable handle (`path#name@Lline`) so the agent can re-target
    // this exact symbol next turn via ctx_search(action="symbol", handle=…).
    let handle = crate::core::handle::emit(&sym.file, &sym.name, sym.start_line);
    let header = format!(
        "{handle}  ({vis} {}, L{}-{}){cc_note}",
        sym.kind, sym.start_line, sym.end_line
    );

    let ctx = format!(
        "File: {} ({} lines, {} tokens)",
        sym.file,
        lines.len(),
        full_tokens
    );

    let savings = protocol::format_savings(full_tokens, snippet_tokens);

    (
        format!("{header}\n{ctx}\n\n{snippet}\n{savings}"),
        full_tokens,
    )
}

/// Optional ` · cc=NN` suffix for a symbol header — the code-health complexity
/// of the function being shown (#1084). Computed fresh from the already-read
/// file content, so it's exact for *any* symbol. Over-threshold functions are
/// flagged `(over)`. Honors the `code_health.annotate_reads` opt-out and is
/// empty for non-functions / when tree-sitter is off.
fn symbol_cc_note(content: &str, file: &str, name: &str, start_line: usize) -> String {
    let cfg = crate::core::config::Config::load();
    if !cfg.code_health.annotate_reads {
        return String::new();
    }
    let ext = Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match crate::core::code_health::annotate::cognitive_for_symbol(content, ext, name, start_line) {
        Some(cc) if cc > cfg.code_health.cognitive_threshold => format!(" · cc={cc} (over)"),
        Some(cc) => format!(" · cc={cc}"),
        None => String::new(),
    }
}

fn resolve_file_path(relative: &str, project_root: &str) -> String {
    let p = Path::new(relative);
    if p.is_absolute() && p.exists() {
        return relative.to_string();
    }
    let joined = Path::new(project_root).join(relative);
    if joined.exists() {
        return joined.to_string_lossy().to_string();
    }
    relative.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph_index::{ProjectIndex, SymbolEntry};
    use crate::core::graph_provider::GraphProvider;

    fn test_provider() -> GraphProvider {
        let mut index = ProjectIndex::new("/tmp/test");
        index.symbols.insert(
            "src/main.rs::main".to_string(),
            SymbolEntry {
                file: "src/main.rs".to_string(),
                name: "main".to_string(),
                kind: "fn".to_string(),
                start_line: 1,
                end_line: 10,
                is_exported: false,
            },
        );
        index.symbols.insert(
            "src/lib.rs::Config".to_string(),
            SymbolEntry {
                file: "src/lib.rs".to_string(),
                name: "Config".to_string(),
                kind: "struct".to_string(),
                start_line: 5,
                end_line: 20,
                is_exported: true,
            },
        );
        index.symbols.insert(
            "src/lib.rs::Config::load".to_string(),
            SymbolEntry {
                file: "src/lib.rs".to_string(),
                name: "Config::load".to_string(),
                kind: "method".to_string(),
                start_line: 22,
                end_line: 35,
                is_exported: true,
            },
        );
        GraphProvider::GraphIndex(index)
    }

    #[test]
    fn find_exact_match() {
        let gp = test_provider();
        let results = gp.find_symbols("main", None, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "main");
    }

    #[test]
    fn find_with_kind_filter() {
        let gp = test_provider();
        let results = gp.find_symbols("Config", None, Some("struct"));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, "struct");
    }

    #[test]
    fn find_with_file_filter() {
        let gp = test_provider();
        let results = gp.find_symbols("Config", Some("lib.rs"), None);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn no_match_returns_empty() {
        let gp = test_provider();
        let results = gp.find_symbols("nonexistent", None, None);
        assert!(results.is_empty());
    }

    #[test]
    fn render_single_header_carries_handle() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("src")).expect("mkdir");
        std::fs::write(
            tmp.path().join("src/lib.rs"),
            "struct Config;\nimpl Config { fn load() {} }\n",
        )
        .expect("write");
        let mut idx = ProjectIndex::new(tmp.path().to_str().unwrap());
        idx.symbols.insert(
            "src/lib.rs::Config".to_string(),
            SymbolEntry {
                file: "src/lib.rs".to_string(),
                name: "Config".to_string(),
                kind: "struct".to_string(),
                start_line: 1,
                end_line: 1,
                is_exported: true,
            },
        );
        let gp = GraphProvider::GraphIndex(idx);
        let sym = gp
            .find_symbols("Config", None, None)
            .into_iter()
            .next()
            .unwrap();
        let (out, _) = render_admitted(&admit_symbols(&[sym], tmp.path().to_str().unwrap()));
        assert!(
            out.contains("src/lib.rs#Config@L1"),
            "header must carry the stable handle, got: {out}"
        );
    }

    #[test]
    fn render_by_handle_rejects_malformed() {
        let (out, tok) = render_by_handle("not-a-handle", "/tmp/does-not-exist");
        assert!(out.contains("Invalid handle"), "got: {out}");
        assert_eq!(tok, 0);
    }

    #[test]
    fn full_task_path_terms_disambiguate_same_named_symbols() {
        let api = SymbolInfo {
            name: "GetMaxCurrent".into(),
            file: "api/actionconfig.go".into(),
            kind: "method".into(),
            start_line: 38,
            end_line: 40,
            is_exported: true,
        };
        let ocpp = SymbolInfo {
            name: "GetMaxCurrent".into(),
            file: "charger/ocpp.go".into(),
            kind: "method".into(),
            start_line: 357,
            end_line: 369,
            is_exported: true,
        };
        let task = "OCPP charger GetMaxCurrent Current.Offered measurand";

        assert!(symbol_task_score(&ocpp, task) > symbol_task_score(&api, task));
    }

    #[test]
    fn equal_scores_preserve_first_symbol_match() {
        assert_eq!(first_highest_score(&[0, 0, 0]), Some(0));
        assert_eq!(first_highest_score(&[2, 7, 7]), Some(1));
        assert_eq!(first_highest_score(&[]), None);
    }

    #[test]
    fn admitted_symbol_recomputes_span_and_rejects_removed_names() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.rs");
        std::fs::write(&source, "// moved\n\npub fn current() {}\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let indexed = SymbolInfo {
            name: "current".into(),
            file: "source.rs".into(),
            kind: "fn".into(),
            start_line: 1,
            end_line: 1,
            is_exported: false,
        };
        let admitted = admit_symbols(std::slice::from_ref(&indexed), root);
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].symbol.start_line, 3);
        assert!(admitted[0].symbol.is_exported);
        std::fs::write(&source, "pub fn renamed() {}\n").unwrap();
        assert!(admit_symbols(&[indexed], root).is_empty());
    }
}
