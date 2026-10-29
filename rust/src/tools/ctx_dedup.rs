use std::collections::{HashMap, HashSet};

use crate::core::cache::{SessionCache, SharedBlock};
use crate::core::codebook;
use crate::core::tokens::count_tokens;

pub fn handle(cache: &SessionCache) -> String {
    analyze(cache)
}

pub fn handle_action(cache: &mut SessionCache, action: &str) -> String {
    match action {
        "apply" => apply_dedup(cache),
        _ => analyze(cache),
    }
}

fn apply_dedup(cache: &mut SessionCache) -> String {
    let entries = cache.get_all_entries();
    if entries.len() < 2 {
        return "Need at least 2 cached files for cross-file dedup.".to_string();
    }

    let mut block_occurrences: HashMap<String, Vec<(String, usize)>> = HashMap::new();
    for (path, _) in &entries {
        // Shared blocks are derived from, and registered with, the content
        // the cache's current-rights accessor releases NOW (G04) — never the
        // raw entry.
        let Some(full_content) =
            crate::tools::admitted_sources::released_cache_content(cache, path)
        else {
            continue;
        };
        let lines: Vec<&str> = full_content.lines().collect();
        for (idx, chunk) in lines.chunks(5).enumerate() {
            if chunk.len() == 5 {
                let block = chunk.join("\n");
                let trimmed = block.trim().to_string();
                if !trimmed.is_empty() && count_tokens(&trimmed) > 10 {
                    block_occurrences
                        .entry(trimmed)
                        .or_default()
                        .push(((*path).clone(), idx * 5 + 1));
                }
            }
        }
    }

    let mut shared = Vec::new();
    for (content, occurrences) in &block_occurrences {
        let unique_files: HashSet<&str> = occurrences.iter().map(|(p, _)| p.as_str()).collect();
        if unique_files.len() >= 2 {
            let (canonical_path, start_line) = &occurrences[0];
            let ref_label = cache
                .file_ref_map()
                .get(canonical_path)
                .cloned()
                .unwrap_or_else(|| "F?".to_string());
            shared.push(SharedBlock {
                canonical_path: canonical_path.clone(),
                canonical_ref: ref_label,
                start_line: *start_line,
                end_line: start_line + 4,
                content: content.clone(),
            });
        }
    }

    let count = shared.len();
    let savings: usize = shared
        .iter()
        .map(|b| {
            let occurrences = block_occurrences.get(&b.content).map_or(0, |o| {
                let unique: HashSet<&str> = o.iter().map(|(p, _)| p.as_str()).collect();
                unique.len() - 1
            });
            count_tokens(&b.content) * occurrences
        })
        .sum();

    cache.set_shared_blocks(shared);

    format!(
        "Applied cross-file dedup: {count} shared blocks registered (~{savings} tokens saveable)"
    )
}

