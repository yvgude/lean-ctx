//! Output rendering: full-output framing, header building, per-mode
//! processing, task-relevant filtering and line-range extraction.
//! Split out of `ctx_read/mod.rs`; `use super::*` re-imports parent items.

#[allow(clippy::wildcard_imports)]
use super::*;
use crate::core::aggressiveness::AggressivenessProfile;

/// Per-read tuning threaded into the per-mode renderers. `Default` reproduces
/// the behaviour from before the aggressiveness knob existed (no override), so
/// every existing caller and test keeps its exact byte output (#498).
#[derive(Clone, Copy, Default)]
pub(crate) struct ReadTuning<'a> {
    /// Resolved 0.0–1.0 compression intensity, or `None` to use each mode's
    /// built-in default. Already resolved via `aggressiveness::effective` at the
    /// read boundary, so the renderer treats it as authoritative.
    pub aggressiveness: Option<f64>,
    /// Explicit `protect` tokens (#709): every line containing one of these
    /// survives the line-based lossy filters (entropy / information-bottleneck)
    /// verbatim. Empty slice reproduces the pre-protect byte output (#498).
    /// Borrowed from the read boundary for the duration of the render call.
    pub protect: &'a [String],
}

impl<'a> ReadTuning<'a> {
    /// Resolves the effective tuning from an explicit per-call aggressiveness
    /// (falling back to the `LEAN_CTX_AGGRESSIVENESS` env var / config field) and
    /// the explicit `protect` token list.
    pub(crate) fn resolve(explicit_aggressiveness: Option<f64>, protect: &'a [String]) -> Self {
        Self {
            aggressiveness: crate::core::aggressiveness::effective(explicit_aggressiveness),
            protect,
        }
    }

    /// For an `auto` read, the `density:` mode an aggressiveness setting maps to
    /// (so one knob drives whole-file intensity via the proven density path).
    pub(crate) fn auto_density_mode(self) -> Option<String> {
        self.aggressiveness.map(|a| {
            format!(
                "density:{:.2}",
                AggressivenessProfile::from_level(a).density_target
            )
        })
    }
}

/// Render a trailing ` [a, b]` techniques tag, or `""` when no compression
/// technique fired. Avoids the empty ` []` metadata field a bare `join` would
/// leave on an incompressible file (#509 output-waste audit, same class as the
/// `ctx_semantic_search` `(rrf: X, )` fix in #511).
fn techniques_tag(techniques: &[String]) -> String {
    if techniques.is_empty() {
        String::new()
    } else {
        format!(" [{}]", techniques.join(", "))
    }
}

/// Code-health annotations (function name → note like `cc=18`) for the
/// over-threshold functions in `content`, honoring `[code_health].annotate_reads`.
/// Empty when disabled, unsupported, or nothing qualifies. Deterministic
/// (#498-safe): a pure function of content + the active threshold.
fn health_annotations(content: &str, ext: &str) -> std::collections::HashMap<String, String> {
    let cfg = crate::core::config::Config::load();
    if !cfg.code_health.annotate_reads {
        return std::collections::HashMap::new();
    }
    let annotations = crate::core::code_health::annotate::annotations_for_file(
        content,
        ext,
        cfg.code_health.cognitive_threshold,
    );
    crate::core::code_health::annotate::by_name(&annotations)
}

pub(crate) fn format_full_output(
    file_ref: &str,
    short: &str,
    ext: &str,
    content: &str,
    original_tokens: usize,
    line_count: usize,
    _task: Option<&str>,
) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new("full");
    let tokens = original_tokens;
    let metadata = build_header(file_ref, short, ext, content, line_count, true);

    let output = format!("{metadata}\n{content}");
    let sent = count_tokens(&output);
    (protocol::append_savings(&output, tokens, sent), sent)
}

