use crate::core::patterns;
use crate::core::tokens::{COUNTING_FAMILY, TokenizerFamily, count_tokens_for};

use super::classification::{
    has_structural_output, is_search_output, is_verbatim_output, looks_like_toon,
};
use super::footer::shell_savings_footer;

pub(in crate::shell) fn compress_and_measure(
    command: &str,
    stdout: &str,
    stderr: &str,
    exit_code: i32,
) -> (String, usize) {
    compress_and_measure_for(command, stdout, stderr, exit_code, COUNTING_FAMILY)
}

pub(in crate::shell) fn compress_and_measure_for(
    command: &str,
    stdout: &str,
    stderr: &str,
    exit_code: i32,
    family: TokenizerFamily,
) -> (String, usize) {
    let compressed_stdout = compress_for_outcome_for(command, stdout, exit_code, family);
    let compressed_stderr = compress_for_outcome_for(command, stderr, exit_code, family);

    let mut result = String::new();
    if !compressed_stdout.is_empty() {
        result.push_str(&compressed_stdout);
    }
    if !compressed_stderr.is_empty() {
        if !result.is_empty() {
            // On failure, label the stderr block so the agent can attribute the
            // error (mirrors `shell::combine_streams`); success keeps the plain
            // join for byte-stable output (#498).
            if exit_code != 0 {
                result.push('\n');
                result.push_str(crate::shell::STDERR_LABEL);
            }
            result.push('\n');
        }
        result.push_str(&compressed_stderr);
    }

    let content_for_counting = if let Some(pos) = result.rfind("\n[lean-ctx: ") {
        &result[..pos]
    } else {
        &result
    };
    let output_tokens = count_tokens_for(content_for_counting, family);
    (result, output_tokens)
}

/// Compress one stream, but never lossily for a command that actually FAILED.
///
/// A non-zero exit keeps the output verbatim (size-capped via [`truncate_verbatim`]
/// with safety-needle-preserving head/tail truncation) so the real error always
/// reaches the model and the agent never has to re-run the command without
/// lean-ctx (#809 / #810). This generalizes the build-tool-error guard inside
/// [`compress_if_beneficial`] to ANY non-zero exit. Empty output and explicit
/// `<lc_safe>` spans keep the normal pipeline (the latter so its markers are
/// stripped correctly); a succeeding command still compresses as before.
pub(crate) fn compress_for_outcome(command: &str, output: &str, exit_code: i32) -> String {
    compress_for_outcome_for(command, output, exit_code, COUNTING_FAMILY)
}

pub(crate) fn compress_for_outcome_for(
    command: &str,
    output: &str,
    exit_code: i32,
    family: TokenizerFamily,
) -> String {
    if exit_code != 0 && !output.trim().is_empty() && !crate::core::protect::has_markers(output) {
        let tokens = count_tokens_for(output, family);
        if tokens <= 2000 {
            return truncate_verbatim(output, tokens, family);
        }
        let compressed = compress_if_beneficial_with_exit(command, output, 0, family);
        let compressed_tokens = count_tokens_for(&compressed, family);
        if compressed_tokens < tokens {
            return compressed;
        }
        return truncate_verbatim(output, tokens, family);
    }
    compress_if_beneficial_with_exit(command, output, exit_code, family)
}

/// Opt-in (#936) lossless crush of a *verbatim* data command's JSON. Returns a
/// savings-footer'd, fully reconstructible reshape only when `enabled` and the
/// crush both pays (at least halves the bytes, via `crush_verbatim`) and clears
/// the output-token floor; otherwise `None`, so the caller keeps the output
/// verbatim. Kept pure (env read stays in the caller) so the gate is unit-tested
/// without mutating the process environment.
pub(crate) fn verbatim_json_crush(
    output: &str,
    original_tokens: usize,
    min_output_tokens: usize,
    enabled: bool,
) -> Option<String> {
    verbatim_json_crush_for(
        output,
        original_tokens,
        min_output_tokens,
        enabled,
        COUNTING_FAMILY,
    )
}

pub(crate) fn verbatim_json_crush_for(
    output: &str,
    original_tokens: usize,
    min_output_tokens: usize,
    enabled: bool,
    family: TokenizerFamily,
) -> Option<String> {
    if !enabled {
        return None;
    }
    let crushed = patterns::json_schema::crush_verbatim(output)?;
    let crushed_tokens = count_tokens_for(&crushed, family);
    (crushed_tokens >= min_output_tokens && crushed_tokens < original_tokens)
        .then(|| shell_savings_footer(&crushed, original_tokens, crushed_tokens))
}

/// Distinct-value ratio at/above which the lossy stage drops an all-present
/// column. Conservative: only near-unique noise (timestamps, UUIDs) is dropped,
/// so genuinely varying-but-meaningful columns are kept.
/// #1129: minimum token savings to justify compression overhead (tee-log pointer
/// + potential read-back round-trip). Below this floor, verbatim is cheaper.
const MIN_USEFUL_SAVINGS: usize = 50;

const LOSSY_DROP_ENTROPY: f64 = 0.9;

/// Opt-in (#936) **lossy** escalation for a verbatim data command's JSON, used
/// only after [`verbatim_json_crush`] (lossless) did not pay. Drops near-unique
/// high-entropy columns and — because data is then lost — persists the verbatim
/// original to the shared CCR store, appending a `ctx_expand` handle so a dropped
/// datum is always recoverable out-of-band (never from the text). Returns `None`
/// unless enabled, the crush both drops a column and clears the token floor, and
/// the original is large enough to persist. The embedded handle is content-
/// addressed, so the rewritten output stays byte-stable across turns (#448/#498).
pub(crate) fn verbatim_json_crush_lossy(
    output: &str,
    original_tokens: usize,
    min_output_tokens: usize,
    enabled: bool,
) -> Option<String> {
    verbatim_json_crush_lossy_for(
        output,
        original_tokens,
        min_output_tokens,
        enabled,
        COUNTING_FAMILY,
    )
}

