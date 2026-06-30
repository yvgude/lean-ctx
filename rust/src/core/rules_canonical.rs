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

/// Owner banner placed as the first line of the project-level `LEAN-CTX.md`
/// artifact (`<repo>/LEAN-CTX.md`, `rust/LEAN-CTX.md`). Marks the whole file as
/// lean-ctx-owned so uninstall can remove it wholesale; the writer
/// (`hooks::ensure_project_agents_integration`), the regenerator
/// (`gen_rules` example) and the drift gate all share this one literal.
pub const PROJECT_LEAN_CTX_OWNED_MARKER: &str = "<!-- lean-ctx-owned: PROJECT-LEAN-CTX.md v1 -->";

/// Closing marker that ends a lean-ctx rule section.
pub const END_MARK: &str = "<!-- /lean-ctx-rules -->";

/// Markers of the heavy compression / output-style block — the per-turn payload
/// that drives cross-channel duplication (#684/#548).
///
/// `render()` wraps the compression prompt in these markers for **persistent
/// carriers** (the `Dedicated` and `Shared` wrappers, i.e. every injected rule
/// file). This is the single carrier/marker model: coverage and dedup
/// (`core::rules_channel`, `cli::rules_dedup`) detect and thin the payload by
/// these markers, so the writer and the readers can never disagree again. The
/// ephemeral `Bare` MCP-instructions channel deliberately omits the markers —
/// its inclusion is *governed* by carrier coverage (`client_autoloads_compression`),
/// so a per-session marker would be pure noise.
pub const COMPRESSION_BLOCK_START: &str = "<!-- lean-ctx-compression -->";

/// Closing marker of the compression / output-style block (see
/// [`COMPRESSION_BLOCK_START`]).
pub const COMPRESSION_BLOCK_END: &str = "<!-- /lean-ctx-compression -->";

/// Current rules version (monotonically increasing integer).  Embedded as
/// `<!-- version: {RULES_VERSION} -->` right after `START_MARK` so the
/// injection layer can parse it and decide whether a file is up-to-date.
///
/// History: v3 (#609) adds the `AGENT_LOOP` taxonomy + `NAV_PARADOX` guidance to
/// the FULL profile and a compact one-liner to COMPACT. v4 adds recovery guidance
/// so agents learn the (MCP-optional) decompression paths proactively instead of
/// re-reading compressed output line-by-line — verbose [`RECOVER`] in FULL and the
/// terse one-liner [`RECOVER_COMPACT`] in COMPACT (the cold-handshake budget).
/// Bumping it forces every committed `LEAN-CTX.md` artifact to be regenerated
/// (see `tests/rules_drift.rs`).
pub const RULES_VERSION: usize = 4;

/// Banner placed at the top of dedicated rule files (non-shadow only).
pub const CRITICAL: &str = "CRITICAL: ALWAYS use lean-ctx ctx_* tools instead of native equivalents. \
     This is NOT optional.";

/// Weak-model adherence nudge (#1067 / GH #593). Smaller models (e.g. GLM 5.2 in
/// Windsurf) often read the tool-mapping rules yet still answer from memory or
/// reach for a built-in tool instead of emitting an actual MCP call — so
/// `lean-ctx watch` stays empty and the layer looks "off". This one imperative
/// line makes the *act of invoking* a ctx_* tool the explicit first step. It
/// lives in the dedicated rule files of *every* agent (`FULL_NON_SHADOW`), which
/// is what #1067 asks for; the Windsurf project template (`windsurfrules.txt`)
/// carries its own copy. It is omitted where it would be dead weight: shadow
/// mode enforces routing at the call layer, and the `Bare`/instructions channel
/// is capped separately.
pub const MUST_INVOKE: &str = "MUST actually CALL the ctx_* MCP tools, not just describe them: \
    before reading, searching, or running a shell command, emit the matching ctx_* tool call \
    (ctx_compose first, then ctx_read / ctx_search / ctx_shell).";

/// Compact bullet-list mapping (for both dedicated and shared contexts).
pub const BULLETS: &str = "\
MANDATORY MAPPING (4 core redirects first):\n\
• Read/cat -> ctx_read(path, mode)\n\
• Grep -> ctx_search(pattern, path)\n\
• Shell/bash -> ctx_shell(command)\n\
• Glob/find -> ctx_glob(pattern)\n\
• ls/find -> ctx_tree(path, depth)";