/// Render `content` with per-line `N:hh|` hash anchors for `ctx_patch`
/// (mode=anchored, epic #1008). The body is verbatim source prefixed with a
/// stable, self-describing one-line legend so a vanilla agent can read the
/// format without prior knowledge (GL#580 self-describing-output philosophy).
///
/// #498 determinism: the output is a pure function of `(file_ref, short,
/// content, line_count)` — no savings footer, timestamps or counters — so
/// identical re-reads stay byte-stable and provider prompt caching applies. No
/// `append_savings`: anchors are lossless *additions*, so a savings line would
/// always be negative and misleading.
pub(crate) fn format_anchored_output(
    file_ref: &str,
    short: &str,
    content: &str,
    line_count: usize,
) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new("anchored");
    let header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
        format!("{file_ref}={short} {line_count}L [anchored: N:hh|line → edit via ctx_patch]")
    } else {
        format!("{short} {line_count}L [anchored: N:hh|line → edit via ctx_patch]")
    };
    let annotated = crate::core::anchor::annotate(content, 1);
    let output = format!("{header}\n{annotated}");
    let sent = count_tokens(&output);
    (output, sent)
}

pub(crate) fn build_header(
    file_ref: &str,
    short: &str,
    ext: &str,
    content: &str,
    line_count: usize,
    include_deps: bool,
) -> String {
    let mut header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
        format!("{file_ref}={short} {line_count}L")
    } else {
        format!("{short} {line_count}L")
    };

    if include_deps {
        let dep_info = deps::extract_deps(content, ext);
        if !dep_info.imports.is_empty() {
            let imports_str: Vec<&str> = dep_info
                .imports
                .iter()
                .take(8)
                .map(std::string::String::as_str)
                .collect();
            header.push_str(&format!("\n deps {}", imports_str.join(",")));
        }
        if !dep_info.exports.is_empty() {
            let exports_str: Vec<&str> = dep_info
                .exports
                .iter()
                .take(8)
                .map(std::string::String::as_str)
                .collect();
            header.push_str(&format!("\n exports {}", exports_str.join(",")));
        }
    }

    header
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_mode(
    content: &str,
    mode: &str,
    file_ref: &str,
    short: &str,
    ext: &str,
    original_tokens: usize,
    crp_mode: CrpMode,
    file_path: &str,
    task: Option<&str>,
) -> (String, usize) {
    process_mode_tuned(
        content,
        mode,
        file_ref,
        short,
        ext,
        original_tokens,
        crp_mode,
        file_path,
        task,
        ReadTuning::default(),
    )
}

