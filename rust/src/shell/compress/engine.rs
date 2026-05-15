use crate::core::patterns;
use crate::core::tokens::count_tokens;

use super::classification::{has_structural_output, is_search_output, is_verbatim_output};
use super::footer::shell_savings_footer;

pub(in crate::shell) fn compress_and_measure(
    command: &str,
    stdout: &str,
    stderr: &str,
) -> (String, usize) {
    let compressed_stdout = compress_if_beneficial(command, stdout);
    let compressed_stderr = compress_if_beneficial(command, stderr);

    let mut result = String::new();
    if !compressed_stdout.is_empty() {
        result.push_str(&compressed_stdout);
    }
    if !compressed_stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&compressed_stderr);
    }

    let content_for_counting = if let Some(pos) = result.rfind("\n[lean-ctx: ") {
        &result[..pos]
    } else {
        &result
    };
    let output_tokens = count_tokens(content_for_counting);
    (result, output_tokens)
}

pub(crate) fn compress_if_beneficial(command: &str, output: &str) -> String {
    if output.trim().is_empty() {
        return String::new();
    }

    if !is_search_output(command) && crate::tools::ctx_shell::contains_auth_flow(output) {
        return output.to_string();
    }

    let original_tokens = count_tokens(output);

    if original_tokens < 50 {
        return output.to_string();
    }

    let min_output_tokens = 5;

    let cfg = crate::core::config::Config::load();
    let policy = crate::shell::output_policy::classify(command, &cfg.excluded_commands);
    if policy == crate::shell::output_policy::OutputPolicy::Verbatim
        || policy == crate::shell::output_policy::OutputPolicy::Passthrough
    {
        return truncate_verbatim(output, original_tokens);
    }

    if is_verbatim_output(command) {
        return truncate_verbatim(output, original_tokens);
    }

    if has_structural_output(command) {
        let cl = command.to_ascii_lowercase();
        if let Some(compressed) = patterns::try_specific_pattern(&cl, output) {
            if !compressed.trim().is_empty() {
                let compressed_tokens = count_tokens(&compressed);
                if compressed_tokens >= min_output_tokens && compressed_tokens < original_tokens {
                    return shell_savings_footer(&compressed, original_tokens, compressed_tokens);
                }
            }
        }
        return output.to_string();
    }

    if let Some(mut compressed) = patterns::compress_output(command, output) {
        if !compressed.trim().is_empty() {
            let config = crate::core::config::Config::load();
            let level = crate::core::config::CompressionLevel::effective(&config);
            if level.is_active() {
                let terse_result =
                    crate::core::terse::pipeline::compress(output, &level, Some(&compressed));
                if terse_result.quality_passed {
                    compressed = terse_result.output;
                }
            }

            let compressed_tokens = count_tokens(&compressed);
            if compressed_tokens >= min_output_tokens && compressed_tokens < original_tokens {
                let ratio = compressed_tokens as f64 / original_tokens as f64;
                if ratio < 0.05 && original_tokens > 100 && original_tokens < 2000 {
                    tracing::warn!("compression removed >95% of small output, returning original");
                    return output.to_string();
                }
                return shell_savings_footer(&compressed, original_tokens, compressed_tokens);
            }
            if compressed_tokens < min_output_tokens {
                return output.to_string();
            }
        }
    }

    {
        let config = crate::core::config::Config::load();
        let level = crate::core::config::CompressionLevel::effective(&config);
        if level.is_active() {
            let terse_result = crate::core::terse::pipeline::compress(output, &level, None);
            if terse_result.quality_passed && terse_result.savings_pct >= 3.0 {
                return shell_savings_footer(
                    &terse_result.output,
                    terse_result.tokens_before as usize,
                    terse_result.tokens_after as usize,
                );
            }
        }
    }

    let cleaned = crate::core::compressor::lightweight_cleanup(output);
    let cleaned_tokens = count_tokens(&cleaned);
    if cleaned_tokens < original_tokens {
        let lines: Vec<&str> = cleaned.lines().collect();
        if lines.len() > 30 {
            let compressed = truncate_with_safety_scan(&lines, original_tokens);
            if let Some(c) = compressed {
                return c;
            }
        }
        if cleaned_tokens < original_tokens {
            return shell_savings_footer(&cleaned, original_tokens, cleaned_tokens);
        }
    }

    let lines: Vec<&str> = output.lines().collect();
    if lines.len() > 30 {
        if let Some(c) = truncate_with_safety_scan(&lines, original_tokens) {
            return c;
        }
    }

    output.to_string()
}

const MAX_VERBATIM_TOKENS: usize = 8000;

/// For verbatim commands: never transform content, only head/tail truncate if huge.
fn truncate_verbatim(output: &str, original_tokens: usize) -> String {
    if original_tokens <= MAX_VERBATIM_TOKENS {
        return output.to_string();
    }
    let lines: Vec<&str> = output.lines().collect();
    let total = lines.len();
    if total <= 60 {
        return output.to_string();
    }
    let head = 30.min(total);
    let tail = 20.min(total.saturating_sub(head));
    let omitted = total - head - tail;
    let mut result = String::with_capacity(output.len() / 2);
    for line in &lines[..head] {
        result.push_str(line);
        result.push('\n');
    }
    result.push_str(&format!(
        "\n[{omitted} lines omitted — output too large for context window]\n\n"
    ));
    for line in lines.iter().skip(total - tail) {
        result.push_str(line);
        result.push('\n');
    }
    let truncated_tokens = count_tokens(&result);
    if crate::core::protocol::savings_footer_visible() {
        result.push_str(&format!(
            "[lean-ctx: {original_tokens}→{truncated_tokens} tok, verbatim truncated]"
        ));
    }
    result
}

fn truncate_with_safety_scan(lines: &[&str], original_tokens: usize) -> Option<String> {
    use crate::core::safety_needles;

    let first = &lines[..5];
    let last = &lines[lines.len() - 5..];
    let middle = &lines[5..lines.len() - 5];

    let safety_lines = safety_needles::extract_safety_lines(middle, 20);
    let safety_count = safety_lines.len();
    let omitted = middle.len() - safety_count;

    let mut parts = Vec::new();
    parts.push(first.join("\n"));
    if safety_count > 0 {
        parts.push(format!(
            "[{omitted} lines omitted, {safety_count} safety-relevant lines preserved]"
        ));
        parts.push(safety_lines.join("\n"));
    } else {
        parts.push(format!("[{omitted} lines omitted]"));
    }
    parts.push(last.join("\n"));

    let compressed = parts.join("\n");
    let ct = count_tokens(&compressed);
    if ct >= original_tokens {
        return None;
    }
    Some(shell_savings_footer(&compressed, original_tokens, ct))
}

/// Public wrapper for integration tests to exercise the compression pipeline.
pub fn compress_if_beneficial_pub(command: &str, output: &str) -> String {
    compress_if_beneficial(command, output)
}