fn analyze(cache: &SessionCache) -> String {
    let entries = cache.get_all_entries();
    if entries.len() < 2 {
        return "Need at least 2 cached files for cross-file deduplication analysis.".to_string();
    }

    let mut import_patterns: HashMap<String, Vec<String>> = HashMap::new();
    let mut boilerplate_blocks: HashMap<String, Vec<String>> = HashMap::new();

    // Every entry is taken once, as the cache's current-rights accessor
    // releases it NOW (G04); the analysis below quotes import lines and block
    // previews, so it must not see a raw entry.
    let released: Vec<(String, String)> = entries
        .iter()
        .filter_map(|(path, _)| {
            let content = crate::tools::admitted_sources::released_cache_content(cache, path)?;
            Some(((*path).clone(), content))
        })
        .collect();

    for (path, full_content) in &released {
        let lines: Vec<&str> = full_content.lines().collect();

        let imports: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| {
                let t = l.trim();
                t.starts_with("import ")
                    || t.starts_with("use ")
                    || t.starts_with("from ")
                    || t.starts_with("require(")
                    || t.starts_with("#include")
            })
            .collect();

        for imp in &imports {
            let key = imp.trim().to_string();
            import_patterns
                .entry(key)
                .or_default()
                .push((*path).clone());
        }

        for chunk in lines.chunks(5) {
            if chunk.len() == 5 {
                let block = chunk.join("\n");
                let block_trimmed = block.trim().to_string();
                if !block_trimmed.is_empty() && count_tokens(&block_trimmed) > 10 {
                    boilerplate_blocks
                        .entry(block_trimmed)
                        .or_default()
                        .push((*path).clone());
                }
            }
        }
    }

    let shared_imports: Vec<_> = import_patterns
        .iter()
        .filter(|(_, files)| files.len() >= 2)
        .collect();

    let shared_blocks: Vec<_> = boilerplate_blocks
        .iter()
        .filter(|(_, files)| {
            let unique: std::collections::HashSet<_> = files.iter().collect();
            unique.len() >= 2
        })
        .collect();

    let mut result = Vec::new();
    result.push(format!(
        "Cross-file deduplication analysis ({} cached files):",
        entries.len()
    ));

    if !shared_imports.is_empty() {
        let total_import_tokens: usize = shared_imports
            .iter()
            .map(|(imp, files)| count_tokens(imp) * (files.len() - 1))
            .sum();

        result.push(format!(
            "\nShared imports ({}, ~{total_import_tokens} redundant tokens):",
            shared_imports.len()
        ));
        for (imp, files) in shared_imports.iter().take(10) {
            let short_files: Vec<String> = files
                .iter()
                .map(|f| crate::core::protocol::shorten_path(f))
                .collect();
            result.push(format!("  {imp}"));
            result.push(format!("    in: {}", short_files.join(", ")));
        }
        if shared_imports.len() > 10 {
            result.push(format!("  ... +{} more", shared_imports.len() - 10));
        }
    }

    if !shared_blocks.is_empty() {
        let total_block_tokens: usize = shared_blocks
            .iter()
            .map(|(block, files)| {
                let unique: std::collections::HashSet<_> = files.iter().collect();
                count_tokens(block) * (unique.len() - 1)
            })
            .sum();

        result.push(format!(
            "\nShared code blocks ({}, ~{total_block_tokens} redundant tokens):",
            shared_blocks.len()
        ));
        for (block, files) in shared_blocks.iter().take(5) {
            let unique: std::collections::HashSet<_> = files.iter().collect();
            let preview = block.lines().next().unwrap_or("...");
            result.push(format!("  \"{preview}...\" (in {} files)", unique.len()));
        }
    }

    // TF-IDF cosine similarity analysis for semantic duplicates
    let semantic_dups = codebook::find_semantic_duplicates(&released, 0.75);
    if !semantic_dups.is_empty() {
        result.push(format!(
            "\nSemantic duplicates (TF-IDF cosine > 0.75, {} pairs):",
            semantic_dups.len()
        ));
        for (a, b, sim) in semantic_dups.iter().take(8) {
            result.push(format!(
                "  {:.0}% similar: {} ↔ {}",
                sim * 100.0,
                crate::core::protocol::shorten_path(a),
                crate::core::protocol::shorten_path(b)
            ));
        }
        if semantic_dups.len() > 8 {
            result.push(format!("  ... +{} more pairs", semantic_dups.len() - 8));
        }
    }

    if shared_imports.is_empty() && shared_blocks.is_empty() && semantic_dups.is_empty() {
        result.push("\nNo significant cross-file duplication detected.".to_string());
    } else {
        let total_savings: usize = shared_imports
            .iter()
            .map(|(imp, files)| count_tokens(imp) * (files.len() - 1))
            .sum::<usize>()
            + shared_blocks
                .iter()
                .map(|(block, files)| {
                    let unique: std::collections::HashSet<_> = files.iter().collect();
                    count_tokens(block) * (unique.len() - 1)
                })
                .sum::<usize>();

        result.push(format!(
            "\nTotal potential savings: ~{total_savings} tokens"
        ));
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// G04: the analysis quoted import lines and block previews straight from
    /// raw cache entries — also of a source that no longer exists. It now
    /// sees only what the cache's current-rights accessor releases.
    #[test]
    fn analysis_quotes_only_released_cache_entries() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project = tempfile::tempdir().unwrap();
        let source = "use widget_core::PlanMarker;\nfn body() {}\n";
        let mut cache = SessionCache::new();
        let mut paths = Vec::new();
        for name in ["a.rs", "b.rs"] {
            let path = project.path().join(name);
            std::fs::write(&path, source).unwrap();
            let path = path.to_string_lossy().into_owned();
            cache.store(&path, source);
            paths.push(path);
        }

        let out = handle(&cache);
        assert!(out.contains("use widget_core::PlanMarker;"), "{out}");

        std::fs::remove_file(&paths[1]).unwrap();
        let out = handle(&cache);
        assert!(!out.contains("widget_core::PlanMarker"), "{out}");
        assert_eq!(
            handle_action(&mut cache, "apply"),
            "Applied cross-file dedup: 0 shared blocks registered (~0 tokens saveable)"
        );
    }
}