pub(crate) fn verbatim_json_crush_lossy_for(
    output: &str,
    original_tokens: usize,
    min_output_tokens: usize,
    enabled: bool,
    family: TokenizerFamily,
) -> Option<String> {
    if !enabled {
        return None;
    }
    let res = crate::core::json_crush::crush_text_lossy_if_beneficial(output, LOSSY_DROP_ENTROPY)?;
    let crushed_tokens = count_tokens_for(&res.text, family);
    if crushed_tokens < min_output_tokens || crushed_tokens >= original_tokens {
        return None;
    }
    // Dropped columns must be recoverable out-of-band; bail if we cannot persist
    // (then the lossless/verbatim path keeps the data) rather than lose it.
    let handle = crate::proxy::ccr::persist_json(output)?;
    let body = shell_savings_footer(&res.text, original_tokens, crushed_tokens);
    Some(format!(
        "{body}\n[lean-ctx: high-entropy column(s) dropped — full data at {handle}, \
         ctx_expand(id=\"{handle}\", json_path=\"…\"|search=\"…\") for a slice]"
    ))
}

/// Try the columnar crusher with the comma then the tab delimiter, returning the
/// first that crushes. Shell output carries no file extension, so the delimiter
/// is inferred by trying the two common ones; the crusher self-guards (returns
/// `None` unless the text is a genuinely redundant rectangular table).
fn tabular_delim_crush<T>(output: &str, crush: impl Fn(&str, char) -> Option<T>) -> Option<T> {
    [',', '\t'].into_iter().find_map(|d| crush(output, d))
}

/// Opt-in (#982) lossless crush of a *verbatim* command's delimited (CSV/TSV)
/// output, tried after the JSON crush did not pay. Hoists constant columns via
/// the columnar crusher — fully reconstructible, so no CCR handle is needed.
/// Returns a footer'd reshape only when `enabled` and the crush both pays (at
/// least halves the bytes) and clears the token floor; otherwise `None`.
pub(crate) fn verbatim_tabular_crush_for(
    output: &str,
    original_tokens: usize,
    min_output_tokens: usize,
    enabled: bool,
    family: TokenizerFamily,
) -> Option<String> {
    if !enabled {
        return None;
    }
    let crushed =
        tabular_delim_crush(output, crate::core::tabular_crush::crush_text_if_beneficial)?;
    let crushed_tokens = count_tokens_for(&crushed, family);
    (crushed_tokens >= min_output_tokens && crushed_tokens < original_tokens)
        .then(|| shell_savings_footer(&crushed, original_tokens, crushed_tokens))
}

/// Opt-in (#982) **lossy** escalation for a verbatim command's CSV/TSV output,
/// used only after [`verbatim_tabular_crush_for`] (lossless) did not pay. Drops
/// near-unique high-entropy columns and — because data is then lost — persists
/// the verbatim original to the shared CCR store, appending a `ctx_expand` handle
/// so a dropped datum is always recoverable out-of-band (never from the text).
/// The embedded handle is content-addressed, so the output stays byte-stable
/// across turns (#448/#498).
pub(crate) fn verbatim_tabular_crush_lossy_for(
    output: &str,
    original_tokens: usize,
    min_output_tokens: usize,
    enabled: bool,
    family: TokenizerFamily,
) -> Option<String> {
    if !enabled {
        return None;
    }
    let res = tabular_delim_crush(output, |text, delim| {
        crate::core::tabular_crush::crush_text_lossy_if_beneficial(text, delim, LOSSY_DROP_ENTROPY)
    })?;
    let crushed_tokens = count_tokens_for(&res.text, family);
    if crushed_tokens < min_output_tokens || crushed_tokens >= original_tokens {
        return None;
    }
    let handle = crate::proxy::ccr::persist_tabular(output)?;
    let body = shell_savings_footer(&res.text, original_tokens, crushed_tokens);
    Some(format!(
        "{body}\n[lean-ctx: high-entropy column(s) dropped — full data at {handle}, \
         ctx_expand(id=\"{handle}\", search=\"…\") for a slice]"
    ))
}

/// Opt-in (#985) lossless crush of a *verbatim* command's YAML output (e.g.
/// `kubectl get -o yaml`, `helm get values`), tried after the JSON and tabular
/// crushers did not pay. Maps the document onto the JSON value model and compacts
/// it through the shared crusher — fully reconstructible to the parsed value, so
/// no CCR handle is needed. The crusher self-guards (returns `None` unless the
/// text is a genuinely structured, redundant document). Returns a footer'd
/// reshape only when `enabled` and the crush clears both the reduction gate and
/// the token floor; otherwise `None`.
pub(crate) fn verbatim_yaml_crush_for(
    output: &str,
    original_tokens: usize,
    min_output_tokens: usize,
    enabled: bool,
    family: TokenizerFamily,
) -> Option<String> {
    if !enabled {
        return None;
    }
    let crushed = crate::core::yaml_crush::crush_text_if_beneficial(output)?;
    let crushed_tokens = count_tokens_for(&crushed, family);
    (crushed_tokens >= min_output_tokens && crushed_tokens < original_tokens)
        .then(|| shell_savings_footer(&crushed, original_tokens, crushed_tokens))
}

/// Opt-in (#985) **lossy** escalation for a verbatim command's YAML output, used
/// only after [`verbatim_yaml_crush_for`] (lossless) did not pay. Drops near-unique
/// high-entropy columns and — because data is then lost — persists the verbatim
/// original to the shared CCR store, appending a `ctx_expand` handle so a dropped
/// datum is always recoverable out-of-band (never from the text). The embedded
/// handle is content-addressed, so the output stays byte-stable across turns
/// (#448/#498).
pub(crate) fn verbatim_yaml_crush_lossy_for(
    output: &str,
    original_tokens: usize,
    min_output_tokens: usize,
    enabled: bool,
    family: TokenizerFamily,
) -> Option<String> {
    if !enabled {
        return None;
    }
    let res = crate::core::yaml_crush::crush_text_lossy_if_beneficial(output, LOSSY_DROP_ENTROPY)?;
    let crushed_tokens = count_tokens_for(&res.text, family);
    if crushed_tokens < min_output_tokens || crushed_tokens >= original_tokens {
        return None;
    }
    let handle = crate::proxy::ccr::persist_yaml(output)?;
    let body = shell_savings_footer(&res.text, original_tokens, crushed_tokens);
    Some(format!(
        "{body}\n[lean-ctx: high-entropy column(s) dropped — full data at {handle}, \
         ctx_expand(id=\"{handle}\", search=\"…\") for a slice]"
    ))
}

