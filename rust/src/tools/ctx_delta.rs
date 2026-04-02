use similar::{ChangeTag, TextDiff};

use crate::core::cache::SessionCache;
use crate::core::protocol;
use crate::core::tokens::count_tokens;

pub fn handle(cache: &mut SessionCache, path: &str) -> String {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return format!("Error: {e}"),
    };

    let short = protocol::shorten_path(path);
    let new_lines = content.lines().count();
    let new_tokens = count_tokens(&content);

    let cached = cache.get(path);
    if cached.is_none() {
        cache.store(path, content.clone());
        return format!(
            "{short} [first read, {new_lines}L, {new_tokens} tok] — cached for future deltas"
        );
    }

    let cached_entry = cached.unwrap();
    let old_content = cached_entry.content.clone();
    let old_hash = cached_entry.hash.clone();

    let new_hash = compute_hash(&content);
    if old_hash == new_hash {
        return format!("{short} cached (no changes)");
    }

    let diff = TextDiff::from_lines(&old_content, &content);
    let mut hunks = Vec::new();
    let mut additions = 0usize;
    let mut deletions = 0usize;

    for group in diff.grouped_ops(3) {
        let mut hunk_lines = Vec::new();
        for op in &group {
            for change in diff.iter_changes(op) {
                let line_no = change.new_index().or(change.old_index()).map(|i| i + 1);
                let text = change.value().trim_end_matches('\n');
                match change.tag() {
                    ChangeTag::Insert => {
                        additions += 1;
                        if let Some(n) = line_no {
                            hunk_lines.push(format!("+{n}: {text}"));
                        }
                    }
                    ChangeTag::Delete => {
                        deletions += 1;
                        if let Some(n) = line_no {
                            hunk_lines.push(format!("-{n}: {text}"));
                        }
                    }
                    ChangeTag::Equal => {
                        if let Some(n) = line_no {
                            hunk_lines.push(format!(" {n}: {text}"));
                        }
                    }
                }
            }
        }
        if !hunk_lines.is_empty() {
            hunks.push(hunk_lines.join("\n"));
        }
    }

    cache.store(path, content);

    let delta_output = hunks.join("\n---\n");
    let delta_tokens = count_tokens(&delta_output);
    let savings = if new_tokens > 0 {
        ((new_tokens as f64 - delta_tokens as f64) / new_tokens as f64 * 100.0) as u32
    } else {
        0
    };

    format!(
        "{short} [delta] +{additions}/-{deletions} lines ({delta_tokens} tok, {savings}% saved vs full)\n{delta_output}"
    )
}

fn compute_hash(content: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}
