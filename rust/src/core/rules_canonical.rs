//! Canonical rules source — single source of truth for all lean-ctx guidance.
//!
//! All content is declared as `pub const` at the top. Two profiles (FULL,
//! COMPACT) define which sections compose each output format. Three wrappers
//! (Dedicated, Shared, Bare) select the profile and wrapping style. One
//! `render()` function assembles everything, including the compression-level
//! output-style prompt (Lite / Standard / Max).
//!
//! ***Every*** template, injected rule file, AGENTS.md block, and MCP
//! instructions field MUST derive its content from this module.

use crate::core::config::CompressionLevel;

/// Stable HTML-comment anchor that marks the start of any lean-ctx rule
/// section.  Never changes — used for find/replace in shared files and for
/// ownership detection in dedicated files.  The version number follows on the
/// next line (see `render`).
pub const START_MARK: &str = "<!-- lean-ctx-rules -->";

/// Prefix shared by every lean-ctx rules marker including legacy versioned
/// formats (`<!-- lean-ctx-rules-v9 -->`). Use for substring detection when
/// the exact constant would miss older installs.
pub const RULES_MARKER_PREFIX: &str = "<!-- lean-ctx-rules";

/// Start marker for lightweight AGENTS.md/CODEBUDDY.md/CLAUDE.md pointer
/// blocks. These are deliberately separate from `START_MARK` / `<!-- lean-ctx-rules -->`
/// because the pointer-only vs full-rules distinction drives duplicate detection
/// in `doctor overhead` — a pointer-only file (`is_pointer_only`) must not be
/// counted as a second source for its client.
pub const AGENTS_BLOCK_START: &str = "<!-- lean-ctx -->";

/// End marker for AGENTS.md/CODEBUDDY.md/CLAUDE.md pointer blocks.
pub const AGENTS_BLOCK_END: &str = "<!-- /lean-ctx -->";

/// Closing marker that ends a lean-ctx rule section.
pub const END_MARK: &str = "<!-- /lean-ctx-rules -->";

/// Current rules version (monotonically increasing integer).  Embedded as
/// `<!-- version: {RULES_VERSION} -->` right after `START_MARK` so the
/// injection layer can parse it and decide whether a file is up-to-date.
pub const RULES_VERSION: usize = 1;

/// Banner placed at the top of dedicated rule files (non-shadow only).
pub const CRITICAL: &str = "CRITICAL: ALWAYS use lean-ctx tools instead of native equivalents. \
     This is NOT optional.";

/// Compact bullet-list mapping (for both dedicated and shared contexts).
pub const BULLETS: &str = "\
MANDATORY MAPPING:\n\
• Read/cat -> ctx_read(path, mode)\n\
• Glob/find -> ctx_glob(pattern)\n\
• Shell/bash -> ctx_shell(command)\n\
• Grep -> ctx_search(pattern, path)\n\
• ls/find -> ctx_tree(path, depth)";

/// One-line admonition reinforcing the tool preference.  Always right after
/// `BULLETS` in both profiles.
pub const NEVER: &str =
    "NEVER use native Read/Grep/Shell/Glob when ctx_* equivalents are available.";

/// Intent-to-tool playbook — maps common agent questions to the right tool.
pub const INTENT: &str = "\
Tool selection by intent:\n\
• Understand code / find answers / before editing -> ctx_compose (call FIRST)\n\
• Read a file -> ctx_read(path, mode=signatures|map|full)\n\
• Find a symbol by name (exact) -> ctx_symbol\n\
• Search code by pattern (fuzzy) -> ctx_search\n\
• Search by meaning (concepts) -> ctx_semantic_search\n\
• Find files by pattern (glob) -> ctx_glob\n\
• Project structure -> ctx_tree\n\
• Who calls this / call graph -> ctx_callgraph\n\
• Session state / memory -> ctx_session / ctx_knowledge";

