//! Shared headless apply path for symbol-body edits (spec v2a §5.1).
//!
//! `local_range_write` is the Trait-default for `replace_symbol_body` /
//! `insert_before_symbol` / `insert_after_symbol`: it writes a resolved range
//! to disk atomically, so edits work without any running language server / IDE.
//! `JetBrainsHttpBackend` overrides the Trait methods with the in-IDE HTTP path;
//! both paths apply the *same* tree-sitter range → byte-identical result.

use crate::lsp::backend::{EditResult, RangeEdit, TextRange0Based};

/// Convert a 0-based (line, character) coordinate to a byte offset into `content`.
/// `line`/`character` count UTF-8 *bytes* per line (wire convention here is byte
/// columns, matching how Rust slices `&str`). Out-of-range → `Err`.
pub fn offset_of(content: &str, line: u32, character: u32) -> Result<usize, String> {
    let mut offset = 0usize;
    let mut cur_line = 0u32;
    for l in content.split_inclusive('\n') {
        if cur_line == line {
            let line_len = l.trim_end_matches('\n').len();
            if character as usize > line_len {
                return Err(format!(
                    "POSITION_OUT_OF_RANGE: character {character} past end of line {line}"
                ));
            }
            return Ok(offset + character as usize);
        }
        offset += l.len();
        cur_line += 1;
    }
    // Allow the position one past the last line (line == cur_line, character 0):
    if line == cur_line && character == 0 {
        return Ok(offset);
    }
    Err(format!(
        "POSITION_OUT_OF_RANGE: line {line} past end of file"
    ))
}

/// Apply a resolved `RangeEdit` to disk (headless). Reads the file, optionally
/// verifies `expected_hash` against the *current* bytes covered by `range`
/// (mismatch → `CONFLICT`), replaces the range with `text`, writes atomically,
/// and returns the post-edit range + a compact diff.
pub fn local_range_write(edit: &RangeEdit) -> Result<EditResult, String> {
    let content = std::fs::read_to_string(&edit.abs_path)
        .map_err(|e| format!("FILE_NOT_FOUND: {}: {e}", edit.abs_path))?;

    let start = offset_of(&content, edit.range.start_line, edit.range.start_char)?;
    let end = offset_of(&content, edit.range.end_line, edit.range.end_char)?;
    if end < start {
        return Err("POSITION_OUT_OF_RANGE: end before start".to_string());
    }
    let old = &content[start..end];

    if let Some(expected) = edit.expected_hash.as_deref() {
        let actual = crate::core::hasher::hash_hex(old.as_bytes());
        if expected != actual {
            return Err(format!(
                "CONFLICT: range hash mismatch (expected={expected}, actual={actual})"
            ));
        }
    }

    let mut new_content = String::with_capacity(content.len() - old.len() + edit.text.len());
    new_content.push_str(&content[..start]);
    new_content.push_str(&edit.text);
    new_content.push_str(&content[end..]);

    write_file_atomic(&edit.abs_path, &new_content)?;

    let new_range = range_after_write(&content[..start], &edit.text);
    Ok(EditResult {
        applied: true,
        new_range,
        edited_text: edit.text.clone(),
        diff: build_range_diff(&edit.rel_path, old, &edit.text),
    })
}

/// Walk up from a file path to the nearest ancestor directory containing `.git`
/// (best-effort project-root detection; `nearest_project_root` does not exist in
/// this repo). Returns None if no `.git` ancestor is found.
fn nearest_git_root(abs_path: &str) -> Option<String> {
    let mut dir = std::path::Path::new(abs_path).parent();
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return Some(d.to_string_lossy().to_string());
        }
        dir = d.parent();
    }
    None
}

