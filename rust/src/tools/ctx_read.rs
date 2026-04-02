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

pub fn read_file_lossy(path: &str) -> Result<String, std::io::Error> {
    let bytes = std::fs::read(path)?;
    match String::from_utf8(bytes) {
        Ok(s) => Ok(s),
        Err(e) => Ok(String::from_utf8_lossy(e.as_bytes()).into_owned()),
    }
}

pub fn handle(cache: &mut SessionCache, path: &str, mode: &str, crp_mode: CrpMode) -> String {
    handle_with_options(cache, path, mode, false, crp_mode, None)
}

pub fn handle_fresh(cache: &mut SessionCache, path: &str, mode: &str, crp_mode: CrpMode) -> String {
    handle_with_options(cache, path, mode, true, crp_mode, None)
}

pub fn handle_with_task(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> String {
    handle_with_options(cache, path, mode, false, crp_mode, task)
}

pub fn handle_fresh_with_task(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> String {
    handle_with_options(cache, path, mode, true, crp_mode, task)
}

fn handle_with_options(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
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
            let result = handle_full_with_auto_delta(cache, path, &file_ref, &short, ext, crp_mode);
            return maybe_apply_task_filter(result, cache, path, task);
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
            path,
            task,
        );
    }

    let content = match read_file_lossy(path) {
        Ok(c) => c,
        Err(e) => return format!("ERROR: {e}"),
    };

    let (entry, _is_hit) = cache.store(path, content.clone());

    if mode == "full" {
        let result = format_full_output(cache, &file_ref, &short, ext, &content, &entry, crp_mode);
        return maybe_apply_task_filter(result, cache, path, task);
    }

    process_mode(
        &content,
        mode,
        &file_ref,
        &short,
        ext,
        entry.original_tokens,
        crp_mode,
        path,
        task,
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
    let disk_content = match read_file_lossy(path) {
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
    _crp_mode: CrpMode,
) -> String {
    let tokens = entry.original_tokens;
    let metadata = build_header(file_ref, short, ext, content, entry.line_count, true);

    let mut sym = SymbolMap::new();
    let idents = symbol_map::extract_identifiers(content, ext);
    for ident in &idents {
        sym.register(ident);
    }

    let sym_beneficial = if sym.len() >= 3 {
        let sym_table = sym.format_table();
        let compressed = sym.apply(content);
        let original_tok = count_tokens(content);
        let compressed_tok = count_tokens(&compressed) + count_tokens(&sym_table);
        let net_saving = original_tok.saturating_sub(compressed_tok);
        original_tok > 0 && net_saving * 100 / original_tok >= 5
    } else {
        false
    };

    if sym_beneficial {
        let compressed_content = sym.apply(content);
        let sym_table = sym.format_table();
        let output = format!("{compressed_content}{sym_table}\n{metadata}");
        let sent = count_tokens(&output);
        let savings = protocol::format_savings(tokens, sent);
        return format!("{output}\n{savings}");
    }

    let output = format!("{content}\n{metadata}");
    let sent = count_tokens(&output);
    let savings = protocol::format_savings(tokens, sent);
    format!("{output}\n{savings}")
}

const TASK_FILTER_TOKEN_THRESHOLD: usize = 1000;
const TASK_FILTER_BUDGET_RATIO: f64 = 0.5;

fn maybe_apply_task_filter(
    full_output: String,
    cache: &mut SessionCache,
    path: &str,
    task: Option<&str>,
) -> String {
    let task_str = match task {
        Some(t) if !t.is_empty() => t,
        _ => return full_output,
    };

    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    if !crate::tools::ctx_smart_read::is_code_ext(ext) {
        return full_output;
    }

    let original_tokens = match cache.get(path) {
        Some(entry) => entry.original_tokens,
        None => return full_output,
    };

    if original_tokens < TASK_FILTER_TOKEN_THRESHOLD {
        return full_output;
    }

    let content = match cache.get(path) {
        Some(entry) => entry.content.clone(),
        None => return full_output,
    };

    let (_files, keywords) = crate::core::task_relevance::parse_task_hints(task_str);
    if keywords.is_empty() {
        return full_output;
    }

    let original_lines = content.lines().count();
    let filtered = crate::core::task_relevance::information_bottleneck_filter(
        &content,
        &keywords,
        TASK_FILTER_BUDGET_RATIO,
    );
    let filtered_lines = filtered.lines().count();

    if filtered_lines >= original_lines {
        return full_output;
    }

    let file_ref = cache.get_file_ref(path);
    let short = protocol::shorten_path(path);
    let header = format!(
        "{file_ref}={short} {original_lines}L [task-enhanced: {original_lines}→{filtered_lines}]"
    );
    let sent = count_tokens(&filtered) + count_tokens(&header);
    let savings = protocol::format_savings(original_tokens, sent);
    format!("{header}\n{filtered}\n{savings}")
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

#[allow(clippy::too_many_arguments)]
fn process_mode(
    content: &str,
    mode: &str,
    file_ref: &str,
    short: &str,
    ext: &str,
    original_tokens: usize,
    crp_mode: CrpMode,
    file_path: &str,
    task: Option<&str>,
) -> String {
    let line_count = content.lines().count();

    match mode {
        "auto" => {
            let sig =
                crate::core::mode_predictor::FileSignature::from_path(file_path, original_tokens);
            let predictor = crate::core::mode_predictor::ModePredictor::new();
            let resolved = predictor
                .predict_best_mode(&sig)
                .unwrap_or_else(|| "full".to_string());
            process_mode(
                content,
                &resolved,
                file_ref,
                short,
                ext,
                original_tokens,
                crp_mode,
                file_path,
                task,
            )
        }
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
            let raw = compressor::aggressive_compress(content, Some(ext));
            let compressed = compressor::safeguard_ratio(content, &raw);
            let header = build_header(file_ref, short, ext, content, line_count, true);

            let mut sym = SymbolMap::new();
            let idents = symbol_map::extract_identifiers(&compressed, ext);
            for ident in &idents {
                sym.register(ident);
            }

            let sym_beneficial = if sym.len() >= 3 {
                let sym_table = sym.format_table();
                let sym_applied = sym.apply(&compressed);
                let orig_tok = count_tokens(&compressed);
                let comp_tok = count_tokens(&sym_applied) + count_tokens(&sym_table);
                let net = orig_tok.saturating_sub(comp_tok);
                orig_tok > 0 && net * 100 / orig_tok >= 5
            } else {
                false
            };

            if sym_beneficial {
                let sym_output = sym.apply(&compressed);
                let sym_table = sym.format_table();
                let sent = count_tokens(&sym_output) + count_tokens(&sym_table);
                let savings = protocol::format_savings(original_tokens, sent);
                return format!("{header}\n{sym_output}{sym_table}\n{savings}");
            }

            let sent = count_tokens(&compressed);
            let savings = protocol::format_savings(original_tokens, sent);
            format!("{header}\n{compressed}\n{savings}")
        }
        "entropy" => {
            let result = entropy::entropy_compress_adaptive(content, file_path);
            let avg_h = entropy::analyze_entropy(content).avg_entropy;
            let header = build_header(file_ref, short, ext, content, line_count, false);
            let techs = result.techniques.join(", ");
            let output = format!("{header} H̄={avg_h:.1} [{techs}]\n{}", result.output);
            let sent = count_tokens(&output);
            let savings = protocol::format_savings(original_tokens, sent);
            format!("{output}\n{savings}")
        }
        "task" => {
            let task_str = task.unwrap_or("");
            if task_str.is_empty() {
                let header = build_header(file_ref, short, ext, content, line_count, true);
                return format!("{header}\n{content}\n[task mode: no task set — returned full]");
            }
            let (_files, keywords) = crate::core::task_relevance::parse_task_hints(task_str);
            if keywords.is_empty() {
                let header = build_header(file_ref, short, ext, content, line_count, true);
                return format!(
                    "{header}\n{content}\n[task mode: no keywords extracted — returned full]"
                );
            }
            let filtered =
                crate::core::task_relevance::information_bottleneck_filter(content, &keywords, 0.3);
            let filtered_lines = filtered.lines().count();
            let header = format!(
                "{file_ref}={short} {line_count}L [task-filtered: {line_count}→{filtered_lines}]"
            );
            let sent = count_tokens(&filtered) + count_tokens(&header);
            let savings = protocol::format_savings(original_tokens, sent);
            format!("{header}\n{filtered}\n{savings}")
        }
        "reference" => {
            let tok = count_tokens(content);
            let output = format!("{file_ref}={short}: {line_count} lines, {tok} tok ({ext})");
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

fn handle_diff(cache: &mut SessionCache, path: &str, file_ref: &str) -> String {
    let short = protocol::shorten_path(path);
    let old_content = cache.get(path).map(|e| e.content.clone());

    let new_content = match read_file_lossy(path) {
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
        let old_header = "F1=main.rs [4L +] deps:[foo,bar] exports:[baz,qux]".to_string();
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

    #[test]
    fn test_task_mode_filters_content() {
        let content = (0..200)
            .map(|i| {
                if i % 20 == 0 {
                    format!("fn validate_token(token: &str) -> bool {{ /* line {i} */ }}")
                } else {
                    format!("fn unrelated_helper_{i}(x: i32) -> i32 {{ x + {i} }}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let full_tokens = count_tokens(&content);
        let task = Some("fix bug in validate_token");
        let result = process_mode(
            &content,
            "task",
            "F1",
            "test.rs",
            "rs",
            full_tokens,
            CrpMode::Off,
            "test.rs",
            task,
        );
        let result_tokens = count_tokens(&result);
        assert!(
            result_tokens < full_tokens,
            "task mode ({result_tokens} tok) should be less than full ({full_tokens} tok)"
        );
        assert!(
            result.contains("task-filtered"),
            "output should contain task-filtered marker"
        );
    }

    #[test]
    fn test_task_mode_without_task_returns_full() {
        let content = "fn main() {}\nfn helper() {}\n";
        let tokens = count_tokens(content);
        let result = process_mode(
            content,
            "task",
            "F1",
            "test.rs",
            "rs",
            tokens,
            CrpMode::Off,
            "test.rs",
            None,
        );
        assert!(
            result.contains("no task set"),
            "should indicate no task: {result}"
        );
    }

    #[test]
    fn test_reference_mode_one_line() {
        let content = "fn main() {}\nfn helper() {}\nfn other() {}\n";
        let tokens = count_tokens(content);
        let result = process_mode(
            content,
            "reference",
            "F1",
            "test.rs",
            "rs",
            tokens,
            CrpMode::Off,
            "test.rs",
            None,
        );
        let lines: Vec<&str> = result.lines().collect();
        assert!(
            lines.len() <= 3,
            "reference mode should be very compact, got {} lines",
            lines.len()
        );
        assert!(result.contains("lines"), "should contain line count");
        assert!(result.contains("tok"), "should contain token count");
    }

    #[test]
    fn benchmark_task_conditioned_compression() {
        let content = generate_benchmark_code(500);
        let full_tokens = count_tokens(&content);
        let task = Some("fix authentication in validate_token");

        let full_output = process_mode(
            &content,
            "full",
            "F1",
            "server.rs",
            "rs",
            full_tokens,
            CrpMode::Off,
            "server.rs",
            task,
        );
        let task_output = process_mode(
            &content,
            "task",
            "F1",
            "server.rs",
            "rs",
            full_tokens,
            CrpMode::Off,
            "server.rs",
            task,
        );
        let sig_output = process_mode(
            &content,
            "signatures",
            "F1",
            "server.rs",
            "rs",
            full_tokens,
            CrpMode::Off,
            "server.rs",
            task,
        );
        let ref_output = process_mode(
            &content,
            "reference",
            "F1",
            "server.rs",
            "rs",
            full_tokens,
            CrpMode::Off,
            "server.rs",
            task,
        );

        let full_tok = count_tokens(&full_output);
        let task_tok = count_tokens(&task_output);
        let sig_tok = count_tokens(&sig_output);
        let ref_tok = count_tokens(&ref_output);

        eprintln!("\n=== Task-Conditioned Compression Benchmark ===");
        eprintln!("Source: 500-line Rust file, task='fix authentication in validate_token'");
        eprintln!("  full:       {full_tok:>6} tokens (baseline)");
        eprintln!(
            "  task:       {task_tok:>6} tokens ({:.0}% savings)",
            (1.0 - task_tok as f64 / full_tok as f64) * 100.0
        );
        eprintln!(
            "  signatures: {sig_tok:>6} tokens ({:.0}% savings)",
            (1.0 - sig_tok as f64 / full_tok as f64) * 100.0
        );
        eprintln!(
            "  reference:  {ref_tok:>6} tokens ({:.0}% savings)",
            (1.0 - ref_tok as f64 / full_tok as f64) * 100.0
        );
        eprintln!("================================================\n");

        assert!(task_tok < full_tok, "task mode should save tokens");
        assert!(sig_tok < full_tok, "signatures should save tokens");
        assert!(ref_tok < sig_tok, "reference should be most compact");
    }

    fn generate_benchmark_code(lines: usize) -> String {
        let mut code = Vec::with_capacity(lines);
        code.push("use std::collections::HashMap;".to_string());
        code.push("use crate::core::auth;".to_string());
        code.push(String::new());
        code.push("pub struct Server {".to_string());
        code.push("    config: Config,".to_string());
        code.push("    cache: HashMap<String, String>,".to_string());
        code.push("}".to_string());
        code.push(String::new());
        code.push("impl Server {".to_string());
        code.push(
            "    pub fn validate_token(&self, token: &str) -> Result<Claims, AuthError> {"
                .to_string(),
        );
        code.push("        let decoded = auth::decode_jwt(token)?;".to_string());
        code.push("        if decoded.exp < chrono::Utc::now().timestamp() {".to_string());
        code.push("            return Err(AuthError::Expired);".to_string());
        code.push("        }".to_string());
        code.push("        Ok(decoded.claims)".to_string());
        code.push("    }".to_string());
        code.push(String::new());

        let remaining = lines.saturating_sub(code.len());
        for i in 0..remaining {
            if i % 30 == 0 {
                code.push(format!(
                    "    pub fn handler_{i}(&self, req: Request) -> Response {{"
                ));
            } else if i % 30 == 29 {
                code.push("    }".to_string());
            } else {
                code.push(format!("        let val_{i} = self.cache.get(\"key_{i}\").unwrap_or(&\"default\".to_string());"));
            }
        }
        code.push("}".to_string());
        code.join("\n")
    }
}
