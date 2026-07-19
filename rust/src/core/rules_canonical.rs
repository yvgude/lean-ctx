//! Canonical rules source — single source of truth for all lean-ctx guidance.
//!
//! All content is declared as `pub const` at the top. Three profiles (LONGFORM,
//! FULL, COMPACT) define which sections compose each output format. Four
//! wrappers (Longform, Dedicated, Shared, Bare) select the profile and wrapping
//! style. One `render()` function assembles everything, including the
//! compression-level output-style prompt (Lite / Standard / Max).
//!
//! Profile economics (#578): every *injected* file bills its tokens on every
//! single session, so FULL (dedicated rule files) and COMPACT (shared files +
//! MCP instructions) stay tight. LONGFORM is carried only by the project
//! `LEAN-CTX.md`, which agents open on demand — it keeps the verbose teaching
//! sections (loop taxonomy, navigation paradox, recovery vocabulary, CEP).
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
/// re-reading compressed output line-by-line. v5 (#578) is the self-footprint
/// diet: the injected FULL profile drops to ~470 tokens (the intent playbook
/// absorbs the loop taxonomy and navigation paradox; the verbose
/// `AGENT_LOOP`/`NAV_PARADOX`/`RECOVER`/`CEP` sections move to the LONGFORM
/// profile that only the on-demand project `LEAN-CTX.md` carries).
/// v6: HookCovered profile strengthened — ctx_compose-first priority, imperative
/// session memory requirement, ctx_callgraph/ctx_search(semantic) promoted from
/// optional to mandatory. Fixes agents falling back to native Read+Grep for
/// exploration on hook-covered hosts (Cursor).
/// Bumping it forces every committed `LEAN-CTX.md` artifact to be regenerated
/// (see `tests/rules_drift.rs`) and every injected user file to resync.
pub const RULES_VERSION: usize = 8;

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
pub const MUST_INVOKE: &str = "ACTUALLY EMIT the ctx_* tool call (ctx_compose first) — \
    describing a tool is not calling it.";

/// Compact bullet-list mapping (for both dedicated and shared contexts).
pub const BULLETS: &str = "\
MANDATORY MAPPING:\n\
• Read/cat -> ctx_read(path, mode)\n\
• Grep -> ctx_search(pattern, path)\n\
• Shell/bash -> ctx_shell(command)\n\
• Glob/find -> ctx_glob(pattern)\n\
• ls/find -> ctx_tree(path, depth)";

/// One-line admonition reinforcing the tool preference.  Always right after
/// `BULLETS` in both profiles.
pub const NEVER: &str = "NEVER use native Read/Grep/Shell/Glob when a ctx_* equivalent \
    exists. SELF-CORRECT: the moment you reach for one, stop and call the ctx_* tool instead.";

/// Intent-to-tool playbook — maps common agent questions to the right tool.
/// Since v5 it also absorbs the agent-loop phases (#609) and the navigation
/// paradox one-liner, replacing the separate verbose sections in the injected
/// profile (they stay verbatim in LONGFORM).
pub const INTENT: &str = "\
Tool selection by intent:\n\
• Orient / understand code (call FIRST) -> ctx_compose\n\
• Read a file -> ctx_read(path, mode=signatures|map|full); edit after reading -> ctx_patch\n\
• Exact symbol -> ctx_search(action=symbol); pattern -> ctx_search; by meaning -> ctx_search(action=semantic)\n\
• Files by glob -> ctx_glob; structure -> ctx_tree; callers/impact -> ctx_callgraph\n\
• Verify after edits -> ctx_shell(test/build); memory -> ctx_session / ctx_knowledge\n\
Semantic questions -> search tools, not whole-file reads: reading more ≠ understanding more.";

/// Anti-patterns that waste tokens and round-trips.
pub const ANTI: &str = "\
Anti-patterns — do NOT:\n\
• Chain ctx_search -> ctx_read -> ctx_search(action=symbol) — one ctx_compose replaces all three\n\
• Use ctx_read(mode=full) for orientation — use mode=signatures\n\
• Use ctx_callgraph/ctx_graph for const/static/variable refs — they track call\n\
  edges and file deps only; use ctx_search instead";

/// Encourages parallel tool calls to reduce round-trips.
pub const PARALLEL: &str = "\
PARALLEL: fire independent tool calls in the SAME turn — ctx_compose bundles \
multiple lookups into one call.";

/// Agent-loop tool taxonomy (#609). Names each phase of the gather → act →
/// verify loop an agent actually runs in and the one lean-ctx tool that serves
/// it. Since v5 (#578) LONGFORM-only — the injected profiles carry the phases
/// folded into `INTENT`.
pub const AGENT_LOOP: &str = "\
AGENT LOOP (phase -> tool):\n\
• Orient — understand before acting -> ctx_compose\n\
• Find — exact symbol by name -> ctx_search(action=symbol)\n\
• Read — a file, structurally -> ctx_read(mode=signatures|map)\n\
• Locate — a pattern across files -> ctx_search\n\
• Trace — callers / callees / blast radius -> ctx_callgraph\n\
• Verify — after an edit -> ctx_shell(test/build) + native lints";