/// Anti-patterns that waste tokens and round-trips.
pub const ANTI: &str = "\
Anti-patterns — do NOT:\n\
• Chain ctx_search -> ctx_read -> ctx_symbol — one ctx_compose replaces all three\n\
• Grep for symbol definitions — ctx_symbol is faster + more precise\n\
• Use ctx_read(mode=full) for orientation — use mode=signatures\n\
• Use ctx_callgraph or ctx_graph for const/static/variable references — they track\n\
  function call edges and file-level deps only. Use grep or ctx_compose instead";

/// Encourages parallel tool calls to reduce round-trips.
pub const PARALLEL: &str = "\
PARALLEL tool calls: fire independent calls in the SAME turn — don't sequence them.\n\
One turn with 5 parallel ctx_read calls completes faster than 5 sequential turns.\n\
ctx_compose bundles multiple lookups into one call; for anything it doesn't\n\
cover, batch independent reads/searches together.";

/// One-line automation reminder.
pub const AUTO: &str = "Auto: preload/dedup/compress run in background. \
    ctx_session=memory, ctx_knowledge=facts, ctx_semantic_search=meaning search, \
    ctx_shell raw=true=uncompressed. Details: LEAN-CTX.md";

/// Context Engineering Protocol version reference.
pub const CEP: &str = "CEP v1: 1.ACT FIRST 2.DELTA ONLY (Fn refs) 3.STRUCTURED (+/-/~) \
     4.ONE LINE PER ACTION 5.QUALITY ANCHOR";

/// Output style rule.
pub const INTELLIGENCE: &str =
    "OUTPUT: never echo tool output, no narration comments, show only changed code.";

/// LITM end-of-instructions preference line.
pub const LITM_END: &str = "TOOL PREFERENCE (END): ctx_compose>chain ctx_read>Read ctx_shell>Shell \
     ctx_search>Grep ctx_glob>Glob ctx_tree>ls | Edit/Write/Delete=native";

// ── Output-style compression prompts ───────────────────────────

/// Lite compression — concise, bullet-point output.
pub const LITE_PROMPT: &str = "\
OUTPUT STYLE: concise
- Bullet points over paragraphs
- Skip filler words and hedging (\"I think\", \"probably\", \"it seems\")
- 1-sentence explanations max, then code/action
- No repeating what the user said";

/// Standard compression — dense, atomic fact lines, abbreviations.
pub const STANDARD_PROMPT: &str = "\
OUTPUT STYLE: dense
- Each statement = one atomic fact line
- Use abbreviations: fn, cfg, impl, deps, req, res, ctx, err, ret
- Diff lines only (+/-/~), never repeat unchanged code
- Symbols: → (causes), + (adds), − (removes), ~ (modifies), ∴ (therefore)
- No narration, no filler, no hedging
- BUDGET: ≤200 tokens per response unless code block required";

/// Max compression — expert-terse, telegraph format, symbolic vocabulary.
pub const MAX_PROMPT: &str = "\
OUTPUT STYLE: expert-terse
- Telegraph format: subject-verb-object, drop articles/prepositions
- Symbolic vocabulary: → cause, ∵ because, ∴ therefore, ⊕ add, ⊖ remove, Δ change, ≈ similar, ≠ different, ∈ in/member, ∅ empty/none, ✓ ok, ✗ fail
- Code blocks: untouched (never compress code syntax)
- Each line: max 80 chars
- Zero narration, zero filler
- BUDGET: ≤100 tokens per non-code response";

/// Return the compression prompt text for a given level (empty string for Off).
pub fn compression_text(level: CompressionLevel) -> &'static str {
    match level {
        CompressionLevel::Off => "",
        CompressionLevel::Lite => LITE_PROMPT,
        CompressionLevel::Standard => STANDARD_PROMPT,
        CompressionLevel::Max => MAX_PROMPT,
    }
}

const FULL_NON_SHADOW: &[&str] = &[
    CRITICAL,
    BULLETS,
    NEVER,
    INTENT,
    ANTI,
    PARALLEL,
    AUTO,
    CEP,
    INTELLIGENCE,
    LITM_END,
];