/// One-line admonition reinforcing the tool preference.  Always right after
/// `BULLETS` in both profiles.
pub const NEVER: &str = "NEVER use native Read/Grep/Shell/Glob when a ctx_* equivalent \
    exists. SELF-CORRECT: the moment you reach for a native Read/Grep/Shell/Glob, stop \
    and call the ctx_* tool instead.";

/// Intent-to-tool playbook — maps common agent questions to the right tool.
pub const INTENT: &str = "\
Tool selection by intent:\n\
• Understand code / find answers / before editing -> ctx_compose (call FIRST)\n\
• Read a file -> ctx_read(path, mode=signatures|map|full)\n\
• Edit code you've read -> ctx_patch (hash-anchored, no exact-recall; read mode=anchored first)\n\
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
ctx_compose bundles multiple lookups into one call; for anything it doesn't\n\
cover, batch independent reads/searches together.";

/// Agent-loop tool taxonomy (#609). Names each phase of the gather → act →
/// verify loop an agent actually runs in and the one lean-ctx tool that serves
/// it, so the agent maps its *current* intent to a call instead of defaulting to
/// a full-file read. Complements `INTENT` (lookup framing) with loop framing.
pub const AGENT_LOOP: &str = "\
AGENT LOOP (phase -> tool):\n\
• Orient — understand before acting -> ctx_compose\n\
• Find — exact symbol by name -> ctx_symbol\n\
• Read — a file, structurally -> ctx_read(mode=signatures|map)\n\
• Locate — a pattern across files -> ctx_search\n\
• Trace — callers / callees / blast radius -> ctx_callgraph\n\
• Verify — after an edit -> ctx_shell(test/build) + native lints";

/// Navigation-paradox guidance (#609): reading more is not understanding more.
/// Steers semantic questions to BM25 + meaning search and reserves the call/dep
/// graph for genuinely hidden architectural edges, so agents stop paging whole
/// files just to "get context".
pub const NAV_PARADOX: &str = "\
NAVIGATION PARADOX: reading more ≠ understanding more.\n\
• Semantic question (\"where/how is X handled?\") -> ctx_search (BM25) + ctx_semantic_search (meaning), not whole-file reads\n\
• Hidden architectural deps (who calls this, what breaks) -> ctx_callgraph / ctx_graph — for these only\n\
• Navigate structure (signatures, symbols) before reading entire files";

/// One-line condensation of `AGENT_LOOP` + `NAV_PARADOX` for the COMPACT profile
/// (shared files + the per-session Bare/MCP channel). Deliberately terse so the
/// Bare skeleton stays within `instructions::INSTRUCTION_CAP_TOKENS`.
pub const LOOP_NAV_COMPACT: &str = "\
AGENT LOOP: Orient(ctx_compose) → Find(ctx_symbol) → Read(ctx_read) → Locate(ctx_search) → Trace(ctx_callgraph) → Verify(ctx_shell). \
Reading more ≠ understanding more: semantic Qs -> ctx_search/ctx_semantic_search; hidden deps -> ctx_callgraph/ctx_graph only.";

/// One-line automation reminder.
pub const AUTO: &str = "Auto: preload/dedup/compress run in background. \
    ctx_session=memory, ctx_knowledge=facts, ctx_semantic_search=meaning search, \
    ctx_shell raw=true=uncompressed. Details: LEAN-CTX.md";

/// Recovery vocabulary (verbose, FULL profile). lean-ctx compression is fully
/// reversible (CCR), but agents otherwise only discover the escape hatch reactively
/// from output hints — so they re-read compressed files line-by-line instead of
/// expanding (the "too compressed" complaint). Teaching it proactively in the
/// dedicated rule files fixes that, and the MCP-free path ("read the shown file
/// path with any tool") covers orgs that forbid MCP. The COMPACT/Bare channel
/// carries the terser [`RECOVER_COMPACT`] instead. Mirrors the reactive footers in
/// `ctx_read`/`archive`/`ctx_shell`.
pub const RECOVER: &str = "RECOVER: compressed output is reversible — never re-read line-by-line. \
    Need full/exact? Read the shown file path with any tool (no MCP), or \
    ctx_read(mode=full|raw=true); [Archived]/tee/firewall → ctx_expand(id=...).";