/// Build a file's structure overview from the tree-sitter symbol index
/// (headless `symbols_overview` default, spec v2a §5.2). Best-effort: returns
/// an empty vec when no graph is available.
#[must_use]
pub fn overview_from_index(abs_path: &str) -> Vec<crate::lsp::backend::SymbolOverviewItem> {
    use crate::core::graph_provider;
    let Some(project_root) = nearest_git_root(abs_path) else {
        return Vec::new();
    };
    let Some(open) = graph_provider::open_or_build(&project_root) else {
        return Vec::new();
    };
    let rel = abs_path
        .strip_prefix(&project_root)
        .map_or(abs_path, |s| s.trim_start_matches('/'));
    let mut items: Vec<_> = open
        .provider
        .find_symbols("", Some(rel), None)
        .into_iter()
        .map(|s| crate::lsp::backend::SymbolOverviewItem {
            name: s.name,
            kind: s.kind,
            line: s.start_line as u32,
        })
        .collect();
    items.sort_by_key(|i| i.line);
    items
}

/// Compute the 0-based range the freshly written `text` now occupies, given the
/// `prefix` (everything before the insertion point).
fn range_after_write(prefix: &str, text: &str) -> TextRange0Based {
    let (sl, sc) = line_col_at_end(prefix);
    let (dl, dc) = line_col_at_end(text);
    let end_line = sl + dl;
    let end_char = if dl == 0 { sc + dc } else { dc };
    TextRange0Based {
        start_line: sl,
        start_char: sc,
        end_line,
        end_char,
    }
}

/// (line, character) of the position *after* the last byte of `s` (0-based).
fn line_col_at_end(s: &str) -> (u32, u32) {
    let line = s.matches('\n').count() as u32;
    let col = match s.rfind('\n') {
        Some(i) => (s.len() - i - 1) as u32,
        None => s.len() as u32,
    };
    (line, col)
}

fn build_range_diff(path: &str, old: &str, new: &str) -> String {
    let mut out = format!("--- {path}\n");
    for l in old.lines() {
        out.push_str(&format!("- {l}\n"));
    }
    for l in new.lines() {
        out.push_str(&format!("+ {l}\n"));
    }
    out
}

fn write_file_atomic(path: &str, content: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);
    // Read-only-roots choke point (#475): headless ctx_refactor symbol edits
    // funnel through here — default-deny inside a read-only root.
    crate::core::pathjail::enforce_writable(p)?;
    let parent = p
        .parent()
        .ok_or_else(|| "invalid path (no parent directory)".to_string())?;
    let filename = p
        .file_name()
        .ok_or_else(|| "invalid path (no filename)".to_string())?
        .to_string_lossy();
    let pid = std::process::id();
    let tmp = parent.join(format!(".{filename}.lean-ctx.v2a.tmp.{pid}"));
    std::fs::write(&tmp, content.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, p).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("atomic write failed: {e}")
    })
}

/// Zero-dependency backend that carries only the Trait default-apply for the
/// three edit methods (used by `ctx_refactor` when no IDE is reachable). The five
/// mandatory read methods are unsupported here (edits never call them).
pub struct HeadlessBackend;