const FULL_SHADOW: &[&str] = &[INTENT, ANTI, PARALLEL, AUTO, CEP, INTELLIGENCE, LITM_END];

const COMPACT_NON_SHADOW: &[&str] = &[CRITICAL, BULLETS, NEVER, INTENT, ANTI, PARALLEL];

const COMPACT_SHADOW: &[&str] = &[INTENT, ANTI, PARALLEL];

/// Selects the profile (FULL vs COMPACT) and the wrapping style (markers,
/// headers, footers) for `render()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrapper {
    /// **Dedicated rule file.**  FULL profile.  Wrapped with `START_MARK`,
    /// `<!-- version: N -->`, and `END_MARK`.  Non-shadow includes the
    /// `CRITICAL` banner before the body.  The whole file is lean-ctx–owned;
    /// the injection layer detects staleness by parsing the version comment.
    Dedicated,

    /// **Shared file section** (appended to AGENTS.md, GEMINI.md, etc.).
    /// COMPACT profile.  Same marker wrapping for find/replace within a
    /// larger shared file.  Non-shadow includes `## Tool Mapping` header.
    Shared,

    /// **MCP session instructions.**  COMPACT profile.  No markers or
    /// headers — bare content used inline in per-session MCP instructions.
    Bare,
}

/// Render lean-ctx rules for a given wrapper, shadow mode, and compression level.
///
/// * `shadow` — when true, tool-mapping sections (BULLETS, NEVER,
///   CRITICAL banner, "## Tool Mapping" header) are omitted.
/// * `wrapper` — selects the profile (FULL / COMPACT) and wrapping style.
/// * `level` — selects the output-style compression prompt (Lite / Standard /
///   Max) which is appended to the body. `Off` omits it.
pub fn render(shadow: bool, wrapper: Wrapper, level: CompressionLevel) -> String {
    let profile = match (wrapper, shadow) {
        (Wrapper::Dedicated, false) => FULL_NON_SHADOW,
        (Wrapper::Dedicated, true) => FULL_SHADOW,
        (_, false) => COMPACT_NON_SHADOW,
        (_, true) => COMPACT_SHADOW,
    };

    let mut body = profile.join("\n\n");

    // Append compression prompt for active levels
    let compression = compression_text(level);
    if !compression.is_empty() {
        body.push('\n');
        body.push_str(compression);
    }

    if matches!(wrapper, Wrapper::Bare) {
        return body;
    }

    let version_line = format!("<!-- version: {RULES_VERSION} -->");

    format!("{START_MARK}\n{version_line}\n\n{body}\n{END_MARK}")
}
// ============================================================
// RULES FILE — centralized interface for reading rule files
// ============================================================

/// A parsed lean-ctx rules section from a file on disk.
///
/// Handles version detection, content boundary discovery, and prefix/suffix
/// extraction.  This is the **only** place that parses `START_MARK` / version
/// comments — every consumer (injection, drift detection, status reporting)
/// goes through this struct.
#[derive(Debug)]
pub struct RulesFile<'a> {
    content: &'a str,
    /// Byte offset of `START_MARK` (or the first old-format marker found).
    start: Option<usize>,
    /// Byte offset of `END_MARK`.
    end: Option<usize>,
    /// Parsed version number (0 if no `<!-- version: N -->` comment found).
    version: usize,
}

/// Parse the version number from the first `<!-- version: N -->` comment
/// found at or after `search_start`.
fn parse_version_number(s: &str) -> Option<usize> {
    let prefix = "<!-- version: ";
    let vs = s.find(prefix)?;
    let num_start = vs + prefix.len();
    let end = s[num_start..].find(" -->")?;
    s[num_start..num_start + end].parse().ok()
}

