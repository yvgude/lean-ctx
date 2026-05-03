use crate::core::cache::SessionCache;
use crate::core::protocol;
use crate::core::signatures;
use crate::core::tokens::count_tokens;
use crate::tools::ctx_response;
use crate::tools::CrpMode;

pub fn handle(cache: &SessionCache, include_signatures: bool, crp_mode: CrpMode) -> String {
    let entries = cache.get_all_entries();
    let file_count = entries.len();

    if file_count == 0 {
        return "CTX CHECKPOINT (0 files)\nNo files cached yet.".to_string();
    }

    let mut sections = Vec::new();
    sections.push(format!("CTX CHECKPOINT ({file_count} files)"));
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
            let sigs = signatures::extract_signatures(&entry.content, ext);
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
        stats.total_reads,
        stats.cache_hits,
        stats.hit_rate()
    ));

    // Cross-file codebook deduplication
    let files_for_codebook: Vec<(String, String)> = entries
        .iter()
        .map(|(p, e)| ((*p).clone(), e.content.clone()))
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

    format!("{cleaned_output}{legend}\nCOMPRESSION: {total_original} → {compressed_tokens} tok\n{savings}")
}
