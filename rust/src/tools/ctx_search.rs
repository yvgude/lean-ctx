use std::path::Path;

use regex::Regex;
use walkdir::WalkDir;

use crate::core::protocol;
use crate::core::symbol_map::{self, SymbolMap};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

pub fn handle(pattern: &str, dir: &str, ext_filter: Option<&str>, max_results: usize, crp_mode: CrpMode) -> String {
    let re = match Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => return format!("ERROR: invalid regex: {e}"),
    };

    let root = Path::new(dir);
    if !root.exists() {
        return format!("ERROR: {dir} does not exist");
    }

    let mut matches = Vec::new();
    let mut files_searched = 0u32;
    let mut total_original_tokens = 0usize;

    for entry in WalkDir::new(root).min_depth(1).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_dir() {
            continue;
        }

        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

        if name.starts_with('.') {
            continue;
        }
        if is_binary_ext(path) {
            continue;
        }

        if let Some(ext) = ext_filter {
            let file_ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if file_ext != ext {
                continue;
            }
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        files_searched += 1;
        total_original_tokens += count_tokens(&content);

        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                let short_path = protocol::shorten_path(&path.to_string_lossy());
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
        return format!("0 matches for '{pattern}' in {files_searched} files");
    }

    let mut result = format!(
        "{} matches in {} files:\n{}",
        matches.len(),
        files_searched,
        matches.join("\n")
    );

    if crp_mode.is_tdd() {
        let file_ext = ext_filter.unwrap_or("rs");
        let mut sym = SymbolMap::new();
        let idents = symbol_map::extract_identifiers(&result, file_ext);
        for ident in &idents {
            sym.register(ident);
        }
        let compressed = sym.apply(&result);
        let sym_table = sym.format_table();
        result = format!("{compressed}{sym_table}");
    }

    let sent = count_tokens(&result);
    let savings = protocol::format_savings(total_original_tokens, sent);

    format!("{result}\n{savings}")
}

fn is_binary_ext(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(
        ext,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "svg"
            | "woff" | "woff2" | "ttf" | "eot"
            | "pdf" | "zip" | "tar" | "gz" | "br"
            | "mp3" | "mp4" | "webm" | "ogg"
            | "wasm" | "so" | "dylib" | "dll" | "exe"
            | "lock"
    )
}