impl<'a> RulesFile<'a> {
    /// Parse `content`, scanning for `START_MARK` and version comment.
    ///
    /// * `START_MARK` not found → `has_content() = false`, version = 0.
    /// * `START_MARK` found but no version → `has_content() = true`, version = 0
    ///   (assume older than current → needs update).
    pub fn parse(content: &'a str) -> Self {
        let start = content.find(START_MARK);
        let version = start
            .and_then(|s| parse_version_number(&content[s + START_MARK.len()..]))
            .unwrap_or(0);
        let end = content.find(END_MARK);
        RulesFile {
            content,
            start,
            end,
            version,
        }
    }

    /// Whether the file carries any lean-ctx rules content.
    pub fn has_content(&self) -> bool {
        self.start.is_some()
    }

    /// The detected version (0 if no version marker — treat as older than
    /// `RULES_VERSION`).
    pub fn version(&self) -> usize {
        self.version
    }

    /// Whether the file's version is at least `RULES_VERSION`.
    pub fn is_current(&self) -> bool {
        self.version >= RULES_VERSION
    }

    /// Content before the first `START_MARK` (user content / frontmatter).
    /// Returns an empty string if no start marker was found.
    pub fn prefix(&self) -> &'a str {
        self.start.map_or("", |s| self.content[..s].trim())
    }

    /// Content after the last `END_MARK` (user content after the lean-ctx
    /// block).  Returns an empty string if no end marker was found.
    pub fn suffix(&self) -> &'a str {
        self.end
            .map_or("", |e| self.content[e + END_MARK.len()..].trim())
    }

    /// Merge freshly-rendered rules into this file.
    ///
    /// * If a lean-ctx section exists → replaces content between `START_MARK`
    ///   and `END_MARK`, preserving user content before/after.
    /// * If no section exists → appends fresh content at the end.
    pub fn merged(&self, shadow: bool, wrapper: Wrapper, level: CompressionLevel) -> String {
        let fresh = render(shadow, wrapper, level);
        if self.start.is_some() {
            let before = self.prefix();
            let after = self.suffix();
            let mut out = String::new();
            if !before.is_empty() {
                out.push_str(before);
                out.push('\n');
                out.push('\n');
            }
            out.push_str(&fresh);
            if !after.is_empty() {
                out.push('\n');
                out.push('\n');
                out.push_str(after);
            }
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out
        } else {
            // No existing section — append.
            let trimmed = self.content.trim_end();
            let mut out = trimmed.to_string();
            if !out.is_empty() {
                out.push('\n');
                out.push('\n');
            }
            out.push_str(&fresh);
            out
        }
    }

    /// Create initial rules content (no existing section to merge with).
    pub fn initial(shadow: bool, wrapper: Wrapper, level: CompressionLevel) -> String {
        render(shadow, wrapper, level)
    }

    // ── Delete ─────────────────────────────────────────────────

    /// Strip the lean-ctx section, keeping only user content before/after.
    pub fn without_section(&self) -> String {
        if let Some(start_pos) = self.start {
            let before = self.content[..start_pos].trim();
            let after = self.suffix();
            let mut out = String::new();
            if !before.is_empty() {
                out.push_str(before);
                out.push('\n');
            }
            if !after.is_empty() {
                out.push('\n');
                out.push_str(after);
            }
            out
        } else {
            self.content.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Constants ---

    #[test]
    fn bullets_uses_ctx_shell() {
        assert!(BULLETS.contains("ctx_shell"));
        assert!(!BULLETS.contains("lean-ctx -c"));
        assert!(!BULLETS.contains("ctx_edit"));
    }

    #[test]
    fn sections_not_empty() {
        assert!(!BULLETS.is_empty());
        assert!(!NEVER.is_empty());
        assert!(!INTENT.is_empty());
        assert!(!ANTI.is_empty());
        assert!(!PARALLEL.is_empty());
        assert!(!AUTO.is_empty());
        assert!(!CEP.is_empty());
        assert!(!INTELLIGENCE.is_empty());
        assert!(!LITM_END.is_empty());
        assert!(!CRITICAL.is_empty());
    }

    #[test]
    fn intent_contains_ctx_compose() {
        assert!(INTENT.contains("ctx_compose"));
    }

    #[test]
    fn anti_contains_do_not() {
        assert!(ANTI.contains("do NOT"));
    }

    #[test]
    fn parallel_contains_parallel() {
        assert!(PARALLEL.contains("PARALLEL"));
    }

    // --- render() — Dedicated ---

    #[test]
    fn dedicated_has_markers_and_version() {
        let out = render(false, Wrapper::Dedicated, CompressionLevel::Off);
        assert!(out.contains(START_MARK));
        assert!(out.contains(&format!("<!-- version: {RULES_VERSION} -->")));
        assert!(out.contains(END_MARK));
        assert!(out.contains(BULLETS));
        assert!(out.contains(NEVER));
        assert!(out.contains("CRITICAL"));
    }

    #[test]
    fn dedicated_shadow_omits_mapping() {
        let out = render(true, Wrapper::Dedicated, CompressionLevel::Off);
        assert!(out.contains(START_MARK));
        assert!(
            !out.contains("MANDATORY MAPPING"),
            "shadow must not contain BULLETS"
        );
        assert!(!out.contains(NEVER), "shadow must not contain NEVER");
        assert!(
            !out.contains("CRITICAL"),
            "shadow must not contain CRITICAL"
        );
        assert!(
            out.contains(INTENT),
            "shadow must keep non-mapping sections"
        );
    }

    #[test]
    fn dedicated_litm_structure() {
        let out = render(false, Wrapper::Dedicated, CompressionLevel::Off);
        let lines: Vec<&str> = out.lines().collect();
        let first_5 = lines[..5.min(lines.len())].join("\n");
        assert!(
            first_5.contains("CRITICAL") || first_5.contains("MUST"),
            "LITM: MUST/CRITICAL instruction near start"
        );
        // LITM_END or NEVER should appear in the final content lines (before END_MARK).
        let tail = lines[lines.len().saturating_sub(8)..].join("\n");
        assert!(
            tail.contains("PREFERENCE") || tail.contains("NEVER"),
            "LITM: reinforcement near end, tail={tail:?}"
        );
    }

    // --- render() — Shared ---

    #[test]
    fn shared_has_markers_and_header() {
        let out = render(false, Wrapper::Shared, CompressionLevel::Off);
        assert!(out.contains(START_MARK));
        assert!(out.contains(END_MARK));
        assert!(out.contains("MANDATORY MAPPING"));
        assert!(out.contains(BULLETS));
    }

    #[test]
    fn shared_shadow_omits_mapping() {
        let out = render(true, Wrapper::Shared, CompressionLevel::Off);
        assert!(out.contains(START_MARK));
        assert!(
            !out.contains("MANDATORY MAPPING"),
            "shadow must not have header"
        );
        assert!(
            !out.contains("MANDATORY MAPPING"),
            "shadow must not contain BULLETS"
        );
    }

    // --- render() — Bare ---

    #[test]
    fn bare_has_no_markers() {
        let out = render(false, Wrapper::Bare, CompressionLevel::Off);
        assert!(!out.contains(START_MARK), "Bare must not have START_MARK");
        assert!(!out.contains(END_MARK), "Bare must not have END_MARK");
        assert!(!out.contains("<!-- version:"), "Bare must not have version");
        assert!(out.contains(BULLETS));
        assert!(out.contains(NEVER));
    }

    #[test]
    fn bare_shadow_only_read_modes() {
        let out = render(true, Wrapper::Bare, CompressionLevel::Off);
        assert!(!out.contains(NEVER), "shadow Bare must not have NEVER");
        assert!(
            !out.contains("MANDATORY MAPPING"),
            "shadow Bare must not have BULLETS"
        );
    }

    // --- Compression level tests ---

    #[test]
    fn render_includes_lite_prompt() {
        let out = render(false, Wrapper::Bare, CompressionLevel::Lite);
        assert!(out.contains("OUTPUT STYLE: concise"));
        assert!(out.contains("Bullet points"));
    }

    #[test]
    fn render_includes_standard_prompt() {
        let out = render(false, Wrapper::Bare, CompressionLevel::Standard);
        assert!(out.contains("OUTPUT STYLE: dense"));
        assert!(out.contains("atomic fact"));
    }

    #[test]
    fn render_includes_max_prompt() {
        let out = render(false, Wrapper::Bare, CompressionLevel::Max);
        assert!(out.contains("OUTPUT STYLE: expert-terse"));
        assert!(out.contains("Telegraph"));
    }

    #[test]
    fn render_off_excludes_compression() {
        let out = render(false, Wrapper::Bare, CompressionLevel::Off);
        assert!(!out.contains("OUTPUT STYLE:"));
    }

    #[test]
    fn compression_text_matches_level() {
        assert!(compression_text(CompressionLevel::Off).is_empty());
        assert!(compression_text(CompressionLevel::Lite).contains("Bullet"));
        assert!(compression_text(CompressionLevel::Standard).contains("fn, cfg"));
        assert!(compression_text(CompressionLevel::Max).contains("Telegraph"));
    }

    // --- Wrapper round-trip ---

    #[test]
    fn all_wrappers_produce_output() {
        for shadow in [false, true] {
            for wrapper in [Wrapper::Dedicated, Wrapper::Shared, Wrapper::Bare] {
                let out = render(shadow, wrapper, CompressionLevel::Off);
                assert!(!out.is_empty(), "{wrapper:?} shadow={shadow} is empty");
            }
        }
    }

    // --- RulesFile ---

    #[test]
    fn rules_file_parses_version() {
        let content = format!(
            "stuff before\n{START_MARK}\n<!-- version: {RULES_VERSION} -->\n\nbody\n{END_MARK}\nstuff after"
        );
        let f = RulesFile::parse(&content);
        assert!(f.has_content());
        assert_eq!(f.version(), RULES_VERSION);
        assert!(f.is_current());
        assert!(f.prefix().contains("stuff before"));
        assert!(f.suffix().contains("stuff after"));
    }

    #[test]
    fn rules_file_no_version_defaults_to_zero() {
        let content = format!("{START_MARK}\nbody\n{END_MARK}");
        let f = RulesFile::parse(&content);
        assert!(f.has_content());
        assert_eq!(f.version(), 0);
        assert!(!f.is_current());
    }

    #[test]
    fn rules_file_no_start_marker_no_content() {
        let f = RulesFile::parse("just user stuff");
        assert!(!f.has_content());
        assert_eq!(f.version(), 0);
    }

    #[test]
    fn rules_file_merged_replaces_section() {
        let content =
            format!("before\n{START_MARK}\n<!-- version: 1 -->\n\nold\n{END_MARK}\nafter");
        let f = RulesFile::parse(&content);
        let merged = f.merged(false, Wrapper::Shared, CompressionLevel::Off);
        assert!(merged.contains("before"), "prefix preserved");
        assert!(merged.contains("after"), "suffix preserved");
        assert!(!merged.contains("old"), "old content replaced");
        assert!(merged.contains(&format!("<!-- version: {RULES_VERSION} -->")));
    }

    #[test]
    fn rules_file_merged_appends_when_no_section() {
        let content = "user content";
        let f = RulesFile::parse(content);
        assert!(!f.has_content());
        let merged = f.merged(false, Wrapper::Bare, CompressionLevel::Off);
        assert!(merged.contains("user content"));
        assert!(merged.contains(BULLETS));
    }

    #[test]
    fn rules_file_without_section_strips_content() {
        let content =
            format!("header\n{START_MARK}\n<!-- version: 1 -->\n\nbody\n{END_MARK}\nfooter");
        let f = RulesFile::parse(&content);
        let stripped = f.without_section();
        assert!(stripped.contains("header"));
        assert!(stripped.contains("footer"));
        assert!(!stripped.contains("body"));
        assert!(!stripped.contains(START_MARK));
    }

    #[test]
    fn rules_file_without_section_noop_when_no_content() {
        let content = "just user text";
        let f = RulesFile::parse(content);
        assert_eq!(f.without_section(), content);
    }
}