/// Renders `content` for `mode`, honouring the aggressiveness knob carried in
/// `tuning`. `process_mode` is the unchanged-behaviour wrapper (`ReadTuning::
/// default()`); the real read path threads a resolved `tuning` through here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_mode_tuned(
    content: &str,
    mode: &str,
    file_ref: &str,
    short: &str,
    ext: &str,
    original_tokens: usize,
    crp_mode: CrpMode,
    file_path: &str,
    task: Option<&str>,
    tuning: ReadTuning<'_>,
) -> (String, usize) {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new(mode);
    let line_count = content.lines().count();

    match mode {
        "raw" => {
            let sent = count_tokens(content);
            (content.to_string(), sent)
        }
        "auto" => {
            // The aggressiveness knob routes `auto` through the density path so a
            // single number drives whole-file intensity; otherwise the learned
            // auto-resolver picks the mode.
            let chosen = tuning
                .auto_density_mode()
                .unwrap_or_else(|| resolve_auto_mode(None, file_path, original_tokens, task));
            process_mode_tuned(
                content,
                &chosen,
                file_ref,
                short,
                ext,
                original_tokens,
                crp_mode,
                file_path,
                task,
                tuning,
            )
        }
        "full" => format_full_output(
            file_ref,
            short,
            ext,
            content,
            original_tokens,
            line_count,
            task,
        ),
        "anchored" => format_anchored_output(file_ref, short, content, line_count),
        "signatures" => {
            let sigs = signatures::extract_signatures(content, ext);
            let dep_info = deps::extract_deps(content, ext);

            let mut output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!("{file_ref}={short} {line_count}L")
            } else {
                format!("{short} {line_count}L")
            };
            if !dep_info.imports.is_empty() {
                let imports_str: Vec<&str> = dep_info
                    .imports
                    .iter()
                    .take(8)
                    .map(std::string::String::as_str)
                    .collect();
                output.push_str(&format!("\n deps {}", imports_str.join(",")));
            }
            // Self-describing outputs (GL #580): symbol notation always ships
            // its own one-line legend so vanilla agents can read it.
            if crp_mode.is_tdd() {
                let refs: Vec<&signatures::Signature> = sigs.iter().collect();
                let legend = signatures::tdd_legend(&refs);
                if !legend.is_empty() {
                    output.push('\n');
                    output.push_str(&legend);
                }
            }
            let health = health_annotations(content, ext);
            for sig in &sigs {
                output.push('\n');
                if crp_mode.is_tdd() {
                    output.push_str(&sig.to_tdd_located());
                } else {
                    output.push_str(&sig.to_compact_located());
                }
                if let Some(note) = health.get(&sig.name) {
                    output.push_str("  ");
                    output.push_str(note);
                }
            }
            if let Some(body) = task_relevant_body(content, file_path, ext, task) {
                output.push('\n');
                output.push_str(&body);
            }
            // JIT disclosure (GL#447): signatures carry L-spans, so point at the
            // targeted range expansion before the full-read escalation.
            if crate::core::profiles::active_profile()
                .output_hints
                .compressed_hint()
                && !sigs.is_empty()
            {
                output.push_str(&format!(
                    "\n  ↳ expand a symbol: ctx_read(\"{file_path}\", mode=\"lines:N-M\") using the spans above"
                ));
            }
            let sent = count_tokens(&output);
            (
                append_compressed_hint(
                    &protocol::append_savings(&output, original_tokens, sent),
                    file_path,
                ),
                sent,
            )
        }
        "map" => {
            if ext == "php"
                && let Some(php_map) = crate::core::patterns::php::compress_php_map(content, short)
            {
                let output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                    format!("{file_ref}={short} {line_count}L\n{php_map}")
                } else {
                    format!("{short} {line_count}L\n{php_map}")
                };
                let sent = count_tokens(&output);
                let output = protocol::append_savings(&output, original_tokens, sent);
                return (append_compressed_hint(&output, file_path), sent);
            }

            let structured = match ext {
                "md" | "mdx" | "rst" => {
                    crate::core::structured_read::extract_markdown_outline(content)
                }
                "json" => crate::core::structured_read::extract_json_structure(content),
                "yaml" | "yml" => crate::core::structured_read::extract_yaml_structure(content),
                "toml" => crate::core::structured_read::extract_toml_structure(content),
                _ if file_path.to_lowercase().ends_with(".lock")
                    || file_path.to_lowercase().ends_with("go.sum") =>
                {
                    crate::core::structured_read::extract_lock_summary(content, file_path)
                }
                _ => String::new(),
            };

            if !structured.is_empty() {
                let mut output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                    format!("{file_ref}={short} {line_count}L\n{structured}")
                } else {
                    format!("{short} {line_count}L\n{structured}")
                };
                let sent = count_tokens(&output);
                output = protocol::append_savings(&output, original_tokens, sent);
                return (append_compressed_hint(&output, file_path), sent);
            }

            let sigs = signatures::extract_signatures(content, ext);
            let dep_info = deps::extract_deps(content, ext);

            let mut output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!("{file_ref}={short} {line_count}L")
            } else {
                format!("{short} {line_count}L")
            };

            if !dep_info.imports.is_empty() {
                output.push_str("\n  deps: ");
                output.push_str(&dep_info.imports.join(", "));
            }

            let key_sigs: Vec<&signatures::Signature> = sigs
                .iter()
                .filter(|s| s.is_exported || s.indent == 0)
                .collect();

            // Drop exports the API section already lists with full signatures
            // (pure redundant tokens in map mode, #361).
            let extra_exports = signatures::exports_not_in_signatures(&dep_info.exports, &key_sigs);
            if !extra_exports.is_empty() {
                output.push_str("\n  exports: ");
                output.push_str(&extra_exports.join(", "));
            }

            if !key_sigs.is_empty() {
                output.push_str("\n  API:");
                // Self-describing outputs (GL #580): legend precedes symbols.
                if crp_mode.is_tdd() {
                    let legend = signatures::tdd_legend(&key_sigs);
                    if !legend.is_empty() {
                        output.push_str(&format!(" {legend}"));
                    }
                }
                let health = health_annotations(content, ext);
                for sig in &key_sigs {
                    output.push_str("\n    ");
                    if crp_mode.is_tdd() {
                        output.push_str(&sig.to_tdd_located());
                    } else {
                        output.push_str(&sig.to_compact_located());
                    }
                    if let Some(note) = health.get(&sig.name) {
                        output.push_str("  ");
                        output.push_str(note);
                    }
                }
            }

            if let Some(body) = task_relevant_body(content, file_path, ext, task) {
                output.push('\n');
                output.push_str(&body);
            }

            let sent = count_tokens(&output);
            (
                append_compressed_hint(
                    &protocol::append_savings(&output, original_tokens, sent),
                    file_path,
                ),
                sent,
            )
        }
        "aggressive" => {
            // Structured JSON (#936): a redundant array-of-objects compacts far
            // better — and losslessly — through the shared `json_crush` core than
            // generic text pruning, which mangles structure. Fires only when it
            // at least halves the file and shrinks the token count; the exact
            // bytes stay recoverable via a `full`/`raw` re-read.
            if ext == "json"
                && let Some(crushed) = crate::core::json_crush::crush_text_if_beneficial(content)
            {
                let header = build_header(file_ref, short, ext, content, line_count, true);
                let body = format!("{header}\n{crushed}");
                let sent = count_tokens(&body);
                if sent < original_tokens {
                    let savings = protocol::format_savings(original_tokens, sent);
                    return (
                        append_compressed_hint(&format!("{body}\n{savings}"), file_path),
                        sent,
                    );
                }
            }

            // Tabular data (CSV/TSV, #982): a redundant table hoists its constant
            // columns once through the columnar crusher (lossless); the exact
            // bytes stay recoverable via a `full`/`raw` re-read.
            if let Some(delim) = compressor::tabular_delimiter(Some(ext))
                && let Some(crushed) =
                    crate::core::tabular_crush::crush_text_if_beneficial(content, delim)
            {
                let header = build_header(file_ref, short, ext, content, line_count, true);
                let body = format!("{header}\n{crushed}");
                let sent = count_tokens(&body);
                if sent < original_tokens {
                    let savings = protocol::format_savings(original_tokens, sent);
                    return (
                        append_compressed_hint(&format!("{body}\n{savings}"), file_path),
                        sent,
                    );
                }
            }

            // YAML (#985): a verbose document compacts losslessly to compact JSON
            // through the shared crusher (formatting dropped, redundant
            // `items`/`list` arrays factored); the exact bytes stay recoverable
            // via a `full`/`raw` re-read.
            if compressor::is_yaml_ext(Some(ext))
                && let Some(crushed) = crate::core::yaml_crush::crush_text_if_beneficial(content)
            {
                let header = build_header(file_ref, short, ext, content, line_count, true);
                let body = format!("{header}\n{crushed}");
                let sent = count_tokens(&body);
                if sent < original_tokens {
                    let savings = protocol::format_savings(original_tokens, sent);
                    return (
                        append_compressed_hint(&format!("{body}\n{savings}"), file_path),
                        sent,
                    );
                }
            }

            #[cfg(feature = "tree-sitter")]
            let ast_pruned = crate::core::signatures_ts::ast_prune(content, ext);
            #[cfg(not(feature = "tree-sitter"))]
            let ast_pruned: Option<String> = None;

            let base = ast_pruned.as_deref().unwrap_or(content);

            let session_intent = crate::core::session::SessionState::load_latest()
                .and_then(|s| s.active_structured_intent);
            let raw = if let Some(ref intent) = session_intent {
                compressor::task_aware_compress(base, Some(ext), intent)
            } else {
                compressor::aggressive_compress(base, Some(ext))
            };
            let compressed = compressor::safeguard_ratio(content, &raw);
            let header = build_header(file_ref, short, ext, content, line_count, true);

            let mut sym = SymbolMap::new();
            let idents = symbol_map::extract_identifiers(&compressed, &[ext]);
            for ident in &idents {
                sym.register(ident);
            }

            if symbol_map::substitution_enabled() && sym.len() >= 3 {
                let sym_table = sym.format_table();
                let sym_applied = sym.apply(&compressed);
                let orig_tok = count_tokens(&compressed);
                let comp_tok = count_tokens(&sym_applied) + count_tokens(&sym_table);
                let net = orig_tok.saturating_sub(comp_tok);
                if orig_tok > 0 && net * 100 / orig_tok >= 5 {
                    let savings = protocol::format_savings(original_tokens, comp_tok);
                    return (
                        append_compressed_hint(
                            &format!("{header}\n{sym_applied}{sym_table}\n{savings}"),
                            file_path,
                        ),
                        comp_tok,
                    );
                }
                let savings = protocol::format_savings(original_tokens, orig_tok);
                return (
                    append_compressed_hint(
                        &format!("{header}\n{compressed}\n{savings}"),
                        file_path,
                    ),
                    orig_tok,
                );
            }

            let sent = count_tokens(&compressed);
            let savings = protocol::format_savings(original_tokens, sent);
            (
                append_compressed_hint(&format!("{header}\n{compressed}\n{savings}"), file_path),
                sent,
            )
        }
        "entropy" => {
            // Query-conditioned IB (#542) — relevance source chain: explicit
            // task param > active session intent > last semantic-search query.
            let task_kws: Vec<String> = task
                .filter(|t| !t.trim().is_empty())
                .map(|t| crate::core::task_relevance::parse_task_hints(t).1)
                .filter(|kws| !kws.is_empty())
                .or_else(|| {
                    let session = crate::core::session::SessionState::load_latest()?;
                    if let Some(intent) = session.active_structured_intent
                        && !intent.keywords.is_empty()
                    {
                        return Some(intent.keywords);
                    }
                    let q = session.last_semantic_query?;
                    let kws = crate::core::task_relevance::parse_task_hints(&q).1;
                    (!kws.is_empty()).then_some(kws)
                })
                .unwrap_or_default();
            let result = match (task_kws.is_empty(), tuning.aggressiveness) {
                // Aggressiveness overrides the learned BPE-entropy threshold for
                // the plain (no task keywords) path; task-conditioned entropy
                // keeps its own relevance-aware thresholds.
                (true, Some(a)) => entropy::entropy_compress_with_threshold(
                    content,
                    file_path,
                    AggressivenessProfile::from_level(a).bpe_entropy,
                    tuning.protect,
                ),
                (true, None) => {
                    entropy::entropy_compress_adaptive(content, file_path, tuning.protect)
                }
                (false, _) => entropy::entropy_compress_task_conditioned(
                    content,
                    file_path,
                    &task_kws,
                    tuning.protect,
                ),
            };
            let avg_h = entropy::analyze_entropy(content).avg_entropy;
            let header = build_header(file_ref, short, ext, content, line_count, false);
            let output = format!(
                "{header} H̄={avg_h:.1}{}\n{}",
                techniques_tag(&result.techniques),
                result.output
            );
            let sent = count_tokens(&output);
            let savings = protocol::format_savings(original_tokens, sent);
            let compression_ratio = if original_tokens > 0 {
                1.0 - (sent as f64 / original_tokens as f64)
            } else {
                0.0
            };
            crate::core::adaptive_thresholds::report_bandit_outcome_for_path(
                file_path,
                compression_ratio > 0.15,
            );
            (
                append_compressed_hint(&format!("{output}\n{savings}"), file_path),
                sent,
            )
        }
        "task" => {
            let task_str = task.unwrap_or("");
            if task_str.is_empty() {
                let header = build_header(file_ref, short, ext, content, line_count, true);
                let out = format!("{header}\n{content}\n[task mode: no task set — returned full]");
                let sent = count_tokens(&out);
                return (out, sent);
            }
            let (_files, keywords) = crate::core::task_relevance::parse_task_hints(task_str);
            if keywords.is_empty() {
                let header = build_header(file_ref, short, ext, content, line_count, true);
                let out = format!(
                    "{header}\n{content}\n[task mode: no keywords extracted — returned full]"
                );
                let sent = count_tokens(&out);
                return (out, sent);
            }
            // Aggressiveness tightens the IB keep-budget; default 0.3 preserved
            // when the knob is unset.
            let ib_budget = tuning.aggressiveness.map_or(0.3, |a| {
                AggressivenessProfile::from_level(a).ib_budget_ratio
            });
            let filtered = crate::core::task_relevance::information_bottleneck_filter(
                content,
                &keywords,
                ib_budget,
                tuning.protect,
            );
            let filtered_lines = filtered.lines().count();
            let header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!(
                    "{file_ref}={short} {line_count}L [task-filtered: {line_count}→{filtered_lines}]"
                )
            } else {
                format!("{short} {line_count}L [task-filtered: {line_count}→{filtered_lines}]")
            };
            let graph_ctx = if crate::core::profiles::active_profile()
                .output_hints
                .graph_context_block()
            {
                let project_root = detect_project_root(file_path);
                crate::core::graph_context::build_graph_context(
                    file_path,
                    &project_root,
                    Some(crate::core::graph_context::GraphContextOptions::default()),
                )
                .map(|c| crate::core::graph_context::format_graph_context(&c))
                .unwrap_or_default()
            } else {
                String::new()
            };

            let sent = count_tokens(&filtered) + count_tokens(&header) + count_tokens(&graph_ctx);
            let savings = protocol::format_savings(original_tokens, sent);
            (
                append_compressed_hint(
                    &format!("{header}\n{filtered}{graph_ctx}\n{savings}"),
                    file_path,
                ),
                sent,
            )
        }
        "reference" => {
            let tok = count_tokens(content);
            let output = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!("{file_ref}={short}: {line_count} lines, {tok} tok ({ext})")
            } else {
                format!("{short}: {line_count} lines, {tok} tok ({ext})")
            };
            let sent = count_tokens(&output);
            let savings = protocol::format_savings(original_tokens, sent);
            (format!("{output}\n{savings}"), sent)
        }
        mode if mode.starts_with("lines:") => {
            let range_str = &mode[6..];
            let extracted = extract_line_range(content, range_str);
            let header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!("{file_ref}={short} {line_count}L lines:{range_str}")
            } else {
                format!("{short} {line_count}L lines:{range_str}")
            };
            let sent = count_tokens(&extracted);
            let savings = protocol::format_savings(original_tokens, sent);
            (format!("{header}\n{extracted}\n{savings}"), sent)
        }
        mode if mode.starts_with("density:") => {
            // SDE target-density mode: compress to a token budget instead of
            // maximum compression. `density:0.4` ≈ 40% of original tokens. A bare
            // `density:` falls back to the aggressiveness target (else 0.5).
            let aggr_target = tuning
                .aggressiveness
                .map(|a| AggressivenessProfile::from_level(a).density_target);
            let target: f64 = mode[8..].parse().ok().or(aggr_target).unwrap_or(0.5);
            let result = entropy::entropy_compress_to_density(content, target);
            let actual = if result.original_tokens > 0 {
                result.compressed_tokens as f64 / result.original_tokens as f64
            } else {
                0.0
            };
            let techs = techniques_tag(&result.techniques);
            let target_clamped = target.clamp(0.05, 1.0);
            let header = if crate::core::protocol::meta_visible() && !file_ref.is_empty() {
                format!(
                    "{file_ref}={short} {line_count}L density target={target_clamped:.2} actual={actual:.2}{techs}"
                )
            } else {
                format!(
                    "{short} {line_count}L density target={target_clamped:.2} actual={actual:.2}{techs}"
                )
            };
            let output = format!("{header}\n{}", result.output);
            let sent = count_tokens(&output);
            let savings = protocol::format_savings(original_tokens, sent);
            (
                append_compressed_hint(&format!("{output}\n{savings}"), file_path),
                sent,
            )
        }
        unknown => {
            let header = build_header(file_ref, short, ext, content, line_count, true);
            let out = format!(
                "[WARNING: unknown mode '{unknown}', falling back to full]\n{header}\n{content}"
            );
            let sent = count_tokens(&out);
            (out, sent)
        }
    }
}