/// HTML content extractor (#1124): extracts article/main content from web
/// pages and converts to clean markdown. Triggered for HTML content in the
/// verbatim crusher ladder (curl, wget, fetch outputs). The full HTML is
/// persisted to CCR under the `html_` prefix for recovery via `ctx_expand`.
pub(crate) fn verbatim_html_crush_for(
    output: &str,
    original_tokens: usize,
    min_output_tokens: usize,
    enabled: bool,
    family: TokenizerFamily,
) -> Option<String> {
    if !enabled {
        return None;
    }
    let result = crate::core::html_crush::crush_if_beneficial(output)?;
    let crushed_tokens = count_tokens_for(&result.text, family);
    if crushed_tokens < min_output_tokens || crushed_tokens >= original_tokens {
        return None;
    }
    let handle = crate::proxy::ccr::persist_html(output)?;
    let body = shell_savings_footer(&result.text, original_tokens, crushed_tokens);
    Some(format!(
        "{body}\n[lean-ctx: HTML extracted to markdown — full page at {handle}, \
         ctx_expand(id=\"{handle}\", search=\"…\") for a section]"
    ))
}

pub(crate) fn compress_if_beneficial(command: &str, output: &str) -> String {
    compress_if_beneficial_for(command, output, COUNTING_FAMILY)
}

pub(crate) fn compress_if_beneficial_for(
    command: &str,
    output: &str,
    family: TokenizerFamily,
) -> String {
    compress_if_beneficial_with_exit(command, output, -1, family)
}

/// Per-tool minimum token threshold for compression.
/// Test runners and structured commands benefit from compression even at lower
/// token counts because their output has high structural redundancy.
fn min_compression_tokens(command: &str) -> usize {
    let cmd = command.to_ascii_lowercase();
    if cmd.starts_with("cargo test")
        || cmd.starts_with("pytest")
        || cmd.starts_with("python -m pytest")
        || cmd.starts_with("go test")
        || cmd.starts_with("npm test")
        || cmd.starts_with("npx jest")
        || cmd.starts_with("dotnet test")
    {
        return 100;
    }
    if cmd.starts_with("git ") {
        return 80;
    }
    if cmd.starts_with("npm audit") || cmd.starts_with("pip list") || cmd.starts_with("pip install")
    {
        return 100;
    }
    if cmd.starts_with("kubectl ") || cmd.starts_with("docker ") {
        return 100;
    }
    120
}