/// Navigation-paradox guidance (#609): reading more is not understanding more.
/// Steers semantic questions to BM25 + meaning search and reserves the call/dep
/// graph for genuinely hidden architectural edges. Since v5 LONGFORM-only —
/// `INTENT` carries the one-line thesis in the injected profiles.
pub const NAV_PARADOX: &str = "\
NAVIGATION PARADOX: reading more ≠ understanding more.\n\
• Semantic question (\"where/how is X handled?\") -> ctx_search (BM25) + ctx_search(action=semantic) (meaning), not whole-file reads\n\
• Hidden architectural deps (who calls this, what breaks) -> ctx_callgraph / ctx_graph — for these only\n\
• Navigate structure (signatures, symbols) before reading entire files";

/// One-line automation reminder.
pub const AUTO: &str = "Long shell jobs: ctx_shell(run_in_background=true), then poll job_id. \
    ctx_session=memory; full guide: LEAN-CTX.md";

/// Recovery vocabulary (verbose, LONGFORM profile). lean-ctx compression is fully
/// reversible (CCR), but agents otherwise only discover the escape hatch reactively
/// from output hints — so they re-read compressed files line-by-line instead of
/// expanding (the "too compressed" complaint). The MCP-free path ("read the shown
/// file path with any tool") covers orgs that forbid MCP. Since v5 every injected
/// profile (FULL + COMPACT/Bare) carries the terser [`RECOVER_COMPACT`] instead;
/// the reactive footers in `ctx_read`/`archive`/`ctx_shell` still teach the
/// `ctx_expand` path in context.
pub const RECOVER: &str = "RECOVER: compressed output is reversible — never re-read line-by-line. \
    Need full/exact? Read the shown file path with any tool (no MCP), or \
    ctx_read(mode=full|raw=true); [Archived]/tee/firewall → ctx_expand(id=...).";

/// Terse injected variant of [`RECOVER`] (FULL + COMPACT/Bare). The cold
/// first-contact handshake renders the COMPACT profile, so this one-liner keeps
/// the static char/token budget (`tests/intensive_benchmarks.rs`,
/// `instructions.rs`); since v5 the FULL dedicated files carry it too (#578).
/// Keeps the two primary MCP-optional paths and the "never line-by-line" rule.
/// Must keep the `(no MCP)` clause (asserted in tests).
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
Exclusive tools (no native trigger): ctx_compose (understand code, call first), ctx_search(action=symbol) (exact symbol), ctx_callgraph (callers), ctx_search(action=semantic) (by meaning), ctx_knowledge / ctx_session (memory).";

/// Hook-covered header (GL #1153): the honest replacement for the
/// `CRITICAL`/`BULLETS`/`NEVER` mapping on hosts whose *installed hooks*
/// already compress the native tools (Cursor: `preToolUse` rewrite covers
/// Shell, redirect covers Read/Grep). There a "NEVER use native tools" rule
/// fights the host's own tool guidance and is unenforceable — the model calls
/// native tools anyway and the hooks compress them transparently. Saying so
/// removes the instruction dissonance instead of losing the battle silently.
pub const HOOK_COVERED_HEADER: &str = "\
CRITICAL: ALWAYS prefer lean-ctx ctx_* tools over native equivalents — ctx_* tools \
provide superior caching, compression, and session memory. Hooks compress native \
Shell/Read/Grep as fallback, but direct ctx_* calls are the primary path.\n\
ACTUALLY EMIT the ctx_* tool call (ctx_compose first) — describing a tool is not calling it.\n\
WHY ctx_read > native Read: ctx_read picks the optimal mode (map/signatures/terse) \
per file, caches for instant re-reads (~13 tokens), and compresses 26-92%. Native \
Read through hook redirect is limited to verbatim pass-through (~5% savings).";

/// The tools worth an explicit MCP call on a hook-covered host: capabilities
/// with *no* native equivalent the hooks could intercept. Kept in sync with
/// [`SHADOW_MINIMAL`]'s exclusive-tools line (same rationale, different cause).
pub const HOOK_COVERED_TOOLS: &str = "\
MANDATORY MAPPING (always use ctx_* instead of native):\n\
• Read/cat -> ctx_read(path, mode) — cached, 10 modes, re-reads ~13 tokens\n\
• Grep/search -> ctx_search(pattern, path) — also action=symbol|semantic for definitions/meaning\n\
• Shell/bash -> ctx_shell(command) — 95+ compression patterns\n\
• ctx_compose — orient in code FIRST (bundles search + read + symbols) — call before editing/debugging\n\
• ctx_callgraph — callers, callees, blast radius — use instead of manual file reading\n\
• ctx_session / ctx_knowledge — persistent memory — record decisions & progress after milestones\n\
• ctx_expand — recover full text from [Archived]/compressed output";

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

