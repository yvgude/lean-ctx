use std::path::Path;

use crate::core::cache::SessionCache;
use crate::core::compressor;
use crate::core::deps;
use crate::core::entropy;
use crate::core::protocol;
use crate::core::signatures;
use crate::core::symbol_map::{self, SymbolMap};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

pub fn handle(cache: &mut SessionCache, path: &str, mode: &str, crp_mode: CrpMode) -> String {
    handle_with_options(cache, path, mode, false, crp_mode)
}

pub fn handle_fresh(cache: &mut SessionCache, path: &str, mode: &str, crp_mode: CrpMode) -> String {
    handle_with_options(cache, path, mode, true, crp_mode)
}

fn handle_with_options(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
) -> String {
    let file_ref = cache.get_file_ref(path);
    let short = protocol::shorten_path(path);
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    if fresh {
        cache.invalidate(path);
    }

    if mode == "diff" {
        return handle_diff(cache, path, &file_ref);
    }

    if cache.get(path).is_some() {
        if mode == "full" {
            return handle_full_with_auto_delta(cache, path, &file_ref, &short, ext, crp_mode);
        }
        let existing = cache.get(path).unwrap();
        let content = existing.content.clone();
        let original_tokens = existing.original_tokens;
        return process_mode(
            &content,
            mode,
            &file_ref,
            &short,
            ext,
            original_tokens,
            crp_mode,
        );
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return format!("ERROR: {e}"),
    };

    let (entry, _is_hit) = cache.store(path, content.clone());

    if mode == "full" {
        return format_full_output(cache, &file_ref, &short, ext, &content, &entry, crp_mode);
    }

    process_mode(
        &content,
        mode,
        &file_ref,
        &short,
        ext,
        entry.original_tokens,
        crp_mode,
    )
}

const AUTO_DELTA_THRESHOLD: f64 = 0.6;

/// Re-reads from disk; if content changed and delta is compact, sends auto-delta.
fn handle_full_with_auto_delta(
    cache: &mut SessionCache,
    path: &str,
    file_ref: &str,
    short: &str,
    ext: &str,
    crp_mode: CrpMode,
) -> String {
    let disk_content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => {
            cache.record_cache_hit(path);
            let existing = cache.get(path).unwrap();
            return format!(
                "{file_ref}={short} cached {}t {}L",
                existing.read_count, existing.line_count
            );
        }
    };

    let old_content = cache.get(path).unwrap().content.clone();
    let (entry, is_hit) = cache.store(path, disk_content.clone());

    if is_hit {
        return format!(
            "{file_ref}={short} cached {}t {}L",
            entry.read_count, entry.line_count
        );
    }

    let diff = compressor::diff_content(&old_content, &disk_content);
    let diff_tokens = count_tokens(&diff);
    let full_tokens = entry.original_tokens;

    if full_tokens > 0 && (diff_tokens as f64) < (full_tokens as f64 * AUTO_DELTA_THRESHOLD) {
        let savings = protocol::format_savings(full_tokens, diff_tokens);
        return format!(
            "{file_ref}={short} [auto-delta] ∆{}L\n{diff}\n{savings}",
            disk_content.lines().count()
        );
    }

    format_full_output(cache, file_ref, short, ext, &disk_content, &entry, crp_mode)
}

fn format_full_output(
    _cache: &mut SessionCache,
    file_ref: &str,
    short: &str,
    ext: &str,
    content: &str,
    entry: &crate::core::cache::CacheEntry,
    crp_mode: CrpMode,
) -> String {
    let tokens = entry.original_tokens;
    let header = build_header(file_ref, short, ext, content, entry.line_count, true);

    if crp_mode.is_tdd() {
        let mut sym = SymbolMap::new();
        let idents = symbol_map::extract_identifiers(content, ext);
        for ident in &idents {
            sym.register(ident);
        }
        let compressed_content = sym.apply(content);
        let sym_table = sym.format_table();
        let output = format!("{header}\n{compressed_content}{sym_table}");
        let sent = count_tokens(&output);
        let savings = protocol::format_savings(tokens, sent);
        return format!("{output}\n{savings}");
    }

    let output = format!("{header}\n{content}");
    let sent = count_tokens(&output);
    let savings = protocol::format_savings(tokens, sent);
    format!("{output}\n{savings}")
}

