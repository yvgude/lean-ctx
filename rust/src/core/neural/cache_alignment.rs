//! KV-Cache alignment for commercial LLM prompt caching.
//!
//! Claude's prompt caching stores KV-tensors for byte-exact prefix matches.
//! GPT models have similar mechanisms. This module ensures lean-ctx outputs
//! are structured to maximize cache hit rates.
//!
//! Key strategies:
//! 1. Stable prefix: invariant content (instructions, tool defs) comes first
//! 2. Cache-block alignment: content segmented to match provider breakpoints
//! 3. Delta-only after cached prefix: only send changes, rest stays in KV-cache
//! 4. Deterministic ordering: same inputs always produce byte-identical output

const CLAUDE_CACHE_MIN_TOKENS: usize = 1024;
const CLAUDE_MAX_CACHE_BREAKPOINTS: usize = 4;

#[derive(Debug, Clone)]
pub struct CacheBlock {
    pub id: String,
    pub content: String,
    pub is_stable: bool,
    pub priority: u8,
    pub estimated_tokens: usize,
}

#[derive(Default)]
pub struct CacheAlignedOutput {
    blocks: Vec<CacheBlock>,
}

impl CacheAlignedOutput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_stable_block(&mut self, id: &str, content: String, priority: u8) {
        let tokens = estimate_tokens(&content);
        self.blocks.push(CacheBlock {
            id: id.to_string(),
            content,
            is_stable: true,
            priority,
            estimated_tokens: tokens,
        });
    }

    pub fn add_variable_block(&mut self, id: &str, content: String, priority: u8) {
        let tokens = estimate_tokens(&content);
        self.blocks.push(CacheBlock {
            id: id.to_string(),
            content,
            is_stable: false,
            priority,
            estimated_tokens: tokens,
        });
    }

    /// Render the output with cache-optimal ordering:
    /// stable blocks first (sorted by priority), then variable blocks.
    pub fn render(&self) -> String {
        let mut stable: Vec<&CacheBlock> = self.blocks.iter().filter(|b| b.is_stable).collect();
        let mut variable: Vec<&CacheBlock> = self.blocks.iter().filter(|b| !b.is_stable).collect();

        stable.sort_by_key(|b| b.priority);
        variable.sort_by_key(|b| b.priority);

        let mut output = String::new();

        for block in &stable {
            output.push_str(&block.content);
            output.push('\n');
        }

        for block in &variable {
            output.push_str(&block.content);
            output.push('\n');
        }

        output
    }

    /// Render with explicit cache breakpoint markers for Claude.
    /// Places up to CLAUDE_MAX_CACHE_BREAKPOINTS markers at optimal positions.
    pub fn render_with_breakpoints(&self) -> (String, Vec<usize>) {
        let rendered = self.render();
        let breakpoints = compute_breakpoints(&rendered);
        (rendered, breakpoints)
    }

    pub fn stable_token_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|b| b.is_stable)
            .map(|b| b.estimated_tokens)
            .sum()
    }

    pub fn variable_token_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|b| !b.is_stable)
            .map(|b| b.estimated_tokens)
            .sum()
    }

    pub fn cache_efficiency(&self) -> f64 {
        let total = self.stable_token_count() + self.variable_token_count();
        if total == 0 {
            return 0.0;
        }
        self.stable_token_count() as f64 / total as f64
    }
}

/// Compute optimal cache breakpoint positions in the output.
/// Tries to place breakpoints at natural content boundaries
/// that align with Claude's minimum cache block size.
fn compute_breakpoints(content: &str) -> Vec<usize> {
    let total_tokens = estimate_tokens(content);
    if total_tokens < CLAUDE_CACHE_MIN_TOKENS {
        return Vec::new();
    }

    let mut breakpoints = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut accumulated_tokens = 0;
    let target_block_size = total_tokens / (CLAUDE_MAX_CACHE_BREAKPOINTS + 1);

    for (i, line) in lines.iter().enumerate() {
        accumulated_tokens += estimate_tokens(line);

        if accumulated_tokens >= target_block_size
            && breakpoints.len() < CLAUDE_MAX_CACHE_BREAKPOINTS
            && is_natural_boundary(line, lines.get(i + 1).copied())
        {
            breakpoints.push(i);
            accumulated_tokens = 0;
        }
    }

    breakpoints
}

fn is_natural_boundary(line: &str, next_line: Option<&str>) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    if trimmed.starts_with("---") || trimmed.starts_with("===") {
        return true;
    }
    if trimmed.starts_with("##") || trimmed.starts_with("//") {
        return true;
    }
    if let Some(next) = next_line {
        let next_trimmed = next.trim();
        if next_trimmed.is_empty() || next_trimmed.starts_with("---") {
            return true;
        }
    }
    false
}

fn estimate_tokens(text: &str) -> usize {
    text.len() / 4 + 1
}