impl crate::lsp::backend::LspBackend for HeadlessBackend {
    fn open_file(&mut self, _u: &lsp_types::Uri, _l: &str, _t: &str) -> Result<(), String> {
        Ok(())
    }
    fn references(
        &mut self,
        _u: &lsp_types::Uri,
        _p: lsp_types::Position,
        _s: &str,
    ) -> Result<Vec<lsp_types::Location>, String> {
        Err("references requires a backend".into())
    }
    fn definition(
        &mut self,
        _u: &lsp_types::Uri,
        _p: lsp_types::Position,
    ) -> Result<lsp_types::GotoDefinitionResponse, String> {
        Err("definition requires a backend".into())
    }
    fn implementations(
        &mut self,
        _u: &lsp_types::Uri,
        _p: lsp_types::Position,
        _s: &str,
    ) -> Result<Vec<lsp_types::Location>, String> {
        Err("implementations requires a backend".into())
    }
    fn rename(
        &mut self,
        _u: &lsp_types::Uri,
        _p: lsp_types::Position,
        _n: &str,
    ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
        Err("rename requires a backend".into())
    }
    // replace_symbol_body / insert_before_symbol / insert_after_symbol inherit
    // the Trait default → local_range_write.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_of_maps_lines_and_columns() {
        let s = "ab\ncde\nf";
        assert_eq!(offset_of(s, 0, 0).unwrap(), 0);
        assert_eq!(offset_of(s, 0, 2).unwrap(), 2); // end of "ab"
        assert_eq!(offset_of(s, 1, 0).unwrap(), 3); // start of "cde"
        assert_eq!(offset_of(s, 1, 3).unwrap(), 6); // end of "cde"
        assert_eq!(offset_of(s, 2, 1).unwrap(), 8); // end of "f"
    }

    #[test]
    fn offset_of_one_past_last_line_is_eof() {
        let s = "ab\ncde\n";
        assert_eq!(offset_of(s, 2, 0).unwrap(), s.len());
    }

    #[test]
    fn offset_of_rejects_overrun() {
        let s = "ab\ncde";
        assert!(offset_of(s, 0, 5).is_err());
        assert!(offset_of(s, 9, 0).is_err());
    }

    fn tmp_file(content: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Foo.txt");
        std::fs::write(&path, content).unwrap();
        (dir, path.to_string_lossy().to_string())
    }

    fn edit(abs: &str, r: TextRange0Based, text: &str, hash: Option<String>) -> RangeEdit {
        RangeEdit {
            abs_path: abs.to_string(),
            rel_path: "Foo.txt".to_string(),
            range: r,
            text: text.to_string(),
            expected_hash: hash,
        }
    }

    #[test]
    fn local_range_write_replaces_range() {
        let (_d, p) = tmp_file("aaa\nBODY\nccc\n");
        let r = TextRange0Based {
            start_line: 1,
            start_char: 0,
            end_line: 1,
            end_char: 4,
        };
        let res = local_range_write(&edit(&p, r, "NEW", None)).unwrap();
        assert!(res.applied);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "aaa\nNEW\nccc\n");
        assert_eq!(res.edited_text, "NEW");
    }

    /// #475: the headless symbol-edit write path (`write_file_atomic`) must
    /// default-deny inside a read-only root, leaving the file untouched.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn local_range_write_denied_in_read_only_root() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("refrepo");
        std::fs::create_dir_all(&ro).unwrap();
        let path = ro.join("Foo.txt");
        std::fs::write(&path, "aaa\nBODY\nccc\n").unwrap();

        let ro_canon = crate::core::pathjail::canonicalize_or_self(&ro);
        crate::test_env::set_var(
            "LEAN_CTX_READ_ONLY_ROOTS",
            ro_canon.to_string_lossy().as_ref(),
        );
        let r = TextRange0Based {
            start_line: 1,
            start_char: 0,
            end_line: 1,
            end_char: 4,
        };
        let res = local_range_write(&edit(&path.to_string_lossy(), r, "NEW", None));
        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        let err = res.expect_err("write into a read-only root must be denied");
        assert!(
            err.contains("read-only"),
            "error must name the read-only tier: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "aaa\nBODY\nccc\n",
            "the file must be left untouched"
        );
    }

    #[test]
    fn overview_from_index_is_empty_without_graph() {
        // A path outside any project root must degrade to empty, not panic.
        let items = overview_from_index("/nonexistent/Nope.rs");
        assert!(items.is_empty());
    }

    #[test]
    fn local_range_write_zero_width_insert() {
        let (_d, p) = tmp_file("aaa\nccc\n");
        let r = TextRange0Based {
            start_line: 1,
            start_char: 0,
            end_line: 1,
            end_char: 0,
        };
        local_range_write(&edit(&p, r, "bbb\n", None)).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "aaa\nbbb\nccc\n");
    }

    #[test]
    fn local_range_write_hash_match_and_mismatch() {
        let (_d, p) = tmp_file("aaa\nBODY\nccc\n");
        let r = TextRange0Based {
            start_line: 1,
            start_char: 0,
            end_line: 1,
            end_char: 4,
        };
        let good = crate::core::hasher::hash_hex(b"BODY");
        // good hash matches current "BODY" → applies, line stays 4 chars wide ("XXXX")
        local_range_write(&edit(&p, r, "XXXX", Some(good))).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "aaa\nXXXX\nccc\n");
        // second write with a stale hash on the still-valid range → CONFLICT, file unchanged
        let err = local_range_write(&edit(&p, r, "YYYY", Some("deadbeef".into()))).unwrap_err();
        assert!(err.starts_with("CONFLICT"), "got: {err}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "aaa\nXXXX\nccc\n");
    }
}
