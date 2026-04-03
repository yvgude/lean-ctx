use std::path::Path;

use ignore::WalkBuilder;
use regex::Regex;

use crate::core::protocol;
use crate::core::symbol_map::{self, SymbolMap};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

const MAX_FILE_SIZE: u64 = 512_000;
const MAX_WALK_DEPTH: usize = 20;

pub fn handle(
    pattern: &str,
    dir: &str,
    ext_filter: Option<&str>,
    max_results: usize,
    _crp_mode: CrpMode,
    respect_gitignore: bool,
) -> (String, usize) {
    let re = match Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => return (format!("ERROR: invalid regex: {e}"), 0),
    };

    let root = Path::new(dir);
    if !root.exists() {
        return (format!("ERROR: {dir} does not exist"), 0);
    }

    let walker = WalkBuilder::new(root)
        .hidden(true)
        .max_depth(Some(MAX_WALK_DEPTH))
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .build();

    let mut matches = Vec::new();
    let mut raw_result_lines = Vec::new();
    let mut files_searched = 0u32;
    let mut files_skipped_size = 0u32;

    for entry in walker.filter_map(|e| e.ok()) {
        if entry.file_type().is_none_or(|ft| ft.is_dir()) {
            continue;
        }

        let path = entry.path();

        if is_binary_ext(path) || is_generated_file(path) {
            continue;
        }

        if let Some(ext) = ext_filter {
            let file_ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if file_ext != ext {
                continue;
            }
        }

        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > MAX_FILE_SIZE {
                files_skipped_size += 1;
                continue;
            }
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        files_searched += 1;

        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                let short_path = protocol::shorten_path(&path.to_string_lossy());
                let full_path = path.to_string_lossy();
                raw_result_lines.push(format!("{full_path}:{}: {}", i + 1, line.trim()));
                matches.push(format!("{short_path}:{} {}", i + 1, line.trim()));
                if matches.len() >= max_results {
                    break;
                }
            }
        }

        if matches.len() >= max_results {
            break;
        }
    }

    if matches.is_empty() {
        let mut msg = format!("0 matches for '{pattern}' in {files_searched} files");
        if files_skipped_size > 0 {
            msg.push_str(&format!(" ({files_skipped_size} large files skipped)"));
        }
        return (msg, 0);
    }

    let mut result = format!(
        "{} matches in {} files:\n{}",
        matches.len(),
        files_searched,
        matches.join("\n")
    );

    if files_skipped_size > 0 {
        result.push_str(&format!("\n({files_skipped_size} files >512KB skipped)"));
    }

    {
        let file_ext = ext_filter.unwrap_or("rs");
        let mut sym = SymbolMap::new();
        let idents = symbol_map::extract_identifiers(&result, file_ext);
        for ident in &idents {
            sym.register(ident);
        }
        if sym.len() >= 3 {
            let sym_table = sym.format_table();
            let compressed = sym.apply(&result);
            let original_tok = count_tokens(&result);
            let compressed_tok = count_tokens(&compressed) + count_tokens(&sym_table);
            let net_saving = original_tok.saturating_sub(compressed_tok);
            if original_tok > 0 && net_saving * 100 / original_tok >= 5 {
                result = format!("{compressed}{sym_table}");
            }
        }
    }

    let raw_output = raw_result_lines.join("\n");
    let raw_tokens = count_tokens(&raw_output);
    let sent = count_tokens(&result);
    let savings = protocol::format_savings(raw_tokens, sent);

    (format!("{result}\n{savings}"), raw_tokens)
}

fn is_binary_ext(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(
        ext,
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "ico"
            | "svg"
            | "woff"
            | "woff2"
            | "ttf"
            | "eot"
            | "pdf"
            | "zip"
            | "tar"
            | "gz"
            | "br"
            | "zst"
            | "bz2"
            | "xz"
            | "mp3"
            | "mp4"
            | "webm"
            | "ogg"
            | "wasm"
            | "so"
            | "dylib"
            | "dll"
            | "exe"
            | "lock"
            | "map"
            | "snap"
            | "patch"
            | "db"
            | "sqlite"
            | "parquet"
            | "arrow"
            | "bin"
            | "o"
            | "a"
            | "class"
            | "pyc"
            | "pyo"
    )
}

fn is_generated_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.ends_with(".min.js")
        || name.ends_with(".min.css")
        || name.ends_with(".bundle.js")
        || name.ends_with(".chunk.js")
        || name.ends_with(".d.ts")
        || name.ends_with(".js.map")
        || name.ends_with(".css.map")
}