/// Generate a delta between two versions of content for cache-efficient updates.
/// Returns only the changed portions, prefixed with stable context identifiers.
pub fn compute_delta(previous: &str, current: &str) -> DeltaResult {
    let prev_lines: Vec<&str> = previous.lines().collect();
    let curr_lines: Vec<&str> = current.lines().collect();

    let common_prefix = prev_lines
        .iter()
        .zip(curr_lines.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let common_suffix = prev_lines
        .iter()
        .rev()
        .zip(curr_lines.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();

    let prev_changed = prev_lines
        .len()
        .saturating_sub(common_prefix + common_suffix);
    let curr_changed = curr_lines
        .len()
        .saturating_sub(common_prefix + common_suffix);

    let changed_lines: Vec<String> = curr_lines
        [common_prefix..curr_lines.len().saturating_sub(common_suffix)]
        .iter()
        .map(|l| l.to_string())
        .collect();

    let prefix_tokens = estimate_tokens(
        &prev_lines[..common_prefix].to_vec().join("\n"),
    );

    DeltaResult {
        common_prefix_lines: common_prefix,
        common_suffix_lines: common_suffix,
        removed_lines: prev_changed,
        added_lines: curr_changed,
        changed_content: changed_lines.join("\n"),
        cached_prefix_tokens: prefix_tokens,
        total_delta_tokens: estimate_tokens(&changed_lines.join("\n")),
    }
}

#[derive(Debug)]
pub struct DeltaResult {
    pub common_prefix_lines: usize,
    pub common_suffix_lines: usize,
    pub removed_lines: usize,
    pub added_lines: usize,
    pub changed_content: String,
    pub cached_prefix_tokens: usize,
    pub total_delta_tokens: usize,
}

impl DeltaResult {
    pub fn savings_ratio(&self) -> f64 {
        let total = self.cached_prefix_tokens + self.total_delta_tokens;
        if total == 0 {
            return 0.0;
        }
        self.cached_prefix_tokens as f64 / total as f64
    }
}

/// Order file contents for maximum cache reuse across tool calls.
/// Stable elements (imports, type defs) first, then variable elements (function bodies).
pub fn cache_order_code(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();

    let mut imports = Vec::new();
    let mut definitions = Vec::new();
    let mut body = Vec::new();

    for line in &lines {
        let trimmed = line.trim();
        if trimmed.starts_with("import ")
            || trimmed.starts_with("use ")
            || trimmed.starts_with("from ")
            || trimmed.starts_with("#include")
        {
            imports.push(*line);
        } else if is_type_definition(trimmed) {
            definitions.push(*line);
        } else {
            body.push(*line);
        }
    }

    let mut result = Vec::new();
    let has_imports = !imports.is_empty();
    let has_definitions = !definitions.is_empty();
    let has_body = !body.is_empty();
    result.extend(imports);
    if has_imports && has_definitions {
        result.push("");
    }
    result.extend(definitions);
    if has_definitions && has_body {
        result.push("");
    }
    result.extend(body);

    result.join("\n")
}

fn is_type_definition(line: &str) -> bool {
    const STARTERS: &[&str] = &[
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "trait ",
        "pub trait ",
        "type ",
        "pub type ",
        "interface ",
        "export interface ",
        "export type ",
        "class ",
        "export class ",
    ];
    STARTERS.iter().any(|s| line.starts_with(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_blocks_come_first() {
        let mut output = CacheAlignedOutput::new();
        output.add_variable_block("var1", "variable content".into(), 1);
        output.add_stable_block("stable1", "stable content".into(), 1);

        let rendered = output.render();
        let stable_pos = rendered.find("stable content").unwrap();
        let var_pos = rendered.find("variable content").unwrap();
        assert!(stable_pos < var_pos);
    }

    #[test]
    fn delta_detects_changes() {
        let prev = "line1\nline2\nline3\nline4";
        let curr = "line1\nline2\nmodified\nline4";

        let delta = compute_delta(prev, curr);
        assert_eq!(delta.common_prefix_lines, 2);
        assert_eq!(delta.common_suffix_lines, 1);
        assert!(delta.changed_content.contains("modified"));
    }

    #[test]
    fn cache_efficiency_high_for_stable() {
        let mut output = CacheAlignedOutput::new();
        output.add_stable_block("s1", "x".repeat(1000), 1);
        output.add_variable_block("v1", "y".repeat(100), 1);

        assert!(output.cache_efficiency() > 0.8);
    }

    #[test]
    fn code_reordering_puts_imports_first() {
        let code = "fn main() {}\nuse std::io;\nimport os\nstruct Foo;";
        let reordered = cache_order_code(code);
        let lines: Vec<&str> = reordered.lines().collect();
        assert!(lines[0].starts_with("use ") || lines[0].starts_with("import "));
    }
}