fn build_header(
    file_ref: &str,
    short: &str,
    ext: &str,
    content: &str,
    line_count: usize,
    include_deps: bool,
) -> String {
    let mut header = format!("{file_ref}={short} {line_count}L");

    if include_deps {
        let dep_info = deps::extract_deps(content, ext);
        if !dep_info.imports.is_empty() {
            let imports_str: Vec<&str> = dep_info
                .imports
                .iter()
                .take(8)
                .map(|s| s.as_str())
                .collect();
            header.push_str(&format!("\n deps {}", imports_str.join(",")));
        }
        if !dep_info.exports.is_empty() {
            let exports_str: Vec<&str> = dep_info
                .exports
                .iter()
                .take(8)
                .map(|s| s.as_str())
                .collect();
            header.push_str(&format!("\n exports {}", exports_str.join(",")));
        }
    }

    header
}

fn process_mode(
    content: &str,
    mode: &str,
    file_ref: &str,
    short: &str,
    ext: &str,
    original_tokens: usize,
    crp_mode: CrpMode,
) -> String {
    let line_count = content.lines().count();

    match mode {
        "signatures" => {
            let sigs = signatures::extract_signatures(content, ext);
            let dep_info = deps::extract_deps(content, ext);

            let mut output = format!("{file_ref}={short} {line_count}L");
            if !dep_info.imports.is_empty() {
                let imports_str: Vec<&str> = dep_info
                    .imports
                    .iter()
                    .take(8)
                    .map(|s| s.as_str())
                    .collect();
                output.push_str(&format!("\n deps {}", imports_str.join(",")));
            }
            for sig in &sigs {
                output.push('\n');
                if crp_mode.is_tdd() {
                    output.push_str(&sig.to_tdd());
                } else {
                    output.push_str(&sig.to_compact());
                }
            }
            let sent = count_tokens(&output);
            let savings = protocol::format_savings(original_tokens, sent);
            format!("{output}\n{savings}")
        }
        "map" => {
            let sigs = signatures::extract_signatures(content, ext);
            let dep_info = deps::extract_deps(content, ext);

            let mut output = format!("{file_ref}={short} {line_count}L");

            if !dep_info.imports.is_empty() {
                output.push_str("\n  deps: ");
                output.push_str(&dep_info.imports.join(", "));
            }

            if !dep_info.exports.is_empty() {
                output.push_str("\n  exports: ");
                output.push_str(&dep_info.exports.join(", "));
            }

            let key_sigs: Vec<&signatures::Signature> = sigs
                .iter()
                .filter(|s| s.is_exported || s.indent == 0)
                .collect();

            if !key_sigs.is_empty() {
                output.push_str("\n  API:");
                for sig in &key_sigs {
                    output.push_str("\n    ");
                    if crp_mode.is_tdd() {
                        output.push_str(&sig.to_tdd());
                    } else {
                        output.push_str(&sig.to_compact());
                    }
                }
            }

            let sent = count_tokens(&output);
            let savings = protocol::format_savings(original_tokens, sent);
            format!("{output}\n{savings}")
        }
        "aggressive" => {
            let compressed = compressor::aggressive_compress(content, Some(ext));
            let header = build_header(file_ref, short, ext, content, line_count, true);

            if crp_mode.is_tdd() {
                let mut sym = SymbolMap::new();
                let idents = symbol_map::extract_identifiers(&compressed, ext);
                for ident in &idents {
                    sym.register(ident);
                }
                let tdd_output = sym.apply(&compressed);
                let sym_table = sym.format_table();
                let sent = count_tokens(&tdd_output) + count_tokens(&sym_table);
                let savings = protocol::format_savings(original_tokens, sent);
                return format!("{header}\n{tdd_output}{sym_table}\n{savings}");
            }

            let sent = count_tokens(&compressed);
            let savings = protocol::format_savings(original_tokens, sent);
            format!("{header}\n{compressed}\n{savings}")
        }
        "entropy" => {
            let result = entropy::entropy_compress(content);
            let avg_h = entropy::analyze_entropy(content).avg_entropy;
            let header = build_header(file_ref, short, ext, content, line_count, false);
            let mut output = format!("{header} (H̄={avg_h:.1})");
            for tech in &result.techniques {
                output.push('\n');
                output.push_str(tech);
            }
            output.push('\n');
            output.push_str(&result.output);
            let sent = count_tokens(&output);
            let savings = protocol::format_savings(original_tokens, sent);
            format!("{output}\n{savings}")
        }
        mode if mode.starts_with("lines:") => {
            let range_str = &mode[6..];
            let extracted = extract_line_range(content, range_str);
            let header = format!("{file_ref}={short} {line_count}L lines:{range_str}");
            let sent = count_tokens(&extracted);
            let savings = protocol::format_savings(original_tokens, sent);
            format!("{header}\n{extracted}\n{savings}")
        }
        _ => {
            let header = build_header(file_ref, short, ext, content, line_count, true);
            format!("{header}\n{content}")
        }
    }
}