fn compress_if_beneficial_with_exit(
    command: &str,
    output: &str,
    exit_code: i32,
    family: TokenizerFamily,
) -> String {
    if output.trim().is_empty() {
        return String::new();
    }

    // #709: honour explicit <lc_safe>…</lc_safe> spans. Secret redaction has
    // already run upstream (ctx_shell::handle → redact_shell_output_secrets), so
    // the pipeline order is redact → protect → compress and a marker can never
    // smuggle a secret past redaction. Protected spans pass through verbatim;
    // each unprotected segment flows through the normal pipeline (footer stripped),
    // and a single savings footer is recomputed over the spliced result.
    if crate::core::protect::has_markers(output) {
        let original_tokens = count_tokens_for(output, family);
        let spliced = crate::core::protect::compress_preserving(output, |seg| {
            strip_shell_footer(&compress_if_beneficial_with_exit(
                command, seg, exit_code, family,
            ))
            .to_string()
        });
        let spliced_tokens = count_tokens_for(&spliced, family);
        return if spliced_tokens < original_tokens {
            shell_savings_footer(&spliced, original_tokens, spliced_tokens)
        } else {
            spliced
        };
    }

    // Test-runner output: structurally compress successful runs through the
    // dedicated test-pattern compressors (cargo test, pytest, jest, etc.).
    // Failed runs (exit_code != 0) stay verbatim to preserve failure diagnostics.
    if is_test_runner_command(command) {
        let base = maybe_fold_progress(output, count_tokens_for(output, family), family)
            .unwrap_or_else(|| output.to_string());
        if exit_code == 0
            && let Some(compressed) = patterns::test::compress(&base)
        {
            let original_tokens = count_tokens_for(output, family);
            let compressed_tokens = count_tokens_for(&compressed, family);
            if compressed_tokens < original_tokens {
                return shell_savings_footer(&compressed, original_tokens, compressed_tokens);
            }
        }
        return truncate_verbatim(&base, count_tokens_for(&base, family), family);
    }

    // Compiler errors, type errors, and lint findings must be preserved verbatim
    // so the agent can see file paths, line numbers, and full diagnostics.
    // Warning-only successful builds intentionally fall through to the normal
    // pattern pipeline, where their dedicated compressor can summarize them.
    match classify_build_output(command, output, exit_code) {
        BuildOutputKind::HasErrors => {
            let base = maybe_fold_progress(output, count_tokens_for(output, family), family)
                .unwrap_or_else(|| output.to_string());
            let base = dedup_build_diagnostics(&base);
            return truncate_verbatim(&base, count_tokens_for(&base, family), family);
        }
        BuildOutputKind::WarningsOnly | BuildOutputKind::Clean | BuildOutputKind::NotBuildTool => {}
    }

    if !is_search_output(command) && crate::tools::ctx_shell::contains_auth_flow(output) {
        return output.to_string();
    }

    let original_tokens = count_tokens_for(output, family);

    // #1129: small outputs are cheaper verbatim than compressed + tee-log round-trip.
    // The tee-log pointer alone costs ~50 tokens; a second read-back call doubles
    // the token cost. Outputs below this floor are never worth compressing.
    // Tool-specific thresholds: test runners and structured commands benefit from
    // compression even at lower token counts.
    if original_tokens < min_compression_tokens(command) {
        return output.to_string();
    }

    // #1387: detect record boundaries in multi-record output. When found,
    // compress each segment independently to prevent cross-record content
    // reattribution (e.g. for-loop output with `===== #N` delimiters).
    if let Some(boundary_spans) = super::boundaries::detect_record_boundaries(output) {
        if let Some(result) = super::boundaries::compress_preserving_boundaries(
            command,
            output,
            exit_code,
            family,
            &boundary_spans,
            original_tokens,
        ) {
            return result;
        }
    }

    let min_output_tokens = 15;

    let cfg = crate::core::config::Config::load();
    let policy = crate::shell::output_policy::classify(command, &cfg.excluded_commands);
    if policy == crate::shell::output_policy::OutputPolicy::Verbatim
        || policy == crate::shell::output_policy::OutputPolicy::Passthrough
    {
        // Opt-in (#936): a verbatim *data* command emitting array-heavy JSON
        // (gh api, jq, kubectl get -o json, curl) can be losslessly crushed —
        // reconstructible, never a dropped datum — when it at least halves the
        // payload. Passthrough (auth/dev servers/streaming) is never touched.
        if policy == crate::shell::output_policy::OutputPolicy::Verbatim {
            let enabled = cfg.crush_verbatim_json_enabled();
            // Lossless first (fully reconstructible). Only if it does not pay does
            // the lossy stage drop high-entropy noise — and always behind a CCR
            // handle, so a dropped datum is never irrecoverable (#936).
            if let Some(crushed) =
                verbatim_json_crush_for(output, original_tokens, min_output_tokens, enabled, family)
            {
                return crushed;
            }
            if let Some(crushed) = verbatim_json_crush_lossy_for(
                output,
                original_tokens,
                min_output_tokens,
                enabled,
                family,
            ) {
                return crushed;
            }
            // Non-JSON delimited data (CSV/TSV): same lossless-then-lossy ladder,
            // self-guarding so only a genuinely redundant table is ever reshaped.
            if let Some(crushed) = verbatim_tabular_crush_for(
                output,
                original_tokens,
                min_output_tokens,
                enabled,
                family,
            ) {
                return crushed;
            }
            if let Some(crushed) = verbatim_tabular_crush_lossy_for(
                output,
                original_tokens,
                min_output_tokens,
                enabled,
                family,
            ) {
                return crushed;
            }
            // Structured YAML (kubectl/helm -o yaml): same lossless-then-lossy
            // ladder, self-guarding so only a genuinely structured, redundant
            // document is ever reshaped.
            if let Some(crushed) =
                verbatim_yaml_crush_for(output, original_tokens, min_output_tokens, enabled, family)
            {
                return crushed;
            }
            if let Some(crushed) = verbatim_yaml_crush_lossy_for(
                output,
                original_tokens,
                min_output_tokens,
                enabled,
                family,
            ) {
                return crushed;
            }
            if let Some(crushed) =
                verbatim_html_crush_for(output, original_tokens, min_output_tokens, enabled, family)
            {
                return crushed;
            }
        }
        return truncate_verbatim(output, original_tokens, family);
    }

    // Format-aware passthrough (#342): output already in a compact, token-oriented
    // format the user opted to preserve (TOON by default) is kept verbatim.
    // Recompressing it saves little and rewrites the exact line/field shape an
    // agent relies on to validate a CLI output contract. This is output-shape
    // based, so any tool emitting the format is covered without listing commands.
    if cfg
        .preserve_compact_formats
        .iter()
        .any(|f| f.eq_ignore_ascii_case("toon"))
        && looks_like_toon(output)
    {
        return truncate_verbatim(output, original_tokens, family);
    }

    if is_verbatim_output(command) {
        return truncate_verbatim(output, original_tokens, family);
    }

    // Structural output AND version-control history are owned by their
    // dedicated compressor: apply it if it yields a gain, otherwise return the
    // output verbatim. Never let the generic terse/dedup/truncate fallbacks
    // below reshape it — they would corrupt commit subjects/hashes or drop
    // commits the caller explicitly requested (`git log --oneline -40`).
    if !is_chained_command(command)
        && (has_structural_output(command) || patterns::has_vcs_owner(command))
    {
        let cl = command.to_ascii_lowercase();
        if let Some(compressed) = patterns::try_specific_pattern(&cl, output)
            && !compressed.trim().is_empty()
        {
            let compressed_tokens = count_tokens_for(&compressed, family);
            let savings = original_tokens.saturating_sub(compressed_tokens);
            if compressed_tokens >= min_output_tokens
                && compressed_tokens < original_tokens
                && savings >= MIN_USEFUL_SAVINGS
            {
                return shell_savings_footer(&compressed, original_tokens, compressed_tokens);
            }
        }
        return output.to_string();
    }

    if !is_chained_command(command)
        && let Some(mut compressed) = patterns::compress_output(command, output)
        && !compressed.trim().is_empty()
    {
        let level = crate::core::config::CompressionLevel::effective(&cfg);
        if level.is_active() {
            let terse_result =
                crate::core::terse::pipeline::compress(output, &level, Some(&compressed));
            // #1286: the terse result may only REPLACE the pattern compressor's
            // output when it is actually smaller. It used to win on
            // quality_passed alone — measured: `ls -la` had its ~80%-saving
            // structural compression displaced by a ~6% terse dictionary pass.
            if terse_result.quality_passed
                && count_tokens_for(&terse_result.output, family)
                    < count_tokens_for(&compressed, family)
            {
                compressed = terse_result.output;
            }
        }

        let compressed_tokens = count_tokens_for(&compressed, family);
        let savings = original_tokens.saturating_sub(compressed_tokens);
        if compressed_tokens >= min_output_tokens
            && compressed_tokens < original_tokens
            && savings >= MIN_USEFUL_SAVINGS
        {
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

    {
        let level = crate::core::config::CompressionLevel::effective(&cfg);
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
    let cleaned_tokens = count_tokens_for(&cleaned, family);
    if cleaned_tokens < original_tokens {
        let lines: Vec<&str> = cleaned.lines().collect();
        if lines.len() > 30 {
            let compressed = truncate_with_safety_scan(&lines, original_tokens, family);
            if let Some(c) = compressed {
                return c;
            }
        }
        if cleaned_tokens < original_tokens {
            return shell_savings_footer(&cleaned, original_tokens, cleaned_tokens);
        }
    }

    let lines: Vec<&str> = output.lines().collect();
    if lines.len() > 30
        && let Some(c) = truncate_with_safety_scan(&lines, original_tokens, family)
    {
        return c;
    }

    output.to_string()
}

/// Strip a trailing `\n[lean-ctx: …]` savings footer so per-segment results can
/// be spliced (protect spans, #709) before a single footer is recomputed.
fn strip_shell_footer(s: &str) -> &str {
    match s.rfind("\n[lean-ctx: ") {
        Some(pos) => &s[..pos],
        None => s,
    }
}

/// Detects whether the output contains error diagnostics from a build/check/lint tool.
/// When true, compression is bypassed to preserve file paths, line numbers, and messages.
/// #848: Deduplicate identical diagnostic lines in build output.
/// MSBuild parallel builds print each warning/error twice: once per build-node
/// and once in the summary section. This removes exact duplicates while
/// preserving order (first occurrence wins).
pub(crate) fn dedup_build_diagnostics(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() < 10 {
        return output.to_string();
    }
    let mut seen = std::collections::HashSet::new();
    let mut deduped_count = 0usize;
    let mut result = Vec::with_capacity(lines.len());
    for line in &lines {
        let trimmed = line.trim();
        if is_diagnostic_line(trimmed) {
            let key = normalize_diagnostic_key(trimmed);
            if !seen.insert(key) {
                deduped_count += 1;
                continue;
            }
        }
        result.push(*line);
    }
    if deduped_count > 0 {
        result.push("");
        let note = format!("[lean-ctx: {deduped_count} duplicate diagnostic(s) removed]");
        // We can't push a &str that references a local — collect into owned
        let mut out: String = result.join("\n");
        out.push('\n');
        out.push_str(&note);
        return out;
    }
    output.to_string()
}

/// Recognises MSBuild / compiler diagnostic lines (file(line): warning CS1234: ...)
/// and generic compiler diagnostics (file:line: warning/error: ...).
/// Normalize a diagnostic line for dedup: strip trailing `[project]` suffixes
/// that MSBuild appends per build node, so identical diagnostics from parallel
/// nodes collapse into one.
fn normalize_diagnostic_key(line: &str) -> String {
    let trimmed = line.trim();
    // MSBuild appends ` [/path/to/project.csproj]` — strip it.
    if let Some(bracket) = trimmed.rfind(" [")
        && trimmed.ends_with(']')
    {
        return trimmed[..bracket].to_string();
    }
    trimmed.to_string()
}

fn is_diagnostic_line(line: &str) -> bool {
    if line.is_empty() {
        return false;
    }
    // MSBuild: path(line,col): warning CS1234: message
    // MSBuild: path(line,col): error CS1234: message
    if (line.contains("): warning ") || line.contains("): error ")) && line.contains('(') {
        return true;
    }
    // GCC/Clang/Rust: path:line:col: warning: / error: / note:
    if (line.contains(": warning:") || line.contains(": error:") || line.contains(": warning["))
        && line.contains(':')
    {
        return true;
    }
    false
}

#[derive(Debug, Eq, PartialEq)]
enum BuildOutputKind {
    /// No errors or warnings — clean build.
    Clean,
    /// Only warnings, no errors — can be pattern-compressed.
    WarningsOnly,
    /// Has actual errors — preserve diagnostics verbatim.
    HasErrors,
    /// Not a build tool command.
    NotBuildTool,
}

fn classify_build_output(command: &str, output: &str, exit_code: i32) -> BuildOutputKind {
    let command = strip_env_prefix(command);
    if !is_build_tool(command) {
        return BuildOutputKind::NotBuildTool;
    }
    if exit_code != 0
        || output.contains("error[E")
        || output.contains("error:")
        || output.contains("could not compile")
    {
        return BuildOutputKind::HasErrors;
    }
    if output.contains("warning:") || output.contains("warning[") {
        return BuildOutputKind::WarningsOnly;
    }
    BuildOutputKind::Clean
}

fn is_build_tool(command: &str) -> bool {
    let cmd = command.trim().to_ascii_lowercase();
    cmd.starts_with("cargo check")
        || cmd.starts_with("cargo build")
        || cmd.starts_with("cargo clippy")
        || cmd.starts_with("cargo test")
        || cmd.starts_with("cargo fmt")
        || cmd.starts_with("cargo run")
        || cmd.starts_with("rustc ")
        || cmd.starts_with("gcc ")
        || cmd.starts_with("g++ ")
        || cmd.starts_with("clang ")
        || cmd.starts_with("clang++ ")
        || cmd.starts_with("make ")
        || cmd.starts_with("cmake ")
        || cmd.starts_with("go build")
        || cmd.starts_with("go vet")
        || cmd.starts_with("go test")
        || cmd.starts_with("golangci-lint")
        || cmd.starts_with("tsc ")
        || cmd.starts_with("tsc\t")
        || cmd == "tsc"
        || cmd.starts_with("npx tsc")
        || cmd.starts_with("eslint")
        || cmd.starts_with("npx eslint")
        || cmd.starts_with("biome ")
        || cmd.starts_with("prettier ")
        || cmd.starts_with("mypy ")
        || cmd.starts_with("pyright ")
        || cmd.starts_with("pylint ")
        || cmd.starts_with("ruff check")
        || cmd.starts_with("flake8")
        || cmd.starts_with("black --check")
        || cmd.starts_with("swift build")
        || cmd.starts_with("swiftc ")
        || cmd.starts_with("xcodebuild ")
        || cmd.starts_with("javac ")
        || cmd.starts_with("gradle ")
        || cmd.starts_with("./gradlew ")
        || cmd.starts_with("mvn ")
        || cmd.starts_with("./mvnw ")
        || cmd.starts_with("dotnet build")
        || cmd.starts_with("dotnet test")
        || cmd.starts_with("msbuild")
        || cmd.starts_with("zig build")
        || cmd.starts_with("nim c ")
        || cmd.starts_with("ghc ")
        || cmd.starts_with("stack build")
        || cmd.starts_with("cabal build")
        || cmd.starts_with("mix compile")
        || cmd.starts_with("mix test")
        || cmd.starts_with("mix credo")
        || cmd.starts_with("shellcheck ")
        || cmd.starts_with("hadolint ")
        || cmd.starts_with("terraform validate")
        || cmd.starts_with("terraform plan")
        || cmd.starts_with("ansible-lint")
        || cmd.starts_with("rubocop ")
        || cmd.starts_with("solhint ")
        || cmd.starts_with("slither ")
}

/// Strips leading `VAR=value` environment assignments from a command segment so
/// `RUST_BACKTRACE=1 cargo test` / `CI=true pytest` are still recognized as the
/// underlying test runner.
fn strip_env_prefix(segment: &str) -> &str {
    let mut rest = segment.trim_start();
    loop {
        let Some(first) = rest.split_whitespace().next() else {
            return rest;
        };
        // An env assignment is a single token containing '=' before any '/' so it
        // isn't confused with a path or a flag like `--threads=4`.
        let is_env_assignment = first.contains('=')
            && !first.starts_with('-')
            && first.split('=').next().is_some_and(|name| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
        if !is_env_assignment {
            return rest;
        }
        rest = rest[first.len()..].trim_start();
    }
}

/// Detects test-runner commands across ecosystems. Their output must never be
/// semantically compressed/deduplicated — only verbatim head/tail truncation
/// (with middle test/error lines preserved). Matched even for fully-passing
/// runs so per-suite summaries always survive. Checks each pipeline segment so
/// `cargo test … | grep …` / `pytest … | tail` are caught too.
fn is_test_runner_command(command: &str) -> bool {
    command
        .split('|')
        .map(|seg| strip_env_prefix(seg.trim()).to_ascii_lowercase())
        .any(|seg| {
            seg.starts_with("cargo test")
                || seg.starts_with("cargo nextest")
                || seg.starts_with("nextest")
                || seg.starts_with("pytest")
                || seg.starts_with("python -m pytest")
                || seg.starts_with("python3 -m pytest")
                || seg.starts_with("py.test")
                || seg.starts_with("go test")
                || seg.starts_with("gotestsum")
                || seg.starts_with("npm test")
                || seg.starts_with("npm run test")
                || seg.starts_with("pnpm test")
                || seg.starts_with("pnpm run test")
                || seg.starts_with("yarn test")
                || seg.starts_with("bun test")
                || seg.starts_with("deno test")
                || seg.starts_with("jest")
                || seg.starts_with("npx jest")
                || seg.starts_with("vitest")
                || seg.starts_with("npx vitest")
                || seg.starts_with("mocha")
                || seg.starts_with("npx mocha")
                || seg.starts_with("dotnet test")
                || seg.starts_with("mix test")
                || seg.starts_with("rspec")
                || seg.starts_with("bundle exec rspec")
                || seg.starts_with("phpunit")
                || seg.starts_with("./vendor/bin/phpunit")
                || seg.starts_with("./gradlew test")
                || seg.starts_with("gradle test")
                || seg.starts_with("mvn test")
                || seg.starts_with("ctest")
        })
}

const MAX_VERBATIM_TOKENS: usize = 4000;

/// For verbatim commands: never transform content, only head/tail truncate if huge.
///
/// Even when truncating, every safety- and test-relevant line from the omitted
/// middle is preserved (test-result summaries, panics, failures, errors). This
/// guarantees a large test run — even a fully passing one with dozens of
/// per-suite `test result:` lines — never silently loses its outcome lines,
/// regardless of OS or client (issue: compression must never swallow signal).
fn truncate_verbatim(output: &str, original_tokens: usize, family: TokenizerFamily) -> String {
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
    let middle = &lines[head..total - tail];

    // Preserve up to 200 safety/test/diagnostic lines from the omitted middle so
    // buried failures and per-suite summaries survive head/tail truncation.
    let preserved = crate::core::safety_needles::extract_safety_lines(middle, 200);
    let omitted = middle.len() - preserved.len();

    let mut result = String::with_capacity(output.len() / 2);
    for line in &lines[..head] {
        result.push_str(line);
        result.push('\n');
    }
    if preserved.is_empty() {
        result.push_str(&format!(
            "\n[{omitted} lines omitted — output too large for context window]\n\n"
        ));
    } else {
        result.push_str(&format!(
            "\n[{omitted} lines omitted, {} test/diagnostic lines preserved]\n",
            preserved.len()
        ));
        for line in &preserved {
            result.push_str(line);
            result.push('\n');
        }
        result.push('\n');
    }
    for line in lines.iter().skip(total - tail) {
        result.push_str(line);
        result.push('\n');
    }
    let truncated_tokens = count_tokens_for(&result, family);
    if crate::core::protocol::savings_footer_visible() {
        result.push_str(&format!(
            "[lean-ctx: {original_tokens}→{truncated_tokens} tok, verbatim truncated]"
        ));
    }
    result
}

/// Does this line have the `path:line:` shape every `grep -n` / `rg` match has?
///
/// The prefix must not be all digits, which is what separates a path from a
/// `12:34:56` timestamp in a log — the one common shape that would otherwise
/// read as a match.
fn looks_like_match_line(line: &str) -> bool {
    let Some(first) = line.find(':') else {
        return false;
    };
    let path = &line[..first];
    if path.is_empty()
        || path.bytes().all(|b| b.is_ascii_digit())
        || path.chars().any(char::is_whitespace)
    {
        return false;
    }
    let rest = &line[first + 1..];
    let Some(second) = rest.find(':') else {
        return false;
    };
    second > 0 && rest[..second].bytes().all(|b| b.is_ascii_digit())
}

/// Is this output a search result rather than a log?
fn is_match_shaped(lines: &[&str]) -> bool {
    let considered: Vec<&&str> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
    if considered.len() < 20 {
        return false;
    }
    let matched = considered
        .iter()
        .filter(|l| looks_like_match_line(l))
        .count();
    matched * 10 >= considered.len() * 9
}

/// GH #1663: search results are not logs, and must never be sampled.
///
/// Head+tail sampling assumes the interesting content sits at the edges. For a
/// search result every line is a discrete answer and the decisive one is at an
/// arbitrary position — usually the *unusual* one, the single reader among two
/// hundred writers. Sampling is biased against exactly the line that matters,
/// and `[222 lines omitted]` reads as "more of the same" rather than "the
/// answer may be in here". A reporter concluded a struct field was dead code on
/// a sampled result whose dropped middle held the line proving it was read.
///
/// So: keep a contiguous prefix, in order, and say plainly that this is a
/// sample and how many matches were not shown. The full set stays reachable
/// through the archive reference the caller already gets.
fn truncate_match_lines(
    lines: &[&str],
    original_tokens: usize,
    family: TokenizerFamily,
) -> Option<String> {
    const SHOWN: usize = 40;
    if lines.len() <= SHOWN {
        return None;
    }
    let total = lines.len();
    let hidden = total - SHOWN;

    let mut compressed = lines[..SHOWN].join("\n");
    compressed.push_str(&format!(
        "\n[⚠ SAMPLE — showing the first {SHOWN} of {total} matching lines, in order; \
         {hidden} not shown. This is NOT a complete result set: nothing here rules \
         out a match further down. Narrow the pattern or use ctx_search before \
         concluding anything from absence.]"
    ));

    let ct = count_tokens_for(&compressed, family);
    if ct >= original_tokens {
        return None;
    }
    Some(shell_savings_footer(&compressed, original_tokens, ct))
}

fn truncate_with_safety_scan(
    lines: &[&str],
    original_tokens: usize,
    family: TokenizerFamily,
) -> Option<String> {
    use crate::core::safety_needles;

    if is_match_shaped(lines) {
        return truncate_match_lines(lines, original_tokens, family);
    }

    let first = &lines[..5];
    let last = &lines[lines.len() - 5..];
    let middle = &lines[5..lines.len() - 5];

    let safety_lines = safety_needles::extract_safety_lines(middle, 80);
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
    let ct = count_tokens_for(&compressed, family);
    if ct >= original_tokens {
        return None;
    }
    Some(shell_savings_footer(&compressed, original_tokens, ct))
}

fn fold_repetitive_progress(output: &str) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    let mut pending_kind: Option<ProgressKind> = None;
    let mut pending: Vec<&str> = Vec::new();
    let mut omitted_low_signal = 0usize;

    for line in output.lines() {
        if is_low_signal_progress(line) {
            omitted_low_signal += 1;
            continue;
        }

        let kind = classify_foldable_progress(line);
        if kind.is_some() && kind == pending_kind {
            pending.push(line);
            continue;
        }

        flush_progress_run(&mut out, pending_kind, &pending);
        pending.clear();
        pending_kind = kind;
        if kind.is_some() {
            pending.push(line);
        } else {
            out.push(line.to_string());
        }
    }

    flush_progress_run(&mut out, pending_kind, &pending);
    if omitted_low_signal > 0 {
        out.push(format!(
            "[{omitted_low_signal} low-signal progress lines omitted]"
        ));
    }

    let folded = out.join("\n") + "\n";
    (folded.len() < output.len()).then_some(folded)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProgressKind {
    CargoCompile,
    CargoFresh,
    CargoTestOk,
    PytestPassed,
    NpmProgress,
}

fn classify_foldable_progress(line: &str) -> Option<ProgressKind> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("Compiling ") || trimmed.starts_with("Checking ") {
        return Some(ProgressKind::CargoCompile);
    }
    if trimmed.starts_with("Fresh ")
        || trimmed.starts_with("Downloaded ")
        || trimmed.starts_with("Downloading ")
    {
        return Some(ProgressKind::CargoFresh);
    }
    if trimmed.starts_with("test ") && trimmed.ends_with("... ok") {
        return Some(ProgressKind::CargoTestOk);
    }
    if line.contains(" PASSED [") {
        return Some(ProgressKind::PytestPassed);
    }
    if trimmed.starts_with('[') && trimmed.contains('%') && trimmed.contains('/') {
        return Some(ProgressKind::NpmProgress);
    }
    None
}

/// Pure dot-run lines (pytest/unittest progress) of any length. Lines with any
/// other character (e.g. `....F...`, which encodes a failure) are never folded.
fn is_low_signal_progress(line: &str) -> bool {
    let trimmed = line.trim();
    !trimmed.is_empty() && trimmed.bytes().all(|b| b == b'.')
}

fn flush_progress_run(out: &mut Vec<String>, kind: Option<ProgressKind>, lines: &[&str]) {
    let Some(kind) = kind else {
        return;
    };
    if lines.is_empty() {
        return;
    }
    let threshold = match kind {
        ProgressKind::CargoCompile | ProgressKind::PytestPassed | ProgressKind::CargoTestOk => 3,
        ProgressKind::CargoFresh | ProgressKind::NpmProgress => 5,
    };
    if lines.len() < threshold {
        out.extend(lines.iter().map(|line| (*line).to_string()));
        return;
    }

    out.push(format!(
        "[{} {} lines folded]",
        lines.len(),
        match kind {
            ProgressKind::CargoCompile => "cargo compile/check",
            ProgressKind::CargoFresh => "cargo download/fresh",
            ProgressKind::CargoTestOk => "cargo test ok",
            ProgressKind::PytestPassed => "pytest PASSED",
            ProgressKind::NpmProgress => "package-manager progress",
        }
    ));
    out.push(lines.first().unwrap_or(&"").to_string());
    if lines.len() > 2 {
        out.push("…".to_string());
    }
    if lines.len() > 1 {
        out.push(lines.last().unwrap_or(&"").to_string());
    }
}

fn maybe_fold_progress(
    output: &str,
    original_tokens: usize,
    family: TokenizerFamily,
) -> Option<String> {
    let folded = fold_repetitive_progress(output)?;
    (count_tokens_for(&folded, family) < original_tokens).then_some(folded)
}

/// Detects shell command chains (`&&`, `;`) outside of quotes.
///
/// Chained commands must skip pattern-based compression because each pattern
/// matcher assumes a single command's output shape — applying the first
/// segment's pattern to the whole chain silently drops later segments (#1130).
pub(crate) fn is_chained_command(command: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = command.as_bytes();
    let len = bytes.len();
    for i in 0..len {
        match bytes[i] {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'&' if !in_single && !in_double && i + 1 < len && bytes[i + 1] == b'&' => {
                return true;
            }
            b';' if !in_single && !in_double => return true,
            _ => {}
        }
    }
    false
}

pub fn compress_if_beneficial_pub(command: &str, output: &str) -> String {
    compress_if_beneficial_pub_for(command, output, COUNTING_FAMILY)
}

pub(crate) fn compress_if_beneficial_pub_for(
    command: &str,
    output: &str,
    family: TokenizerFamily,
) -> String {
    compress_if_beneficial_for(command, output, family)
}

/// Preserve build/test output verbatim, applying only the safety-line-preserving
/// head/tail truncation when it is oversized.
///
/// The proxy funnel uses this when a foreign shell tool produced unmistakable
/// build/test output but supplied no recognizable command — the engine's
/// command-gated verbatim guards cannot fire, yet compiler errors, panics and
/// test summaries must still reach the model intact for a bug-fix task.
pub(crate) fn preserve_verbatim_pub_for(output: &str, family: TokenizerFamily) -> String {
    truncate_verbatim(output, count_tokens_for(output, family), family)
}

#[cfg(test)]
mod tests {
    use super::{BuildOutputKind, classify_build_output};

    #[test]
    fn test_classify_clean_build() {
        assert_eq!(
            classify_build_output("cargo build", "Finished dev profile", 0),
            BuildOutputKind::Clean
        );
    }

    #[test]
    fn test_classify_warnings_only() {
        assert_eq!(
            classify_build_output("cargo check", "warning: unused variable", 0),
            BuildOutputKind::WarningsOnly
        );
    }

    #[test]
    fn test_classify_has_errors() {
        assert_eq!(
            classify_build_output("cargo build", "error[E0308]: mismatched types", 1),
            BuildOutputKind::HasErrors
        );
    }

    #[test]
    fn test_classify_exit_nonzero() {
        assert_eq!(
            classify_build_output("cargo clippy", "lint output", 1),
            BuildOutputKind::HasErrors
        );
    }

    #[test]
    fn test_classify_with_env_prefix() {
        assert_eq!(
            classify_build_output("RUST_LOG=debug cargo build", "warning: unused variable", 0),
            BuildOutputKind::WarningsOnly
        );
    }
}

#[cfg(test)]
mod gh1663 {
    use super::*;

    /// 236 grep matches, the decisive one at 205 — the reporter's real case.
    fn grep_output() -> Vec<String> {
        let mut v: Vec<String> = (0..236)
            .map(|i| format!("./interp/typecheck.go:{}:\t\t\tc0 = c0.child[0]", 90 + i))
            .collect();
        v[204] = "./interp/debugger.go:720:\tfor _, ch := range sc.child {".to_string();
        v
    }

    #[test]
    fn grep_shaped_output_is_never_sampled_head_and_tail() {
        let owned = grep_output();
        let lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        assert!(is_match_shaped(&lines), "precondition: reads as matches");

        let out = truncate_with_safety_scan(&lines, 10_000, TokenizerFamily::Cl100k)
            .expect("output is long enough to compress");

        assert!(
            !out.contains("safety-relevant lines preserved"),
            "the head+tail sampler must not run on search results: {out}"
        );
        assert!(
            out.contains("SAMPLE"),
            "the sample must announce itself: {out}"
        );
        assert!(
            out.contains("of 236 matching lines"),
            "and name the true match count: {out}"
        );
    }

    /// What is shown is a contiguous, ordered prefix — so "not in the output"
    /// can never be mistaken for "not in the results".
    #[test]
    fn the_shown_matches_are_a_contiguous_prefix() {
        let owned = grep_output();
        let lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        let out = truncate_with_safety_scan(&lines, 10_000, TokenizerFamily::Cl100k).unwrap();

        let body: Vec<&str> = out
            .lines()
            .take_while(|l| !l.starts_with("[⚠ SAMPLE"))
            .collect();
        for (i, line) in body.iter().enumerate() {
            assert_eq!(*line, lines[i], "line {i} is out of order or substituted");
        }
    }

    /// A log with timestamps must keep the sampler: `12:34:56` is not a path.
    #[test]
    fn timestamped_logs_are_not_mistaken_for_matches() {
        let owned: Vec<String> = (0..120)
            .map(|i| format!("12:34:{:02} INFO worker {i} still running", i % 60))
            .collect();
        let lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        assert!(!is_match_shaped(&lines), "a timestamp is not a path");
    }

    /// A short result set is delivered whole — no notice, no sampling.
    #[test]
    fn a_small_match_set_is_not_truncated_at_all() {
        let owned: Vec<String> = (0..25)
            .map(|i| format!("src/main.rs:{i}:    let x = {i};"))
            .collect();
        let lines: Vec<&str> = owned.iter().map(String::as_str).collect();
        assert!(is_match_shaped(&lines));
        assert!(truncate_match_lines(&lines, 10_000, TokenizerFamily::Cl100k).is_none());
    }
}