/// Raw compression — densest possible output. Bullet points only, zero prose,
/// diff-style facts, no intro/outro. Tighter than Max (#795).
pub const RAW_PROMPT: &str = "\
OUTPUT STYLE: raw-dense
- Bullet points ONLY, zero prose, no intro, no outro, no greetings
- Diff-style facts: +added, -removed, ~changed, !breaking
- One fact per line, max 60 chars
- Symbolic: → ∵ ∴ ⊕ ⊖ Δ ≈ ≠ ∈ ∅ ✓ ✗ (same as expert-terse)
- Code blocks: untouched
- BUDGET: ≤50 tokens per non-code response
- NEVER explain what you did — show only the result";

/// Return the compression prompt text for a given level (empty string for Off).
pub fn compression_text(level: CompressionLevel) -> &'static str {
    match level {
        CompressionLevel::Off => "",
        CompressionLevel::Lite => LITE_PROMPT,
        CompressionLevel::Standard => STANDARD_PROMPT,
        CompressionLevel::Max => MAX_PROMPT,
        CompressionLevel::Raw => RAW_PROMPT,
    }
}

// The verbose teaching profile — only the on-demand project `LEAN-CTX.md`
// Static profile arrays removed (#756): replaced by the profile-aware
// section-builder functions below (longform_non_shadow_sections, etc.).

// ── Profile-aware section assembly (#756) ──────────────────────
//
// Each function returns Vec<String> with the same section ordering as the
// static arrays above, but replaces tool-referencing constants with their
// dynamic equivalents from `rules_sections`.

fn s(c: &str) -> String {
    c.to_string()
}

fn longform_non_shadow_sections(p: &super::tool_profiles::ToolProfile) -> Vec<String> {
    use super::rules_sections as rs;
    let mut v = vec![
        s(CRITICAL),
        s(MUST_INVOKE),
        s(BULLETS),
        s(NEVER),
        rs::intent_section(p),
        s(AGENT_LOOP), // verbose teaching — LONGFORM only
        rs::anti_section(p),
        s(NAV_PARADOX), // verbose teaching — LONGFORM only
        s(PARALLEL),
        s(AUTO),
        s(RECOVER),
        s(CEP),
        s(INTELLIGENCE),
        rs::litm_end_section(p),
    ];
    if let Some(fb) = rs::ctx_call_fallback(p) {
        v.push(fb);
    }
    v
}

fn full_non_shadow_sections(p: &super::tool_profiles::ToolProfile) -> Vec<String> {
    use super::rules_sections as rs;
    let mut v = vec![
        s(CRITICAL),
        s(MUST_INVOKE),
        s(BULLETS),
        s(NEVER),
        rs::intent_section(p),
        rs::anti_section(p),
        s(PARALLEL),
        s(AUTO),
        s(RECOVER_COMPACT),
        s(INTELLIGENCE),
        rs::litm_end_section(p),
    ];
    if let Some(fb) = rs::ctx_call_fallback(p) {
        v.push(fb);
    }
    v
}

fn full_shadow_sections(p: &super::tool_profiles::ToolProfile) -> Vec<String> {
    use super::rules_sections as rs;
    vec![rs::shadow_minimal_section(p), s(INTELLIGENCE)]
}

fn hook_covered_non_shadow_sections(p: &super::tool_profiles::ToolProfile) -> Vec<String> {
    use super::rules_sections as rs;
    let mut v = vec![
        s(HOOK_COVERED_HEADER),
        rs::hook_covered_tools_section(p),
        s(PARALLEL),
        s(RECOVER_COMPACT),
        s(INTELLIGENCE),
    ];
    if let Some(fb) = rs::ctx_call_fallback(p) {
        v.push(fb);
    }
    v
}

fn compact_non_shadow_sections(p: &super::tool_profiles::ToolProfile) -> Vec<String> {
    use super::rules_sections as rs;
    let mut v = vec![
        s(CRITICAL),
        s(BULLETS),
        s(NEVER),
        rs::intent_section(p),
        rs::anti_section(p),
        s(PARALLEL),
        s(RECOVER_COMPACT),
    ];
    if let Some(fb) = rs::ctx_call_fallback(p) {
        v.push(fb);
    }
    v
}

fn compact_shadow_sections(p: &super::tool_profiles::ToolProfile) -> Vec<String> {
    use super::rules_sections as rs;
    vec![rs::shadow_minimal_section(p)]
}