fn extract_line_range(content: &str, range_str: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let mut selected = Vec::new();

    for part in range_str.split(',') {
        let part = part.trim();
        if let Some((start_s, end_s)) = part.split_once('-') {
            let start = start_s.trim().parse::<usize>().unwrap_or(1).max(1);
            let end = end_s.trim().parse::<usize>().unwrap_or(total).min(total);
            for i in start..=end {
                if i >= 1 && i <= total {
                    selected.push(format!("{i:>4}| {}", lines[i - 1]));
                }
            }
        } else if let Ok(n) = part.parse::<usize>() {
            if n >= 1 && n <= total {
                selected.push(format!("{n:>4}| {}", lines[n - 1]));
            }
        }
    }

    if selected.is_empty() {
        "No lines matched the range.".to_string()
    } else {
        selected.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_toon_format_no_brackets() {
        let content = "use std::io;\nfn main() {}\n";
        let header = build_header("F1", "main.rs", "rs", content, 2, false);
        assert!(!header.contains('['));
        assert!(!header.contains(']'));
        assert!(header.contains("F1=main.rs 2L"));
    }

    #[test]
    fn test_header_toon_deps_indented() {
        let content = "use crate::core::cache;\nuse crate::tools;\npub fn main() {}\n";
        let header = build_header("F1", "main.rs", "rs", content, 3, true);
        if header.contains("deps") {
            assert!(
                header.contains("\n deps "),
                "deps should use indented TOON format"
            );
            assert!(
                !header.contains("deps:["),
                "deps should not use bracket format"
            );
        }
    }

    #[test]
    fn test_header_toon_saves_tokens() {
        let content = "use crate::foo;\nuse crate::bar;\npub fn baz() {}\npub fn qux() {}\n";
        let old_header = format!("F1=main.rs [4L +] deps:[foo,bar] exports:[baz,qux]");
        let new_header = build_header("F1", "main.rs", "rs", content, 4, true);
        let old_tokens = count_tokens(&old_header);
        let new_tokens = count_tokens(&new_header);
        assert!(
            new_tokens <= old_tokens,
            "TOON header ({new_tokens} tok) should be <= old format ({old_tokens} tok)"
        );
    }

    #[test]
    fn test_tdd_symbols_are_compact() {
        let symbols = [
            "⊕", "⊖", "∆", "→", "⇒", "✓", "✗", "⚠", "λ", "§", "∂", "τ", "ε",
        ];
        for sym in &symbols {
            let tok = count_tokens(sym);
            assert!(tok <= 2, "Symbol {sym} should be 1-2 tokens, got {tok}");
        }
    }
}

fn handle_diff(cache: &mut SessionCache, path: &str, file_ref: &str) -> String {
    let short = protocol::shorten_path(path);
    let old_content = cache.get(path).map(|e| e.content.clone());

    let new_content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return format!("ERROR: {e}"),
    };

    let original_tokens = count_tokens(&new_content);

    let diff_output = if let Some(old) = &old_content {
        compressor::diff_content(old, &new_content)
    } else {
        format!("[first read]\n{new_content}")
    };

    cache.store(path, new_content);

    let sent = count_tokens(&diff_output);
    let savings = protocol::format_savings(original_tokens, sent);
    format!("{file_ref}={short} [diff]\n{diff_output}\n{savings}")
}
