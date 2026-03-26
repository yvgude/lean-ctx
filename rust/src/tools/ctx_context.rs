use crate::core::cache::SessionCache;
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

#[allow(dead_code)]
const DEFAULT_WINDOW_SIZE: usize = 10;

#[allow(dead_code)]
pub struct ContextWindow {
    entries: Vec<ContextEntry>,
    max_entries: usize,
}

#[allow(dead_code)]
struct ContextEntry {
    turn_id: usize,
    tool: String,
    path: Option<String>,
    summary: String,
    token_count: usize,
}

#[allow(dead_code)]
impl ContextWindow {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_entries,
        }
    }

    pub fn record(&mut self, turn_id: usize, tool: &str, path: Option<&str>, content: &str) {
        let summary = summarize_content(content);
        let token_count = count_tokens(content);

        self.entries.push(ContextEntry {
            turn_id,
            tool: tool.to_string(),
            path: path.map(|p| p.to_string()),
            summary,
            token_count,
        });

        while self.entries.len() > self.max_entries {
            self.entries.remove(0);
        }
    }

    pub fn get_known_files(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter_map(|e| e.path.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn was_recently_read(&self, path: &str, within_turns: usize) -> bool {
        let current_turn = self.entries.last().map(|e| e.turn_id).unwrap_or(0);
        self.entries
            .iter()
            .rev()
            .any(|e| e.path.as_deref() == Some(path) && (current_turn - e.turn_id) <= within_turns)
    }

    pub fn format_summary(&self) -> String {
        if self.entries.is_empty() {
            return "No context recorded yet.".to_string();
        }

        let mut result = Vec::new();
        result.push(format!(
            "Context window ({}/{} entries):",
            self.entries.len(),
            self.max_entries
        ));

        let total_tokens: usize = self.entries.iter().map(|e| e.token_count).sum();
        result.push(format!("Total tokens processed: {total_tokens}"));
        result.push(String::new());

        for entry in &self.entries {
            let path_info = entry
                .path
                .as_deref()
                .map(|p| format!(" ({})", crate::core::protocol::shorten_path(p)))
                .unwrap_or_default();
            result.push(format!(
                "  T{}: {}{} — {} ({} tok)",
                entry.turn_id, entry.tool, path_info, entry.summary, entry.token_count
            ));
        }

        result.join("\n")
    }
}

#[allow(dead_code)]
pub fn handle(cache: &SessionCache, window: &ContextWindow) -> String {
    let mut result = Vec::new();

    result.push(window.format_summary());
    result.push(String::new());

    let known_files = window.get_known_files();
    if !known_files.is_empty() {
        result.push(format!("Files in context ({}):", known_files.len()));
        for file in &known_files {
            let in_cache = cache.get(file).is_some();
            let status = if in_cache { "cached" } else { "evicted" };
            result.push(format!(
                "  {} [{}]",
                crate::core::protocol::shorten_path(file),
                status
            ));
        }
    }

    result.join("\n")
}

pub fn handle_status(cache: &SessionCache, turn_count: usize, crp_mode: CrpMode) -> String {
    let entries = cache.get_all_entries();
    let mut result = Vec::new();

    result.push(format!("Multi-turn context (turn {turn_count}):"));
    result.push(format!("  Cached files: {}", entries.len()));

    let total_tokens: usize = entries.iter().map(|(_, e)| e.original_tokens).sum();
    let total_reads: u32 = entries.iter().map(|(_, e)| e.read_count).sum();
    result.push(format!("  Total original tokens: {total_tokens}"));
    result.push(format!("  Total reads: {total_reads}"));

    let frequent: Vec<_> = entries.iter().filter(|(_, e)| e.read_count > 1).collect();
    if !frequent.is_empty() {
        result.push(format!("\n  Frequently accessed ({}):", frequent.len()));
        for (path, entry) in &frequent {
            result.push(format!(
                "    {} ({}x, {} tok)",
                crate::core::protocol::shorten_path(path),
                entry.read_count,
                entry.original_tokens
            ));
        }
    }

    let mode_label = match crp_mode {
        CrpMode::Off => "off",
        CrpMode::Compact => "compact",
        CrpMode::Tdd => "tdd",
    };
    result.push(format!("\n  CRP mode: {mode_label}"));

    let complexity = crate::core::adaptive::classify_from_context(cache);
    result.push(format!("\n  {}", complexity.encoded_suffix()));

    let hints = generate_prefill_hints(cache);
    if !hints.is_empty() {
        result.push("\nSMART HINTS:".to_string());
        for hint in &hints {
            result.push(format!("  → {hint}"));
        }
    }

    result.join("\n")
}

fn generate_prefill_hints(cache: &SessionCache) -> Vec<String> {
    let entries = cache.get_all_entries();
    let mut hints = Vec::new();

    // Hint 1: Read-only files that could use lighter modes
    let read_heavy: Vec<_> = entries
        .iter()
        .filter(|(_, e)| e.read_count >= 3 && e.original_tokens > 500)
        .collect();
    for (path, entry) in &read_heavy {
        let short = crate::core::protocol::shorten_path(path);
        hints.push(format!(
            "{short} read {}x ({} tok) — consider mode=map for future reads",
            entry.read_count, entry.original_tokens
        ));
    }

    // Hint 2: Large files that might benefit from aggressive/entropy
    let large: Vec<_> = entries
        .iter()
        .filter(|(_, e)| e.original_tokens > 2000 && e.read_count <= 1)
        .collect();
    for (path, entry) in &large {
        let short = crate::core::protocol::shorten_path(path);
        hints.push(format!(
            "{short} is large ({} tok) — consider mode=signatures or aggressive",
            entry.original_tokens
        ));
    }

    // Hint 3: Stale cache entries (very old reads, only once)
    let stale_count = entries.iter().filter(|(_, e)| e.read_count == 1).count();
    if stale_count > 5 {
        hints.push(format!(
            "{stale_count} files read only once — ctx_cache clear to free context"
        ));
    }

    hints.truncate(5);
    hints
}

#[allow(dead_code)]
fn summarize_content(content: &str) -> String {
    let lines: Vec<&str> = content.lines().take(3).collect();
    let first = lines.first().unwrap_or(&"");
    if first.len() > 80 {
        let truncated: String = first.chars().take(77).collect();
        format!("{truncated}...")
    } else {
        first.to_string()
    }
}
