//! Structural diff using tree-sitter chunk identities (named declarations).
//!
//! Compares structural chunks from [`super::chunks_ts`] between two sources.

use serde::Serialize;

use super::chunk_data::ChunkKind;

/// Added / removed / modified structural symbol (declaration identified by name + start line).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StructuralSymbolDiff {
    pub change: StructuralChangeKind,
    pub name: String,
    pub symbol_kind: ChunkKind,
    /// 1-based start line in the **new** source (`Modified`, `Added`) or **old** (`Removed`).
    pub line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StructuralChangeKind {
    Added,
    Removed,
    Modified,
}

#[cfg(feature = "tree-sitter")]
type ChunkKey = (String, usize);

#[cfg(feature = "tree-sitter")]
fn chunk_index(
    source: &str,
    extension: &str,
) -> Option<std::collections::HashMap<ChunkKey, (String, ChunkKind)>> {
    use std::collections::HashMap;

    let chunks = super::chunks_ts::extract_chunks_ts("", source, extension)?;
    let mut map = HashMap::new();
    for c in chunks {
        map.insert((c.symbol_name, c.start_line), (c.content, c.kind));
    }
    Some(map)
}

#[cfg(feature = "tree-sitter")]
fn chunk_order(source: &str, extension: &str) -> Option<Vec<ChunkKey>> {
    Some(
        super::chunks_ts::extract_chunks_ts("", source, extension)?
            .into_iter()
            .map(|c| (c.symbol_name, c.start_line))
            .collect(),
    )
}

/// Compare two sources and report structural declaration changes for `extension` (e.g. `"rs"`).
///
/// Identity is `(symbol_name, start_line)` within each version; body text inequality ⇒ `Modified`.
/// Returns an empty list when tree-sitter is disabled or the language is unsupported.
#[must_use]
pub fn structural_symbol_diff(
    old_source: &str,
    new_source: &str,
    extension: &str,
) -> Vec<StructuralSymbolDiff> {
    #[cfg(feature = "tree-sitter")]
    {
        structural_symbol_diff_impl(old_source, new_source, extension)
    }
    #[cfg(not(feature = "tree-sitter"))]
    {
        let _ = (old_source, new_source, extension);
        Vec::new()
    }
}

#[cfg(feature = "tree-sitter")]
fn structural_symbol_diff_impl(
    old_source: &str,
    new_source: &str,
    extension: &str,
) -> Vec<StructuralSymbolDiff> {
    let Some(old_map) = chunk_index(old_source, extension) else {
        return Vec::new();
    };
    let Some(new_map) = chunk_index(new_source, extension) else {
        return Vec::new();
    };
    let Some(new_order) = chunk_order(new_source, extension) else {
        return Vec::new();
    };
    let Some(old_order) = chunk_order(old_source, extension) else {
        return Vec::new();
    };

    let mut out = Vec::new();

    for key in &new_order {
        let Some((body_new, kind_new)) = new_map.get(key) else {
            continue;
        };
        match old_map.get(key) {
            None => {
                out.push(StructuralSymbolDiff {
                    change: StructuralChangeKind::Added,
                    name: key.0.clone(),
                    symbol_kind: kind_new.clone(),
                    line: key.1,
                });
            }
            Some((body_old, _)) => {
                if body_old != body_new {
                    out.push(StructuralSymbolDiff {
                        change: StructuralChangeKind::Modified,
                        name: key.0.clone(),
                        symbol_kind: kind_new.clone(),
                        line: key.1,
                    });
                }
            }
        }
    }

    for key in &old_order {
        if !new_map.contains_key(key) {
            let Some((_, kind_old)) = old_map.get(key) else {
                continue;
            };
            out.push(StructuralSymbolDiff {
                change: StructuralChangeKind::Removed,
                name: key.0.clone(),
                symbol_kind: kind_old.clone(),
                line: key.1,
            });
        }
    }

    out.sort_by(|a, b| a.line.cmp(&b.line).then(a.name.cmp(&b.name)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "tree-sitter")]
    #[test]
    fn structural_diff_detects_added_removed_modified_rust() {
        let old = r"
pub fn a() { 1 }
pub fn b() { 2 }
";
        let new = r"
pub fn a() { 99 }
pub fn b() { 2 }
pub fn c() { 3 }
";
        let d = structural_symbol_diff(old, new, "rs");
        let kinds: Vec<_> = d.iter().map(|x| (&x.change, x.name.as_str())).collect();
        assert!(
            kinds.contains(&(&StructuralChangeKind::Modified, "a")),
            "{kinds:?}"
        );
        assert!(
            kinds.contains(&(&StructuralChangeKind::Added, "c")),
            "{kinds:?}"
        );

        let old2 = r"pub fn only() {}";
        let new2 = r"pub fn renamed() {}";
        let d2 = structural_symbol_diff(old2, new2, "rs");
        assert!(
            d2.iter()
                .any(|x| x.change == StructuralChangeKind::Removed && x.name == "only")
        );
        assert!(
            d2.iter()
                .any(|x| x.change == StructuralChangeKind::Added && x.name == "renamed")
        );
    }

    #[cfg(not(feature = "tree-sitter"))]
    #[test]
    fn structural_diff_disabled_returns_empty() {
        assert!(structural_symbol_diff("a", "b", "rs").is_empty());
    }
}
