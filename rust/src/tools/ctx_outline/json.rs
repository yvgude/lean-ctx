//! Deterministic JSON outline (gitlab #981). Byte-stable per #498: fixed field
//! order (serde struct order), files pre-sorted by the directory walker, symbols
//! in source order, no timestamps/counters. Each file carries the extraction
//! `backend` (`tree-sitter` | `regex`) so the syntax-aware claim is verifiable.
//! Ships what `ast-grep outline` still lists as a TODO.

use serde::Serialize;

use crate::core::language_capabilities::language_for_ext;
use crate::core::signatures::{SigBackend, Signature};

use super::FileSymbols;

const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize)]
struct SymbolJson<'a> {
    kind: &'a str,
    name: &'a str,
    exported: bool,
    #[serde(rename = "async")]
    is_async: bool,
    start_line: Option<usize>,
    end_line: Option<usize>,
    params: &'a str,
    return_type: &'a str,
}

#[derive(Serialize)]
struct FileJson<'a> {
    path: &'a str,
    language: &'a str,
    backend: &'a str,
    symbols: Vec<SymbolJson<'a>>,
}

#[derive(Serialize)]
struct DirJson<'a> {
    version: u32,
    root: &'a str,
    truncated: bool,
    files: Vec<FileJson<'a>>,
    /// Sources withheld by a path rule, the project policy or the gateway.
    /// Absent when none was withheld, so an unaffected listing keeps its
    /// bytes (#498); a count, never a path.
    #[serde(skip_serializing_if = "is_zero")]
    withheld: usize,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes the field by reference
fn is_zero(count: &usize) -> bool {
    *count == 0
}

pub(super) fn file_json(path: &str, ext: &str, backend: SigBackend, sigs: &[&Signature]) -> String {
    let fj = build_file_json(path, ext, backend, sigs.iter().copied());
    serde_json::to_string_pretty(&fj).unwrap_or_else(|_| "{}".to_string())
}

pub(super) fn dir_json(
    root: &str,
    files: &[FileSymbols],
    truncated: bool,
    withheld: usize,
) -> String {
    let files_json: Vec<FileJson> = files
        .iter()
        .map(|f| build_file_json(&f.rel, &f.ext, f.backend, f.sigs.iter()))
        .collect();
    let dj = DirJson {
        version: SCHEMA_VERSION,
        root,
        truncated,
        files: files_json,
        withheld,
    };
    serde_json::to_string_pretty(&dj).unwrap_or_else(|_| "{}".to_string())
}

fn build_file_json<'a, I>(path: &'a str, ext: &'a str, backend: SigBackend, sigs: I) -> FileJson<'a>
where
    I: IntoIterator<Item = &'a Signature>,
{
    let language = language_for_ext(ext).map_or(ext, |l| l.id_str());
    let symbols = sigs
        .into_iter()
        .map(|s| SymbolJson {
            kind: s.kind,
            name: s.name.as_str(),
            exported: s.is_exported,
            is_async: s.is_async,
            start_line: s.start_line,
            end_line: s.end_line,
            params: s.params.as_str(),
            return_type: s.return_type.as_str(),
        })
        .collect();
    FileJson {
        path,
        language,
        backend: backend.as_str(),
        symbols,
    }
}