/// When a task is active, find the symbol whose name best matches a task
/// keyword and return its body as numbered source lines (capped).
///
/// `map`/`signatures` stay compact but include the one symbol body the agent is
/// most likely about to read, avoiding a follow-up full read. Uses the
/// tree-sitter chunk extractor (which carries spans + body across languages); a
/// no-op when tree-sitter is unavailable.
pub(crate) fn task_relevant_body(
    content: &str,
    file_path: &str,
    ext: &str,
    task: Option<&str>,
) -> Option<String> {
    const MAX_BODY_LINES: usize = 80;

    let task = task.map(str::trim).filter(|t| !t.is_empty())?;
    let (_files, keywords) = crate::core::task_relevance::parse_task_hints(task);
    if keywords.is_empty() {
        return None;
    }
    let kw_lower: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();

    let chunks = crate::core::chunks_ts::extract_chunks_ts(file_path, content, ext)?;

    // Score: exact name match (2) beats substring overlap (1).
    let mut best_idx: Option<usize> = None;
    let mut best_score = 0u8;
    for (i, ch) in chunks.iter().enumerate() {
        if ch.symbol_name.is_empty() {
            continue;
        }
        let name_l = ch.symbol_name.to_lowercase();
        let substr = kw_lower
            .iter()
            .any(|k| k.len() >= 3 && (name_l.contains(k.as_str()) || k.contains(name_l.as_str())));
        let score = if kw_lower.contains(&name_l) {
            2
        } else {
            u8::from(substr)
        };
        if score > best_score {
            best_score = score;
            best_idx = Some(i);
        }
    }

    let ch = &chunks[best_idx?];
    let body_lines: Vec<&str> = ch.content.lines().collect();
    let total = body_lines.len();
    let shown = total.min(MAX_BODY_LINES);
    let body: String = body_lines[..shown]
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>4}|{l}", ch.start_line + i))
        .collect::<Vec<_>>()
        .join("\n");
    let truncated = if shown < total {
        format!(
            "\n  … +{} lines — ctx_read(mode=\"lines:{}-{}\")",
            total - shown,
            ch.start_line + shown,
            ch.end_line
        )
    } else {
        String::new()
    };
    Some(format!(
        "  ▸ body {} L{}-{}:\n{body}{truncated}",
        ch.symbol_name, ch.start_line, ch.end_line
    ))
}

pub(crate) fn extract_line_range(content: &str, range_str: &str) -> String {
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
        } else if let Ok(n) = part.parse::<usize>()
            && n >= 1
            && n <= total
        {
            selected.push(format!("{n:>4}| {}", lines[n - 1]));
        }
    }

    if selected.is_empty() {
        "No lines matched the range.".to_string()
    } else {
        selected.join("\n")
    }
}

#[cfg(test)]
mod render_tests {
    use super::techniques_tag;

    #[test]
    fn techniques_tag_omits_empty_brackets() {
        // An incompressible file leaves no techniques — the header must not
        // carry an empty ` []` field (#509 output-waste audit).
        assert_eq!(techniques_tag(&[]), "");
    }

    #[test]
    fn techniques_tag_wraps_nonempty_with_leading_space() {
        assert_eq!(
            techniques_tag(&["⊘ 3 low-entropy lines".to_string(), "⊘ 2 dups".to_string()]),
            " [⊘ 3 low-entropy lines, ⊘ 2 dups]"
        );
    }

    #[test]
    fn techniques_tag_single() {
        assert_eq!(
            techniques_tag(&["density target=0.40".to_string()]),
            " [density target=0.40]"
        );
    }
}