/// Selects the profile (LONGFORM / FULL / COMPACT) and the wrapping style
/// (markers, headers, footers) for `render()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrapper {
    /// **On-demand long form** (project `LEAN-CTX.md`). LONGFORM profile with
    /// the same marker wrapping as `Dedicated`. Not auto-loaded by any client
    /// — agents open it on demand via the AGENTS.md pointer, so it can afford
    /// the verbose teaching sections.
    Longform,

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

    /// **Hook-covered dedicated rule file** (GL #1153). For hosts whose
    /// installed lean-ctx hooks already compress the native tools (Cursor:
    /// PreToolUse rewrite/redirect). Same marker/version wrapping as
    /// `Dedicated`, but the body swaps the unenforceable tool-mapping for the
    /// honest hook-coverage note plus the exclusive-capability advert.
    /// Shadow mode collapses it to the same minimal profile as `Dedicated`.
    HookCovered,
}

/// Render lean-ctx rules for a given wrapper, shadow mode, compression level,
/// and tool profile (#756).
///
/// * `shadow` — when true, tool-mapping sections (BULLETS, NEVER,
///   CRITICAL banner, "## Tool Mapping" header) are omitted.
/// * `wrapper` — selects the profile (FULL / COMPACT) and wrapping style.
/// * `level` — selects the output-style compression prompt (Lite / Standard /
///   Max) which is appended to the body. `Off` omits it.
/// * `tool_profile` — filters tool references in dynamic sections so agents
///   only see tools they can actually call.
pub fn render(
    shadow: bool,
    wrapper: Wrapper,
    level: CompressionLevel,
    tool_profile: &super::tool_profiles::ToolProfile,
) -> String {
    use super::rules_sections as rs;

    let sections: Vec<String> = match (wrapper, shadow) {
        (Wrapper::Longform, false) => longform_non_shadow_sections(tool_profile),
        (Wrapper::Longform | Wrapper::Dedicated | Wrapper::HookCovered, true) => {
            full_shadow_sections(tool_profile)
        }
        (Wrapper::Dedicated, false) => full_non_shadow_sections(tool_profile),
        (Wrapper::HookCovered, false) => hook_covered_non_shadow_sections(tool_profile),
        (_, false) => compact_non_shadow_sections(tool_profile),
        (_, true) => compact_shadow_sections(tool_profile),
    };

    // Suppress empty sections (a section builder may return "" when the
    // profile hides all tools it would mention).
    let mut body: String = sections
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let _ = rs::intent_section; // anchor — ensures the module is linked

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

/// Unmarked render of the hook-covered profile for ephemeral channels
/// (the mcp.json `instructions` snapshot on hook-covered hosts, GL #1153).
/// The `Bare` counterpart of `Wrapper::HookCovered`: same body, no markers —
/// per-session channels are governed by carrier coverage, so markers would be
/// noise (see [`COMPRESSION_BLOCK_START`]). Shadow collapses to the regular
/// bare shadow profile (interception supersedes hook coverage).
pub fn render_hook_covered_bare(
    shadow: bool,
    level: CompressionLevel,
    tool_profile: &super::tool_profiles::ToolProfile,
) -> String {
    if shadow {
        return render(true, Wrapper::Bare, level, tool_profile);
    }
    let sections = hook_covered_non_shadow_sections(tool_profile);
    let mut body: String = sections
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let compression = compression_text(level);
    if !compression.is_empty() {
        body.push('\n');
        body.push_str(compression);
    }
    body
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
        tool_profile: &super::tool_profiles::ToolProfile,
    ) -> bool {
        match self.block() {
            Some(block) => block.trim() == render(shadow, wrapper, level, tool_profile).trim(),
            None => false,
        }
    }

    /// Merge freshly-rendered rules into this file.
    ///
    /// * If a lean-ctx section exists → replaces content between `START_MARK`
    ///   and `END_MARK`, preserving user content before/after.
    /// * If no section exists → appends fresh content at the end.
    pub fn merged(
        &self,
        shadow: bool,
        wrapper: Wrapper,
        level: CompressionLevel,
        tool_profile: &super::tool_profiles::ToolProfile,
    ) -> String {
        let fresh = render(shadow, wrapper, level, tool_profile);
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
    pub fn initial(
        shadow: bool,
        wrapper: Wrapper,
        level: CompressionLevel,
        tool_profile: &super::tool_profiles::ToolProfile,
    ) -> String {
        render(shadow, wrapper, level, tool_profile)
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

    fn tp() -> super::super::tool_profiles::ToolProfile {
        super::super::tool_profiles::ToolProfile::Power
    }

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
            NAV_PARADOX.contains("ctx_search(action=semantic)"),
            "semantic route must use folded action (#509)"
        );
        assert!(NAV_PARADOX.contains("ctx_callgraph"), "graph route");
        assert!(
            NAV_PARADOX.contains("≠"),
            "must carry the reading≠understanding thesis"
        );
    }

    #[test]
    fn longform_carries_loop_and_paradox_injected_full_does_not() {
        // v5 (#578): the verbose teaching sections live only in the on-demand
        // LEAN-CTX.md (Longform); the injected dedicated files fold the loop
        // phases + paradox thesis into INTENT.
        let long = render(false, Wrapper::Longform, CompressionLevel::Off, &tp());
        assert!(
            long.contains("AGENT LOOP"),
            "LONGFORM must carry AGENT_LOOP"
        );
        assert!(
            long.contains("NAVIGATION PARADOX"),
            "LONGFORM must carry NAV_PARADOX"
        );
        assert!(long.contains(CEP), "LONGFORM must carry CEP");

        let full = render(false, Wrapper::Dedicated, CompressionLevel::Off, &tp());
        assert!(
            !full.contains("AGENT LOOP (phase -> tool):"),
            "injected FULL must not inline the multi-line AGENT_LOOP block"
        );
        assert!(
            !full.contains("NAVIGATION PARADOX: reading"),
            "injected FULL must not inline the multi-line NAV_PARADOX block"
        );
        assert!(
            full.contains('≠'),
            "INTENT must keep the reading≠understanding thesis in FULL"
        );
        // #509: ctx_symbol folded into ctx_search(action=symbol)
        for phase_tool in [
            "ctx_compose",
            "ctx_search(action=symbol)",
            "ctx_search",
            "ctx_callgraph",
        ] {
            assert!(
                full.contains(phase_tool),
                "FULL keeps the loop tools via INTENT: {phase_tool}"
            );
        }
    }

    #[test]
    fn injected_profiles_stay_lean() {
        // The whole point of v5 (#578): injected files bill every session.
        // chars/4 ≈ tokens — the dedicated body must stay around ~470 tok and
        // the Longform must be a strict superset.
        let full = render(false, Wrapper::Dedicated, CompressionLevel::Off, &tp());
        eprintln!(
            "rules footprint: dedicated={} chars (~{} tok), longform={} chars, bare={} chars",
            full.len(),
            full.len() / 4,
            render(false, Wrapper::Longform, CompressionLevel::Off, &tp()).len(),
            render(false, Wrapper::Bare, CompressionLevel::Off, &tp()).len(),
        );
        assert!(
            full.len() <= 2400,
            "injected dedicated rules must stay ≤2400 chars (~600 tok), got {} chars (~{} tok)",
            full.len(),
            full.len() / 4
        );
        let long = render(false, Wrapper::Longform, CompressionLevel::Off, &tp());
        assert!(
            long.len() > full.len(),
            "Longform ({}) must carry more than the injected profile ({})",
            long.len(),
            full.len()
        );
        let compact = render(false, Wrapper::Bare, CompressionLevel::Off, &tp());
        assert!(
            compact.len() < full.len(),
            "COMPACT/Bare ({}) must stay below the dedicated profile ({})",
            compact.len(),
            full.len()
        );
    }

    #[test]
    fn compact_profile_has_no_multiline_teaching_sections() {
        // COMPACT (shared + Bare) keeps the per-session channel lean: no
        // multi-line AGENT_LOOP/NAV_PARADOX blocks; INTENT carries the thesis.
        let out = render(false, Wrapper::Shared, CompressionLevel::Off, &tp());
        assert!(
            !out.contains("AGENT LOOP (phase -> tool):"),
            "COMPACT must not inline the multi-line AGENT_LOOP block"
        );
        assert!(
            !out.contains("NAVIGATION PARADOX: reading"),
            "COMPACT must not inline the multi-line NAV_PARADOX block"
        );
        assert!(
            out.contains('≠'),
            "COMPACT keeps the reading≠understanding thesis via INTENT"
        );
    }

    #[test]
    fn shadow_omits_loop_and_paradox() {
        // #963: shadow collapses to the irreducible minimum — the routing
        // taxonomy is redundant once native calls are intercepted.
        for wrapper in [Wrapper::Longform, Wrapper::Dedicated, Wrapper::Shared] {
            let out = render(true, wrapper, CompressionLevel::Off, &tp());
            assert!(!out.contains("AGENT LOOP"), "{wrapper:?} shadow drops loop");
            assert!(
                !out.contains("NAVIGATION PARADOX"),
                "{wrapper:?} shadow drops paradox"
            );
        }
    }

    #[test]
    fn recover_reaches_every_non_shadow_carrier() {
        // The recovery vocabulary must reach every non-shadow carrier so agents
        // never re-read compressed output line-by-line, and every carrier must
        // keep the MCP-free path ("read the shown path") for orgs that ban MCP.
        // v5 (#578): only Longform carries the verbose RECOVER; every injected
        // profile (Dedicated FULL + Shared/Bare COMPACT) carries the terse
        // RECOVER_COMPACT one-liner.
        let long = render(false, Wrapper::Longform, CompressionLevel::Off, &tp());
        assert!(
            long.contains(RECOVER),
            "Longform must carry the verbose RECOVER verbatim"
        );
        for wrapper in [Wrapper::Dedicated, Wrapper::Shared, Wrapper::Bare] {
            let out = render(false, wrapper, CompressionLevel::Off, &tp());
            assert!(
                out.contains(RECOVER_COMPACT),
                "{wrapper:?} must carry RECOVER_COMPACT verbatim"
            );
            assert!(
                !out.contains(RECOVER),
                "{wrapper:?} must not inline the verbose RECOVER block"
            );
        }
        for wrapper in [
            Wrapper::Longform,
            Wrapper::Dedicated,
            Wrapper::Shared,
            Wrapper::Bare,
        ] {
            assert!(
                render(false, wrapper, CompressionLevel::Off, &tp()).contains("(no MCP)"),
                "{wrapper:?} recovery line must keep the MCP-free path"
            );
        }
        for wrapper in [Wrapper::Dedicated, Wrapper::Shared] {
            let out = render(true, wrapper, CompressionLevel::Off, &tp());
            assert!(
                !out.contains(RECOVER) && !out.contains(RECOVER_COMPACT),
                "{wrapper:?} shadow drops all RECOVER guidance"
            );
        }
    }

    // --- render() — Dedicated ---

    #[test]
    fn dedicated_has_markers_and_version() {
        let out = render(false, Wrapper::Dedicated, CompressionLevel::Off, &tp());
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
        let out = render(true, Wrapper::Dedicated, CompressionLevel::Off, &tp());
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
        let shadow = render(true, Wrapper::Dedicated, CompressionLevel::Off, &tp());
        let full = render(false, Wrapper::Dedicated, CompressionLevel::Off, &tp());
        assert!(
            shadow.len() < full.len(),
            "shadow ({}) must be smaller than non-shadow ({})",
            shadow.len(),
            full.len()
        );
    }

    #[test]
    fn dedicated_litm_structure() {
        let out = render(false, Wrapper::Dedicated, CompressionLevel::Off, &tp());
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
        let dedicated = render(false, Wrapper::Dedicated, CompressionLevel::Off, &tp());
        assert!(
            dedicated.contains(MUST_INVOKE),
            "dedicated non-shadow rules must carry the MUST_INVOKE nudge"
        );
        assert!(
            !render(true, Wrapper::Dedicated, CompressionLevel::Off, &tp()).contains(MUST_INVOKE),
            "shadow mode must not carry the nudge (routing is enforced at the call layer)"
        );
        assert!(
            !render(false, Wrapper::Bare, CompressionLevel::Off, &tp()).contains(MUST_INVOKE),
            "Bare/instructions channel is capped separately and carries no copy"
        );
    }

    // --- render() — Shared ---

    #[test]
    fn shared_has_markers_and_header() {
        let out = render(false, Wrapper::Shared, CompressionLevel::Off, &tp());
        assert!(out.contains(START_MARK));
        assert!(out.contains(END_MARK));
        assert!(out.contains("MANDATORY MAPPING"));
        assert!(out.contains(BULLETS));
    }

    #[test]
    fn shared_shadow_omits_mapping() {
        let out = render(true, Wrapper::Shared, CompressionLevel::Off, &tp());
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
        let out = render(false, Wrapper::Bare, CompressionLevel::Off, &tp());
        assert!(!out.contains(START_MARK), "Bare must not have START_MARK");
        assert!(!out.contains(END_MARK), "Bare must not have END_MARK");
        assert!(!out.contains("<!-- version:"), "Bare must not have version");
        assert!(out.contains(BULLETS));
        assert!(out.contains(NEVER));
    }

    #[test]
    fn bare_shadow_only_read_modes() {
        let out = render(true, Wrapper::Bare, CompressionLevel::Off, &tp());
        assert!(!out.contains(NEVER), "shadow Bare must not have NEVER");
        assert!(
            !out.contains("MANDATORY MAPPING"),
            "shadow Bare must not have BULLETS"
        );
    }

    // --- Compression level tests ---

    #[test]
    fn render_includes_lite_prompt() {
        let out = render(false, Wrapper::Bare, CompressionLevel::Lite, &tp());
        assert!(out.contains("OUTPUT STYLE: concise"));
        assert!(out.contains("Bullet points"));
    }

    #[test]
    fn render_includes_standard_prompt() {
        let out = render(false, Wrapper::Bare, CompressionLevel::Standard, &tp());
        assert!(out.contains("OUTPUT STYLE: dense"));
        assert!(out.contains("atomic fact"));
    }

    #[test]
    fn render_includes_max_prompt() {
        let out = render(false, Wrapper::Bare, CompressionLevel::Max, &tp());
        assert!(out.contains("OUTPUT STYLE: expert-terse"));
        assert!(out.contains("Telegraph"));
    }

    #[test]
    fn render_off_excludes_compression() {
        let out = render(false, Wrapper::Bare, CompressionLevel::Off, &tp());
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
        for wrapper in [Wrapper::Longform, Wrapper::Dedicated, Wrapper::Shared] {
            let out = render(false, wrapper, CompressionLevel::Standard, &tp());
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
        let out = render(false, Wrapper::Bare, CompressionLevel::Standard, &tp());
        assert!(out.contains("OUTPUT STYLE: dense"));
        assert!(!out.contains(COMPRESSION_BLOCK_START));
        assert!(!out.contains(COMPRESSION_BLOCK_END));
    }

    #[test]
    fn compression_off_emits_no_markers_in_any_wrapper() {
        for wrapper in [
            Wrapper::Longform,
            Wrapper::Dedicated,
            Wrapper::Shared,
            Wrapper::Bare,
        ] {
            let out = render(false, wrapper, CompressionLevel::Off, &tp());
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
        let dedicated = render(false, Wrapper::Dedicated, CompressionLevel::Lite, &tp());
        assert!(crate::core::rules_channel::carries_full_rules(&dedicated));
        assert!(dedicated.contains(COMPRESSION_BLOCK_START));
    }

    // --- Wrapper round-trip ---

    #[test]
    fn all_wrappers_produce_output() {
        for shadow in [false, true] {
            for wrapper in [
                Wrapper::Longform,
                Wrapper::Dedicated,
                Wrapper::Shared,
                Wrapper::Bare,
            ] {
                let out = render(shadow, wrapper, CompressionLevel::Off, &tp());
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
        let fresh = render(false, Wrapper::Dedicated, CompressionLevel::Off, &tp());
        let content = format!("user before\n{fresh}\nuser after");
        let f = RulesFile::parse(&content);
        assert!(f.is_current(), "fresh render carries the current version");
        assert!(
            f.block_matches_render(false, Wrapper::Dedicated, CompressionLevel::Off, &tp()),
            "an unchanged block must compare equal to a fresh render"
        );
    }

    #[test]
    fn block_matches_render_false_on_compression_change() {
        // Body rendered at Off, then asked whether it matches a Max render:
        // the version is identical but the compression payload differs (#548).
        let content = render(false, Wrapper::Dedicated, CompressionLevel::Off, &tp());
        let f = RulesFile::parse(&content);
        assert!(f.is_current());
        assert!(
            !f.block_matches_render(false, Wrapper::Dedicated, CompressionLevel::Max, &tp()),
            "a compression-level change must be detected as drift"
        );
    }

    #[test]
    fn block_matches_render_false_on_shadow_change() {
        let content = render(false, Wrapper::Dedicated, CompressionLevel::Lite, &tp());
        let f = RulesFile::parse(&content);
        assert!(
            !f.block_matches_render(true, Wrapper::Dedicated, CompressionLevel::Lite, &tp()),
            "a shadow-mode toggle must be detected as drift"
        );
    }

    #[test]
    fn block_matches_render_false_without_block() {
        let f = RulesFile::parse("plain user content, no markers");
        assert!(!f.block_matches_render(false, Wrapper::Dedicated, CompressionLevel::Off, &tp()));
    }

    #[test]
    fn rules_file_merged_replaces_section() {
        let content =
            format!("before\n{START_MARK}\n<!-- version: 1 -->\n\nold\n{END_MARK}\nafter");
        let f = RulesFile::parse(&content);
        let merged = f.merged(false, Wrapper::Shared, CompressionLevel::Off, &tp());
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
        let merged = f.merged(false, Wrapper::Bare, CompressionLevel::Off, &tp());
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

    // --- HookCovered profile (GL #1153) ---

    #[test]
    fn hook_covered_carries_strict_mapping() {
        // v8: HookCovered is strict — always prefer ctx_* over native tools.
        // The MANDATORY MAPPING replaces the old "no native equivalent" list.
        let out = render(false, Wrapper::HookCovered, CompressionLevel::Off, &tp());
        assert!(
            !out.contains(NEVER),
            "HookCovered uses its own strict header, not the NEVER constant"
        );
        assert!(
            out.contains(HOOK_COVERED_HEADER),
            "must carry the strict preference header"
        );
        assert!(
            out.contains("MANDATORY MAPPING")
                && out.contains("ctx_read")
                && out.contains("ctx_search"),
            "must carry the full mandatory mapping"
        );
        assert!(
            out.contains("ctx_compose") && out.contains("action=symbol|semantic"),
            "must advertise the search capabilities"
        );
    }

    #[test]
    fn hook_covered_keeps_markers_version_and_recovery() {
        // Coverage detection (rules_channel::carries_full_rules /
        // client_autoloads_rules) and the injector's drift check both key on
        // the canonical markers — HookCovered must stay a first-class carrier.
        let out = render(false, Wrapper::HookCovered, CompressionLevel::Off, &tp());
        assert!(out.contains(START_MARK) && out.contains(END_MARK));
        assert!(out.contains(&format!("<!-- version: {RULES_VERSION} -->")));
        assert!(out.contains(RECOVER_COMPACT), "recovery line must survive");
        assert!(out.contains("(no MCP)"), "MCP-free recovery path stays");
    }

    #[test]
    fn hook_covered_is_leaner_than_full() {
        let covered = render(false, Wrapper::HookCovered, CompressionLevel::Off, &tp());
        let full = render(false, Wrapper::Dedicated, CompressionLevel::Off, &tp());
        assert!(
            covered.len() < full.len(),
            "HookCovered ({}) must be a strict reduction of FULL ({})",
            covered.len(),
            full.len()
        );
    }

    #[test]
    fn hook_covered_shadow_collapses_to_minimal() {
        // Interception supersedes hook coverage — same minimal profile as
        // Dedicated shadow.
        let covered_shadow = render(true, Wrapper::HookCovered, CompressionLevel::Off, &tp());
        let dedicated_shadow = render(true, Wrapper::Dedicated, CompressionLevel::Off, &tp());
        assert_eq!(covered_shadow, dedicated_shadow);
    }

    #[test]
    fn hook_covered_wraps_compression_in_markers() {
        let out = render(
            false,
            Wrapper::HookCovered,
            CompressionLevel::Standard,
            &tp(),
        );
        assert!(out.contains(COMPRESSION_BLOCK_START) && out.contains(COMPRESSION_BLOCK_END));
        assert!(out.contains("OUTPUT STYLE: dense"));
    }

    // --- Profile-aware rules (#756) ---

    #[test]
    fn rules_only_mention_enabled_tools() {
        use super::super::rules_sections;
        use super::super::tool_profiles::ToolProfile;
        let exclusive_tools = ["ctx_compose", "ctx_callgraph", "ctx_patch"];
        for profile in [ToolProfile::Minimal, ToolProfile::Standard] {
            let mut sections = vec![
                rules_sections::intent_section(&profile),
                rules_sections::hook_covered_tools_section(&profile),
                rules_sections::shadow_minimal_section(&profile),
                rules_sections::anti_section(&profile),
                rules_sections::litm_end_section(&profile),
            ];
            if let Some(fb) = rules_sections::ctx_call_fallback(&profile) {
                sections.push(fb);
            }
            for tool in &exclusive_tools {
                if !profile.is_tool_enabled(tool) {
                    for section in &sections {
                        assert!(
                            !section.contains(tool),
                            "profile {:?} dynamic section must not mention disabled tool {tool}",
                            profile.as_str()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn minimal_rules_shorter_than_standard() {
        use super::super::tool_profiles::ToolProfile;
        let min = render(
            false,
            Wrapper::Dedicated,
            CompressionLevel::Off,
            &ToolProfile::Minimal,
        );
        let std = render(
            false,
            Wrapper::Dedicated,
            CompressionLevel::Off,
            &ToolProfile::Standard,
        );
        assert!(
            min.len() < std.len(),
            "minimal ({}) must be shorter than standard ({})",
            min.len(),
            std.len()
        );
    }

    #[test]
    fn power_profile_output_identical_to_default() {
        use super::super::tool_profiles::ToolProfile;
        let power = render(
            false,
            Wrapper::Dedicated,
            CompressionLevel::Off,
            &ToolProfile::Power,
        );
        assert!(
            power.contains("ctx_compose"),
            "Power must include all tools"
        );
        assert!(
            power.contains("ctx_callgraph"),
            "Power must include all tools"
        );
        assert!(
            power.contains("ctx_session"),
            "Power must include all tools"
        );
    }

    #[test]
    fn render_is_deterministic_across_profiles() {
        use super::super::tool_profiles::ToolProfile;
        for profile in [
            ToolProfile::Minimal,
            ToolProfile::Standard,
            ToolProfile::Power,
        ] {
            let a = render(false, Wrapper::Dedicated, CompressionLevel::Off, &profile);
            let b = render(false, Wrapper::Dedicated, CompressionLevel::Off, &profile);
            assert_eq!(
                a,
                b,
                "render must be deterministic for {:?}",
                profile.as_str()
            );
        }
    }

    #[test]
    fn minimal_has_ctx_call_fallback() {
        use super::super::tool_profiles::ToolProfile;
        let rules = render(
            false,
            Wrapper::Dedicated,
            CompressionLevel::Off,
            &ToolProfile::Minimal,
        );
        assert!(
            rules.contains("ctx_call"),
            "minimal profile must include ctx_call gateway hint"
        );
    }

    #[test]
    fn power_has_no_ctx_call_fallback() {
        use super::super::tool_profiles::ToolProfile;
        let rules = render(
            false,
            Wrapper::Dedicated,
            CompressionLevel::Off,
            &ToolProfile::Power,
        );
        assert!(
            !rules.contains("ctx_call(tool="),
            "power profile must not include ctx_call gateway hint"
        );
    }
}
