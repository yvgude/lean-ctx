use crate::core::cache::SessionCache;
use crate::core::protocol;
use crate::core::signatures;
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;
use crate::tools::ctx_response;

pub fn handle(cache: &SessionCache, include_signatures: bool, crp_mode: CrpMode) -> String {
    let entries = cache.get_all_entries();
    let file_count = entries.len();

    if file_count == 0 {
        return "CTX CHECKPOINT (0 files)\nNo files cached yet.".to_string();
    }

    let mut sections = Vec::new();
    sections.push(format!("CTX CHECKPOINT ({file_count} files)"));
    // Self-describing outputs (GL #580): one legend up front when the
    // checkpoint renders TDD symbol notation below.
    if include_signatures && crp_mode.is_tdd() {
        sections.push("[λ=fn §=class ∂=trait τ=type ε=enum ν=val +=pub ~=async]".to_string());
    }
    sections.push(String::new());

    let mut total_original = 0usize;
    let refs = cache.file_ref_map();

    for (path, entry) in &entries {
        total_original += entry.original_tokens;
        let file_ref = refs.get(*path).map_or("F?", |s| s.as_str());
        let short = protocol::shorten_path(path);

        if include_signatures {
            let ext = std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            // Signatures are a derivation of the cached copy: they come from
            // what the cache's current-rights accessor releases NOW (G04),
            // never from the raw entry. An entry that is not released keeps
            // its metadata line below and contributes no signature.
            let Some(content) = crate::tools::admitted_sources::released_cache_content(cache, path)
            else {
                sections.push(format!(
                    "{file_ref} {short} [{}L]: [withheld]",
                    entry.line_count
                ));
                continue;
            };
            let sigs = signatures::extract_signatures(&content, ext);
            let sig_names: Vec<String> = sigs
                .iter()
                .take(5)
                .map(|s| {
                    if crp_mode.is_tdd() {
                        s.to_tdd()
                    } else {
                        s.to_compact()
                    }
                })
                .collect();
            let more = if sigs.len() > 5 {
                format!("+{}", sigs.len() - 5)
            } else {
                String::new()
            };
            sections.push(format!(
                "{file_ref} {short} [{}L]: {}{more}",
                entry.line_count,
                sig_names.join(", "),
            ));
        } else {
            sections.push(format!(
                "{file_ref} {short} [{}L {}t]",
                entry.line_count, entry.original_tokens
            ));
        }
    }

    let stats = cache.get_stats();
    sections.push(String::new());
    sections.push(format!(
        "STATS: {} reads, {} hits ({:.0}%)",
        stats.total_reads(),
        stats.cache_hits(),
        stats.hit_rate()
    ));

    // ACE delta playbook (#541): distill the session into incremental,
    // stable-ID entries instead of re-summarizing previous summaries —
    // prevents brevity bias and context collapse across checkpoints.
    if let Some(playbook_block) = update_playbook_from_session() {
        sections.push(String::new());
        sections.push(playbook_block);
    }

    let contents: Vec<(String, String)> = entries
        .iter()
        .filter_map(|(p, _)| {
            let released = crate::tools::admitted_sources::released_cache_content(cache, p)?;
            Some(((*p).clone(), released))
        })
        .collect();
    let files_for_codebook: Vec<(&str, &str)> = contents
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let mut codebook = crate::core::codebook::Codebook::new();
    codebook.build_from_files(&files_for_codebook);

    let output = sections.join("\n");

    let (final_output, legend) = if codebook.is_empty() {
        (output, String::new())
    } else {
        let (compressed, refs_used) = codebook.compress(&output);
        let legend = codebook.format_legend(&refs_used);
        if refs_used.is_empty() {
            (output, String::new())
        } else {
            (compressed, format!("\n{legend}"))
        }
    };

    // Apply filler removal to checkpoint output
    let cleaned_output = ctx_response::handle(&final_output, crp_mode);

    let compressed_tokens = count_tokens(&cleaned_output) + count_tokens(&legend);
    let savings = protocol::format_savings(total_original, compressed_tokens);

    format!(
        "{cleaned_output}{legend}\nCOMPRESSION: {total_original} → {compressed_tokens} tok\n{savings}"
    )
}