/// Terse COMPACT/Bare variant of [`RECOVER`]. The cold first-contact handshake
/// renders the COMPACT profile, so it carries this one-liner to stay within the
/// static char/token budget (`tests/intensive_benchmarks.rs`, `instructions.rs`)
/// — the verbose block ships in the FULL dedicated rule files. Keeps the two
/// primary MCP-optional paths and the "never line-by-line" rule; the
/// `[Archived]`/tee → `ctx_expand` path is still taught reactively by the output
/// footers. Must keep the `(no MCP)` clause (asserted in tests).
pub const RECOVER_COMPACT: &str = "RECOVER: compression is reversible — read the shown path \
    (no MCP) or ctx_read(raw=true), never re-read line-by-line.";

/// Context Engineering Protocol version reference.
pub const CEP: &str = "CEP v1: 1.ACT FIRST 2.DELTA ONLY (Fn refs) 3.STRUCTURED (+/-/~) \
     4.ONE LINE PER ACTION 5.QUALITY ANCHOR";

/// Output style rule.
pub const INTELLIGENCE: &str =
    "OUTPUT: never echo tool output, no narration comments, show only changed code.";

/// LITM end-of-instructions preference line.
pub const LITM_END: &str = "TOOL PREFERENCE (END): ctx_compose>chain ctx_read>Read ctx_shell>Shell \
     ctx_search>Grep ctx_glob>Glob ctx_tree>ls | Edit/Write/Delete=native";

/// Minimal rules body for shadow mode (#963). Under shadow-mode interception
/// native Read/Grep/Shell/Glob calls are transparently routed to ctx_*, so the
/// tool-mapping and "use ctx_* instead of native" guidance is dead weight — the
/// enforcement happens at the call layer, not in the prompt. Only the lean-ctx
/// tools that have *no* native trigger to intercept still need advertising.
pub const SHADOW_MINIMAL: &str = "\
lean-ctx shadow mode: native file/search/shell calls auto-route to ctx_* — no tool-mapping needed.\n\
Exclusive tools (no native trigger): ctx_compose (understand code, call first), ctx_symbol (exact symbol), ctx_callgraph (callers), ctx_semantic_search (by meaning), ctx_knowledge / ctx_session (memory).";

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
    MUST_INVOKE,
    BULLETS,
    NEVER,
    INTENT,
    AGENT_LOOP,
    ANTI,
    NAV_PARADOX,
    PARALLEL,
    AUTO,
    RECOVER,
    CEP,
    INTELLIGENCE,
    LITM_END,
];

// #963: shadow profiles collapse to the irreducible minimum. Every routing
// section (INTENT/ANTI/PARALLEL/AUTO/CEP/LITM_END) is redundant once native
// calls are intercepted; only SHADOW_MINIMAL (exclusive tools) plus the output
// style survive. Footprint reduction is provable via the #959 delta harness.
const FULL_SHADOW: &[&str] = &[SHADOW_MINIMAL, INTELLIGENCE];

const COMPACT_NON_SHADOW: &[&str] = &[
    CRITICAL,
    BULLETS,
    NEVER,
    INTENT,
    LOOP_NAV_COMPACT,
    ANTI,
    PARALLEL,
    RECOVER_COMPACT,
];