/// Grow-and-refine (#541): fold the caller's own session findings, decisions
/// and modified files into its playbook as deltas (dedup confirms instead of
/// duplicating), evict locally, and return the rendered block. Never the
/// project's latest saved session, which may be another agent's (I07/S03);
/// without a caller session (CLI) there is no playbook.
fn update_playbook_from_session() -> Option<String> {
    use crate::core::session::EntryKind;

    let mut session = crate::core::session::caller::with(Clone::clone)?;
    let turn = session.stats.total_tool_calls;

    let findings: Vec<String> = session
        .findings
        .iter()
        .rev()
        .take(8)
        .map(|f| f.summary.clone())
        .collect();
    let decisions: Vec<String> = session
        .decisions
        .iter()
        .rev()
        .take(5)
        .map(|d| d.summary.clone())
        .collect();
    let modified: Vec<String> = session
        .files_touched
        .iter()
        .filter(|f| f.modified)
        .rev()
        .take(10)
        .map(|f| {
            let why = f.summary.as_deref().unwrap_or("modified this session");
            format!("{} — {}", f.path, why)
        })
        .collect();

    for s in findings {
        session.playbook.add_delta(EntryKind::Fact, &s, turn);
    }
    for s in decisions {
        session.playbook.add_delta(EntryKind::Strategy, &s, turn);
    }
    for s in modified {
        session.playbook.add_delta(EntryKind::FileRef, &s, turn);
    }
    // Pitfalls from live bounce evidence: extensions that keep bouncing are
    // exactly the "this bit us" knowledge ACE wants preserved verbatim.
    if let Ok(bt) = crate::core::bounce_tracker::global().lock()
        && bt.total_bounces() > 0
    {
        for ext_stat in bt.per_extension_json() {
            let (Some(ext), Some(rate)) = (
                ext_stat.get("ext").and_then(|v| v.as_str()),
                ext_stat.get("rate").and_then(serde_json::Value::as_f64),
            ) else {
                continue;
            };
            if rate >= 0.3 {
                session.playbook.add_delta(
                    EntryKind::Pitfall,
                    &format!(
                        "{ext} files bounce often ({:.0}% rate) — prefer mode=full",
                        rate * 100.0
                    ),
                    turn,
                );
            }
        }
    }
    // Reflector (ACE analog): distill the gotcha trace — proven error→fix
    // strategies and recurring unresolved pitfalls — into playbook deltas, so
    // hard-won shell-outcome knowledge survives checkpoints next to live
    // session findings. Deterministic and dedup-confirmed by `add_delta`.
    if let Some(root) = session.project_root.as_deref() {
        let store = crate::core::gotcha_tracker::GotchaStore::load(root);
        let insights = crate::core::gotcha_tracker::reflect(&store);
        crate::core::gotcha_tracker::fold_into_playbook(&insights, &mut session.playbook, turn);
    }

    session.playbook.evict(turn);

    let rendered = session.playbook.render(20);
    // The server's save cadence persists the caller's session.
    crate::core::session::caller::with_mut(|own| {
        own.playbook = session.playbook;
        own.increment();
    });
    if rendered.is_empty() {
        None
    } else {
        Some(rendered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G04: the checkpoint derived signatures from the raw cache entry — also
    /// when the source behind it was gone. It now derives only from what the
    /// cache's current-rights accessor releases.
    #[test]
    fn signatures_come_only_from_released_cache_entries() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project = tempfile::tempdir().unwrap();
        let kept = project.path().join("kept.rs");
        let gone = project.path().join("gone.rs");
        std::fs::write(&kept, "pub fn kept_symbol() {}\n").unwrap();
        std::fs::write(&gone, "pub fn gone_symbol() {}\n").unwrap();
        let (kept, gone) = (
            kept.to_string_lossy().into_owned(),
            gone.to_string_lossy().into_owned(),
        );
        let mut cache = SessionCache::new();
        cache.store(&kept, "pub fn kept_symbol() {}\n");
        cache.store(&gone, "pub fn gone_symbol() {}\n");

        let out = handle(&cache, true, CrpMode::Off);
        assert!(
            out.contains("kept_symbol") && out.contains("gone_symbol"),
            "{out}"
        );

        std::fs::remove_file(&gone).unwrap();
        let out = handle(&cache, true, CrpMode::Off);
        assert!(out.contains("kept_symbol"), "{out}");
        assert!(!out.contains("gone_symbol"), "{out}");
        assert!(out.contains("[withheld]"), "{out}");
    }
}