const COMPACT_SHADOW: &[&str] = &[SHADOW_MINIMAL];

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

    // Append the compression / output-style prompt for active levels. Persistent
    // carriers (Dedicated, Shared) wrap it in the canonical COMPRESSION_BLOCK
    // markers so coverage/dedup (rules_channel, rules_dedup) can detect and thin
    // it; the ephemeral Bare MCP channel keeps it unmarked (#684/#548).
    let compression = compression_text(level);
    if !compression.is_empty() {
        body.push('\n');
        if matches!(wrapper, Wrapper::Bare) {
            body.push_str(compression);
        } else {
            body.push_str(COMPRESSION_BLOCK_START);
            body.push('\n');
            body.push_str(compression);
            body.push('\n');
            body.push_str(COMPRESSION_BLOCK_END);
        }
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

    /// The lean-ctx block on disk, from `START_MARK` through `END_MARK`
    /// (inclusive), if both markers are present.
    fn block(&self) -> Option<&'a str> {
        match (self.start, self.end) {
            (Some(s), Some(e)) if e >= s => Some(&self.content[s..e + END_MARK.len()]),
            _ => None,
        }
    }

    /// Whether the on-disk block is already byte-identical (ignoring surrounding
    /// whitespace) to a fresh [`render`] for these parameters.
    ///
    /// [`is_current`](Self::is_current) only compares the embedded
    /// `<!-- version: N -->` against [`RULES_VERSION`], so a change that keeps
    /// the version but alters the rendered body — toggling `shadow_mode`,
    /// switching `compression_level`, or editing a canonical section without a
    /// version bump — would otherwise be skipped by the injector. Callers pair
    /// this with `is_current()` to detect that content/compression drift (#548).
    pub fn block_matches_render(
        &self,
        shadow: bool,
        wrapper: Wrapper,
        level: CompressionLevel,
    ) -> bool {
        match self.block() {
            Some(block) => block.trim() == render(shadow, wrapper, level).trim(),
            None => false,
        }
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

    // --- Agent loop + navigation paradox (#609) ---

    #[test]
    fn agent_loop_names_every_phase() {
        for phase in ["Orient", "Find", "Read", "Locate", "Trace", "Verify"] {
            assert!(AGENT_LOOP.contains(phase), "AGENT_LOOP must name {phase}");
        }
        assert!(AGENT_LOOP.contains("ctx_compose") && AGENT_LOOP.contains("ctx_callgraph"));
    }

    #[test]
    fn nav_paradox_steers_semantic_vs_graph() {
        assert!(
            NAV_PARADOX.contains("ctx_semantic_search"),
            "semantic route"
        );
        assert!(NAV_PARADOX.contains("ctx_callgraph"), "graph route");
        assert!(
            NAV_PARADOX.contains("≠"),
            "must carry the reading≠understanding thesis"
        );
    }

    #[test]
    fn full_profile_carries_loop_and_paradox() {
        let out = render(false, Wrapper::Dedicated, CompressionLevel::Off);
        assert!(out.contains("AGENT LOOP"), "FULL must carry AGENT_LOOP");
        assert!(
            out.contains("NAVIGATION PARADOX"),
            "FULL must carry NAV_PARADOX"
        );
    }

    #[test]
    fn compact_profile_uses_one_liner_not_full_sections() {
        // COMPACT (shared + Bare) carries the condensed one-liner, never the
        // multi-line FULL sections — that keeps the per-session channel lean.
        let out = render(false, Wrapper::Shared, CompressionLevel::Off);
        assert!(
            out.contains(LOOP_NAV_COMPACT),
            "COMPACT must carry one-liner"
        );
        assert!(
            !out.contains("AGENT LOOP (phase -> tool):"),
            "COMPACT must not inline the multi-line AGENT_LOOP block"
        );
        assert!(
            !out.contains("NAVIGATION PARADOX: reading"),
            "COMPACT must not inline the multi-line NAV_PARADOX block"
        );
    }

    #[test]
    fn shadow_omits_loop_and_paradox() {
        // #963: shadow collapses to the irreducible minimum — the routing
        // taxonomy is redundant once native calls are intercepted.
        for wrapper in [Wrapper::Dedicated, Wrapper::Shared] {
            let out = render(true, wrapper, CompressionLevel::Off);
            assert!(!out.contains("AGENT LOOP"), "{wrapper:?} shadow drops loop");
            assert!(
                !out.contains("NAVIGATION PARADOX"),
                "{wrapper:?} shadow drops paradox"
            );
        }
    }

    #[test]
    fn recover_reaches_every_non_shadow_carrier() {
        // The recovery vocabulary must reach FULL *and* COMPACT/Bare so agents
        // never re-read compressed output line-by-line, and every carrier must
        // keep the MCP-free path ("read the shown path") for orgs that ban MCP.
        // FULL carries the verbose RECOVER; COMPACT/Bare carry the terse
        // RECOVER_COMPACT one-liner (cold-handshake budget).
        let full = render(false, Wrapper::Dedicated, CompressionLevel::Off);
        assert!(
            full.contains(RECOVER),
            "FULL non-shadow must carry the verbose RECOVER verbatim"
        );
        for wrapper in [Wrapper::Shared, Wrapper::Bare] {
            let out = render(false, wrapper, CompressionLevel::Off);
            assert!(
                out.contains(RECOVER_COMPACT),
                "{wrapper:?} (COMPACT) must carry RECOVER_COMPACT verbatim"
            );
            assert!(
                !out.contains(RECOVER),
                "{wrapper:?} (COMPACT) must not inline the verbose RECOVER block"
            );
        }
        for wrapper in [Wrapper::Dedicated, Wrapper::Shared, Wrapper::Bare] {
            assert!(
                render(false, wrapper, CompressionLevel::Off).contains("(no MCP)"),
                "{wrapper:?} recovery line must keep the MCP-free path"
            );
        }
        // Shadow stays minimal; the reactive footers still cover recovery there.
        for wrapper in [Wrapper::Dedicated, Wrapper::Shared] {
            let out = render(true, wrapper, CompressionLevel::Off);
            assert!(
                !out.contains(RECOVER) && !out.contains(RECOVER_COMPACT),
                "{wrapper:?} shadow drops all RECOVER guidance"
            );
        }
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
    fn dedicated_shadow_is_minimal() {
        // #963: shadow drops the whole tool-mapping AND routing playbook —
        // interception makes them redundant. Only the exclusive-tool advert and
        // the output style remain.
        let out = render(true, Wrapper::Dedicated, CompressionLevel::Off);
        assert!(out.contains(START_MARK));
        assert!(!out.contains("MANDATORY MAPPING"), "no BULLETS in shadow");
        assert!(!out.contains(NEVER), "no NEVER in shadow");
        assert!(!out.contains("CRITICAL"), "no CRITICAL banner in shadow");
        assert!(
            !out.contains("Tool selection by intent"),
            "routing INTENT block is redundant under interception"
        );
        assert!(
            !out.contains("Anti-patterns") && !out.contains("PARALLEL tool calls"),
            "ANTI/PARALLEL routing guidance is dropped in shadow"
        );
        assert!(
            out.contains("shadow mode") && out.contains("ctx_compose"),
            "shadow keeps the exclusive-tool advert"
        );
        assert!(out.contains(INTELLIGENCE), "shadow keeps the output style");
    }

    #[test]
    fn shadow_is_smaller_than_non_shadow() {
        // The whole point of #963: the shadow body must be a strict reduction.
        let shadow = render(true, Wrapper::Dedicated, CompressionLevel::Off);
        let full = render(false, Wrapper::Dedicated, CompressionLevel::Off);
        assert!(
            shadow.len() < full.len(),
            "shadow ({}) must be smaller than non-shadow ({})",
            shadow.len(),
            full.len()
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

    #[test]
    fn dedicated_carries_weak_model_invoke_nudge() {
        // #1067/GH #593: the "actually CALL ctx_*" nudge must ride every dedicated
        // rule file (Windsurf, Cursor, Claude, …) in non-shadow mode, and must be
        // absent where it is dead weight: shadow mode (call-layer routing) and the
        // Bare/instructions channel (separately capped).
        let dedicated = render(false, Wrapper::Dedicated, CompressionLevel::Off);
        assert!(
            dedicated.contains(MUST_INVOKE),
            "dedicated non-shadow rules must carry the MUST_INVOKE nudge"
        );
        assert!(
            !render(true, Wrapper::Dedicated, CompressionLevel::Off).contains(MUST_INVOKE),
            "shadow mode must not carry the nudge (routing is enforced at the call layer)"
        );
        assert!(
            !render(false, Wrapper::Bare, CompressionLevel::Off).contains(MUST_INVOKE),
            "Bare/instructions channel is capped separately and carries no copy"
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

    // --- Compression marker model (#548 B2) ---

    #[test]
    fn carrier_wrappers_wrap_compression_in_markers() {
        // Persistent carriers must delimit the compression payload so coverage
        // and dedup can detect/thin it (#684/#548).
        for wrapper in [Wrapper::Dedicated, Wrapper::Shared] {
            let out = render(false, wrapper, CompressionLevel::Standard);
            assert!(
                out.contains(COMPRESSION_BLOCK_START) && out.contains(COMPRESSION_BLOCK_END),
                "{wrapper:?} must wrap compression in COMPRESSION_BLOCK markers"
            );
            // The marked region must actually contain the prompt body.
            let start = out.find(COMPRESSION_BLOCK_START).unwrap();
            let end = out.find(COMPRESSION_BLOCK_END).unwrap();
            assert!(start < end, "{wrapper:?}: start marker precedes end marker");
            assert!(out[start..end].contains("OUTPUT STYLE: dense"));
        }
    }

    #[test]
    fn bare_wrapper_emits_compression_without_markers() {
        // The ephemeral MCP channel keeps the payload unmarked — its inclusion is
        // governed by carrier coverage, so per-session markers would be noise.
        let out = render(false, Wrapper::Bare, CompressionLevel::Standard);
        assert!(out.contains("OUTPUT STYLE: dense"));
        assert!(!out.contains(COMPRESSION_BLOCK_START));
        assert!(!out.contains(COMPRESSION_BLOCK_END));
    }

    #[test]
    fn compression_off_emits_no_markers_in_any_wrapper() {
        for wrapper in [Wrapper::Dedicated, Wrapper::Shared, Wrapper::Bare] {
            let out = render(false, wrapper, CompressionLevel::Off);
            assert!(
                !out.contains(COMPRESSION_BLOCK_START) && !out.contains(COMPRESSION_BLOCK_END),
                "{wrapper:?}: Off must emit no compression markers"
            );
        }
    }

    #[test]
    fn rendered_carrier_block_is_seen_as_carrying_compression() {
        // The detection helper that coverage/dedup rely on must agree with the
        // writer's output (the bug this slice fixes: it previously never did).
        let dedicated = render(false, Wrapper::Dedicated, CompressionLevel::Lite);
        assert!(crate::core::rules_channel::carries_full_rules(&dedicated));
        assert!(dedicated.contains(COMPRESSION_BLOCK_START));
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
    fn block_matches_render_true_for_fresh_render() {
        let fresh = render(false, Wrapper::Dedicated, CompressionLevel::Off);
        let content = format!("user before\n{fresh}\nuser after");
        let f = RulesFile::parse(&content);
        assert!(f.is_current(), "fresh render carries the current version");
        assert!(
            f.block_matches_render(false, Wrapper::Dedicated, CompressionLevel::Off),
            "an unchanged block must compare equal to a fresh render"
        );
    }

    #[test]
    fn block_matches_render_false_on_compression_change() {
        // Body rendered at Off, then asked whether it matches a Max render:
        // the version is identical but the compression payload differs (#548).
        let content = render(false, Wrapper::Dedicated, CompressionLevel::Off);
        let f = RulesFile::parse(&content);
        assert!(f.is_current());
        assert!(
            !f.block_matches_render(false, Wrapper::Dedicated, CompressionLevel::Max),
            "a compression-level change must be detected as drift"
        );
    }

    #[test]
    fn block_matches_render_false_on_shadow_change() {
        let content = render(false, Wrapper::Dedicated, CompressionLevel::Lite);
        let f = RulesFile::parse(&content);
        assert!(
            !f.block_matches_render(true, Wrapper::Dedicated, CompressionLevel::Lite),
            "a shadow-mode toggle must be detected as drift"
        );
    }

    #[test]
    fn block_matches_render_false_without_block() {
        let f = RulesFile::parse("plain user content, no markers");
        assert!(!f.block_matches_render(false, Wrapper::Dedicated, CompressionLevel::Off));
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

    #[test]
    fn bullets_lead_with_four_core_redirects() {
        // Most-used routes (Read/Grep/Shell/Glob) lead; ls->ctx_tree trails.
        let read = BULLETS.find("ctx_read").expect("ctx_read mapping present");
        let search = BULLETS
            .find("ctx_search")
            .expect("ctx_search mapping present");
        let shell = BULLETS
            .find("ctx_shell")
            .expect("ctx_shell mapping present");
        let glob = BULLETS.find("ctx_glob").expect("ctx_glob mapping present");
        let tree = BULLETS.find("ctx_tree").expect("ctx_tree mapping present");
        assert!(
            read < search && search < shell && shell < glob && glob < tree,
            "core redirects (read<search<shell<glob) must precede ctx_tree"
        );
    }

    #[test]
    fn never_carries_self_correction() {
        // Self-correction reinforces the redirect harder than a bare prohibition.
        assert!(
            NEVER.contains("SELF-CORRECT"),
            "NEVER must teach self-correction"
        );
        assert!(
            NEVER.contains("call"),
            "NEVER must spell out the corrective action"
        );
    }

    #[test]
    fn critical_names_ctx_family() {
        assert!(
            CRITICAL.contains("ctx_*"),
            "CRITICAL must name the ctx_* family"
        );
    }
}
