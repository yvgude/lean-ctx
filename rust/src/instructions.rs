use crate::tools::CrpMode;

/// Claude Code truncates MCP server instructions at 2048 characters.
/// Full instructions live in the `<!-- lean-ctx -->` block in `~/.claude/CLAUDE.md`
/// plus the on-demand skill (`~/.claude/skills/lean-ctx/SKILL.md`); the legacy
/// always-loaded `~/.claude/rules/lean-ctx.md` was retired in 3.8 (GL #555).
/// Session state is dynamically appended to the MCP instructions for continuity.
///
/// Universal instruction cap for all MCP clients (in tokens, not bytes).
/// Enforced via `count_tokens` so truncation is accurate regardless of
/// character mix (ASCII, CJK, emoji).
///
/// Budget split (#579): the static skeleton must stay <= 400 tokens
/// (asserted in tests — details belong in LEAN-CTX.md on disk, not in every
/// session); the remainder is headroom for dynamic session/knowledge blocks.
const INSTRUCTION_CAP_TOKENS: usize = 800;

/// Token budget for the static instruction skeleton (no session/knowledge
/// state). CI-asserted so instruction creep cannot silently tax every session.
/// Tdd mode pays extra for the CRP suffix + INSTRUCTION CODES decoder.
#[cfg(test)]
const STATIC_INSTRUCTION_BUDGET_TOKENS: usize = 400;
#[cfg(test)]
const STATIC_INSTRUCTION_BUDGET_TDD_TOKENS: usize = 500;
/// Windows additionally carries the one-line `SHELL:` hint (POSIX vs
/// PowerShell disambiguation, see `build_shell_hint`) inside the skeleton.
/// Budgeted explicitly so the cap stays honest on every platform.
#[cfg(all(test, windows))]
const STATIC_INSTRUCTION_SHELL_HINT_TOKENS: usize = 25;
#[cfg(all(test, not(windows)))]
const STATIC_INSTRUCTION_SHELL_HINT_TOKENS: usize = 0;

pub fn build_instructions(crp_mode: CrpMode) -> String {
    build_instructions_with_client(crp_mode, "")
}

pub fn build_instructions_with_client(crp_mode: CrpMode, client_name: &str) -> String {
    if is_claude_code_client(client_name) || is_codebuddy_client(client_name) {
        return build_claude_code_instructions();
    }
    build_full_instructions(crp_mode, client_name)
}

pub fn build_instructions_for_test(crp_mode: CrpMode) -> String {
    // Avoid loading dynamic on-disk session/knowledge/gotcha blocks in tests, which can
    // vary across machines and between concurrent test runs.
    build_full_instructions_for_test(crp_mode, "")
}

pub fn build_instructions_with_client_for_test(crp_mode: CrpMode, client_name: &str) -> String {
    if is_claude_code_client(client_name) || is_codebuddy_client(client_name) {
        return build_claude_code_instructions();
    }
    build_full_instructions_for_test(crp_mode, client_name)
}

/// Deterministic instruction builder for the Instruction Compiler.
///
/// MUST NOT depend on process-global env toggles or on-disk mutable config, because the compiler
/// output is intended to be stable and diffable across runs and in CI.
pub fn build_instructions_with_client_for_compiler(
    crp_mode: CrpMode,
    client_name: &str,
    unified_tool_mode: bool,
) -> String {
    if is_claude_code_client(client_name) || is_codebuddy_client(client_name) {
        return build_claude_code_instructions();
    }
    build_full_instructions_for_compiler(crp_mode, client_name, unified_tool_mode)
}

fn is_claude_code_client(client_name: &str) -> bool {
    let lower = client_name.to_lowercase();
    lower.contains("claude") && !lower.contains("cursor")
}

fn is_codebuddy_client(client_name: &str) -> bool {
    let lower = client_name.to_lowercase();
    lower.contains("codebuddy")
}

/// LITM calibration manifest rotation (#539).
///
/// Settles the previous manifest — every entry the agent never re-recalled is
/// a placement *hit* (misses were already recorded by the recall hook) — then
/// stores the manifest for the injection built from `session` right now:
/// task + decisions go to the begin block, findings + next steps to the end.
fn rotate_wakeup_manifest(session: &crate::core::session::SessionState, profile_name: &str) {
    use crate::core::litm_calibration::{Position, record_outcome};
    use crate::core::session::ManifestEntry;

    let mut updated = session.clone();

    for entry in &updated.wakeup_manifest {
        if !entry.missed
            && let Some(pos) = Position::parse(&entry.position)
        {
            record_outcome(&entry.profile, pos, true);
        }
    }

    let mut manifest: Vec<ManifestEntry> = Vec::new();
    let mut push = |key: &str, position: &str| {
        let key = key.trim();
        if !key.is_empty() {
            manifest.push(ManifestEntry {
                key: key.chars().take(80).collect(),
                position: position.to_string(),
                profile: profile_name.to_string(),
                missed: false,
            });
        }
    };

    if let Some(ref task) = updated.task {
        push(&task.description, "begin");
    }
    for d in updated.decisions.iter().rev().take(5) {
        push(&d.summary, "begin");
    }
    for f in updated.findings.iter().rev().take(8) {
        push(&f.summary, "end");
    }
    for n in updated.next_steps.iter().take(3) {
        push(n, "end");
    }

    updated.wakeup_manifest = manifest;
    let _ = updated.save();
}

pub fn claude_config_dir_display() -> String {
    match std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(dir) if !dir.trim().is_empty() => {
            let dir = dir.trim().to_string();
            if dir.starts_with('~') {
                dir
            } else if let Some(home) = dirs::home_dir() {
                let home_str = home.to_string_lossy();
                if let Some(rest) = dir.strip_prefix(home_str.as_ref()) {
                    format!("~{rest}")
                } else {
                    dir
                }
            } else {
                dir
            }
        }
        _ => "~/.claude".to_string(),
    }
}

fn build_claude_code_instructions() -> String {
    let shell_hint = build_shell_hint();
    let config_dir = claude_config_dir_display();

    // Load session state for continuity (compact version for Claude Code's char limit)
    let session_block = match crate::core::session::SessionState::load_latest() {
        Some(session) => {
            let mut parts = Vec::new();
            if let Some(ref task) = session.task {
                let pct = task
                    .progress_pct
                    .map_or(String::new(), |p| format!(" [{p}%]"));
                parts.push(format!("Task: {}{pct}", task.description));
            }
            if !session.decisions.is_empty() {
                let items: Vec<&str> = session
                    .decisions
                    .iter()
                    .rev()
                    .take(3)
                    .map(|d| d.summary.as_str())
                    .collect();
                parts.push(format!("Decisions: {}", items.join("; ")));
            }
            if !session.files_touched.is_empty() {
                let modified: Vec<&str> = session
                    .files_touched
                    .iter()
                    .filter(|f| f.modified)
                    .take(5)
                    .map(|f| f.path.as_str())
                    .collect();
                if !modified.is_empty() {
                    parts.push(format!("Modified: {}", modified.join(", ")));
                }
            }
            if !session.findings.is_empty() {
                let recent: Vec<&str> = session
                    .findings
                    .iter()
                    .rev()
                    .take(3)
                    .map(|f| f.summary.as_str())
                    .collect();
                parts.push(format!("Recent: {}", recent.join("; ")));
            }
            if parts.is_empty() {
                String::new()
            } else {
                format!("\n\n--- SESSION ---\n{}\n---", parts.join("\n"))
            }
        }
        None => String::new(),
    };

    let cfg = crate::core::config::Config::load();
    let shadow_preamble = if cfg.shadow_mode {
        "SHADOW MODE ACTIVE: ALL reads/searches/shell MUST use ctx_* tools. Native equivalents are intercepted.\n\n"
    } else {
        ""
    };

    let instr = format!("\
{shadow_preamble}\
ALWAYS use lean-ctx MCP tools instead of native equivalents.

Tool mapping (MANDATORY):
• Read/cat/head/tail -> ctx_read(path, mode)
• Shell/bash -> ctx_shell(command)
• Grep/rg -> ctx_search(pattern, path)
• ls/find -> ctx_tree(path, depth)
• Edit/StrReplace -> native (lean-ctx=READ only). If Edit needs Read and Read is unavailable, use ctx_edit.
• Write, Delete, Glob -> normal. NEVER loop on Edit failures — use ctx_edit.

ctx_read modes: auto(default)|full|map|signatures|diff|task|reference|aggressive|entropy|lines:N-M
Omit mode → auto-selects optimal. full only right before editing. Re-reads ~13 tok. File refs F1,F2.. persist.

Workflow: Orient(ctx_overview) → Locate(ctx_search) → Read(ctx_read) → Edit(ctx_edit/native) → Verify(ctx_read diff + ctx_shell test) → Record(ctx_knowledge).

CEP: 1.ACT FIRST 2.DELTA ONLY 3.STRUCTURED(+/-/~) 4.ONE LINE 5.QUALITY
{shell_hint}\
Prefer: ctx_read>Read | ctx_shell>Shell | ctx_search>Grep | ctx_tree>ls
Edit: native Edit/StrReplace preferred, ctx_edit if Edit unavailable.
Never echo tool output. Never narrate. Show only changed code.
Full instructions at {config_dir}/CLAUDE.md (imports rules/lean-ctx.md){session_block}");

    instr
}

fn build_full_instructions(crp_mode: CrpMode, client_name: &str) -> String {
    let cfg = crate::core::config::Config::load();
    let minimal = cfg.minimal_overhead_effective_for_client(client_name);

    let profile = crate::core::litm::LitmProfile::from_client_name(client_name);
    let loaded_session = if minimal {
        None
    } else {
        crate::core::session::SessionState::load_latest()
    };

    let (session_block, litm_end_block) = match loaded_session {
        Some(ref session) => {
            // LITM calibration (#539): rotate the placement manifest — every
            // entry the agent never re-recalled counts as a placement hit —
            // then rebuild it for this injection and apply the learned share.
            rotate_wakeup_manifest(session, profile.name);
            let share = crate::core::litm_calibration::begin_share(profile.name);
            let positioned = crate::core::litm::position_optimize_with_share(session, share);
            let begin = format!(
                "\n\n--- ACTIVE SESSION (LITM P1: begin position, profile: {}) ---\n{}\n---\n",
                profile.name, positioned.begin_block
            );
            let end = if positioned.end_block.is_empty() {
                String::new()
            } else {
                format!(
                    "\n--- SESSION RESUME (post-compaction) ---\n{}\n---\n",
                    positioned.end_block
                )
            };
            (begin, end)
        }
        None => (String::new(), String::new()),
    };

    let project_root_for_blocks = if minimal {
        None
    } else {
        loaded_session
            .as_ref()
            .and_then(|s| s.project_root.clone())
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
            })
    };

    let knowledge_block = match &project_root_for_blocks {
        Some(root) => {
            let knowledge = crate::core::knowledge::ProjectKnowledge::load(root);
            match knowledge {
                Some(k) if !k.facts.is_empty() || !k.patterns.is_empty() => {
                    let aaak = k.format_aaak();
                    if aaak.is_empty() {
                        String::new()
                    } else {
                        format!("\n--- PROJECT MEMORY (AAAK) ---\n{}\n---\n", aaak.trim())
                    }
                }
                _ => String::new(),
            }
        }
        None => String::new(),
    };

    let gotcha_block = match &project_root_for_blocks {
        Some(root) => {
            let store = crate::core::gotcha_tracker::GotchaStore::load(root);
            let files: Vec<String> = loaded_session
                .as_ref()
                .map(|s| s.files_touched.iter().map(|ft| ft.path.clone()).collect())
                .unwrap_or_default();
            let block = store.format_injection_block(&files);
            if block.is_empty() {
                String::new()
            } else {
                format!("\n{block}\n")
            }
        }
        None => String::new(),
    };

    let shell_hint = build_shell_hint();

    use crate::core::rules_canonical as rc;
    let tool_bullets = rc::tool_mapping_bullets(rc::Mode::Mcp);
    let read_modes = rc::ctx_read_modes_block();
    let auto_block = rc::automation_block();
    let cep = rc::cep_block();
    let litm_pref = rc::litm_end_block(rc::Mode::Mcp);

    let shadow_preamble = if cfg.shadow_mode {
        "SHADOW MODE ACTIVE: ALL file reads, searches, and shell commands MUST go through ctx_* tools. \
         Native Read/Grep/Shell are intercepted and redirected — using ctx_* directly is faster and more reliable.\n\n"
    } else {
        ""
    };

    let mut base = format!(
        "\
{shadow_preamble}\
CRITICAL: ALWAYS use lean-ctx MCP tools instead of native equivalents for token savings.\n\
\n\
{tool_bullets}\n\
{shell_hint}\
\n\
{read_modes}\n\
\n\
{auto_block}\n\
\n\
{cep}\n\
\n\
{decoder_block}\n\
\n\
{session_block}\
{knowledge_block}\
{gotcha_block}\
\n\
{origin}\n\
\n\
{litm_pref}\
{litm_end_block}",
        decoder_block =
            crate::core::protocol::instruction_decoder_block(matches!(crp_mode, CrpMode::Tdd)),
        origin = crate::core::integrity::origin_line(),
        litm_end_block = &litm_end_block
    );

    if should_use_unified(client_name) {
        base.push_str("\n\n");
        base.push_str(rc::unified_tool_mode_block());
        base.push('\n');
    }

    let intelligence_block = build_intelligence_block();
    let terse_block = build_terse_agent_block_for_client(&crp_mode, client_name);

    // The guidance suffix (CRP-mode rules + compression/output-style + the
    // intelligence block) is the operational contract for the agent and must
    // survive the token cap. The variable session/knowledge/gotcha blocks live
    // inside `base` and are the right thing to shed under pressure (H3). So we
    // protect the suffix and truncate only `base` to fit the budget.
    let guidance_suffix = match crp_mode_suffix(&crp_mode) {
        "" => format!("{terse_block}{intelligence_block}"),
        crp => format!("{crp}\n\n{terse_block}{intelligence_block}"),
    };

    assemble_within_cap(&base, &guidance_suffix, INSTRUCTION_CAP_TOKENS)
}

/// CRP-mode contract appended to the instructions. One compact line per mode
/// (#579): the abbreviation list and notation example double as the legend.
fn crp_mode_suffix(crp_mode: &CrpMode) -> &'static str {
    match crp_mode {
        CrpMode::Off => "",
        CrpMode::Compact => {
            "CRP MODE: compact — omit filler; abbreviate fn,cfg,impl,deps,req,res; \
             diff lines (+/-) only; <=200 tok; trust tool outputs."
        }
        CrpMode::Tdd => {
            "CRP MODE: tdd — max density; Fn refs + diff lines only \
             (+F1:42 | -F1:10-15 | ~F1:42 old->new); <=150 tok; zero narration."
        }
    }
}

/// Join `base` and a protected `suffix` so the result fits `cap_tokens`,
/// truncating only `base` if needed. The suffix is the agent's operational
/// contract (compression/output-style guidance) and is preserved verbatim as
/// long as it fits on its own; otherwise we fall back to capping the whole.
fn assemble_within_cap(base: &str, suffix: &str, cap_tokens: usize) -> String {
    use crate::core::tokens::count_tokens;
    let suffix = suffix.trim_end_matches('\n');
    if suffix.is_empty() {
        let full = base.to_string();
        return if count_tokens(&full) > cap_tokens {
            truncate_to_token_cap(&full, cap_tokens)
        } else {
            full
        };
    }

    let full = format!("{base}\n\n{suffix}");
    if count_tokens(&full) <= cap_tokens {
        return full;
    }

    let suffix_tokens = count_tokens(suffix);
    // Reserve room for the suffix plus the "\n\n" join. If the suffix alone is
    // already at/over budget, degrade to a plain tail-cap of the whole text.
    let Some(base_budget) = cap_tokens.checked_sub(suffix_tokens + 1) else {
        return truncate_to_token_cap(&full, cap_tokens);
    };
    let trimmed_base = truncate_to_token_cap(base, base_budget);
    format!("{trimmed_base}\n\n{suffix}")
}

fn truncate_to_token_cap(s: &str, cap_tokens: usize) -> String {
    use crate::core::tokens::count_tokens;
    if count_tokens(s) <= cap_tokens {
        return s.to_string();
    }
    // Keep whole lines: candidate cut points are the byte offsets of each
    // newline. Token count is monotonic in prefix length, so binary-search for
    // the longest whole-line prefix within the cap. This costs O(log lines)
    // tokenizations instead of O(lines) — the per-line loop was pathologically
    // slow on large session blocks (and timed out under coverage's ptrace
    // instrumentation).
    let cuts: Vec<usize> = s.match_indices('\n').map(|(i, _)| i).collect();
    let (mut lo, mut hi) = (0usize, cuts.len());
    let mut best: Option<usize> = None;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let end = cuts[mid];
        if end > 0 && count_tokens(&s[..end]) <= cap_tokens {
            best = Some(end);
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if let Some(end) = best {
        return s[..end].to_string();
    }
    // No line boundary fits — fall back to a char-boundary byte approximation.
    let byte_approx = cap_tokens * 4;
    let safe = s.floor_char_boundary(byte_approx.min(s.len()));
    s[..safe].to_string()
}

fn build_full_instructions_for_test(crp_mode: CrpMode, client_name: &str) -> String {
    use crate::core::rules_canonical as rc;
    let shell_hint = build_shell_hint();
    let session_block = String::new();
    let knowledge_block = String::new();
    let gotcha_block = String::new();
    let litm_end_block = String::new();

    let tool_bullets = rc::tool_mapping_bullets(rc::Mode::Mcp);
    let read_modes = rc::ctx_read_modes_block();
    let auto_block = rc::automation_block();
    let cep = rc::cep_block();
    let litm_pref = rc::litm_end_block(rc::Mode::Mcp);

    let mut base = format!(
        "\
CRITICAL: ALWAYS use lean-ctx MCP tools instead of native equivalents for token savings.\n\
\n\
{tool_bullets}\n\
{shell_hint}\
\n\
{read_modes}\n\
\n\
{auto_block}\n\
\n\
{cep}\n\
\n\
{decoder_block}\n\
\n\
{session_block}\
{knowledge_block}\
{gotcha_block}\
\n\
{origin}\n\
\n\
{litm_pref}\
{litm_end_block}",
        decoder_block =
            crate::core::protocol::instruction_decoder_block(matches!(crp_mode, CrpMode::Tdd)),
        origin = crate::core::integrity::origin_line(),
        litm_end_block = &litm_end_block
    );

    if should_use_unified(client_name) {
        base.push_str("\n\n");
        base.push_str(rc::unified_tool_mode_block());
        base.push('\n');
    }

    let intelligence_block = build_intelligence_block();
    let terse_block = build_terse_agent_block_for_client(&crp_mode, client_name);

    match crp_mode_suffix(&crp_mode) {
        "" => format!("{base}\n\n{terse_block}{intelligence_block}"),
        crp => format!("{base}\n\n{crp}\n\n{terse_block}{intelligence_block}"),
    }
}

fn build_full_instructions_for_compiler(
    crp_mode: CrpMode,
    client_name: &str,
    unified_tool_mode: bool,
) -> String {
    let shell_hint = build_shell_hint();
    let session_block = String::new();
    let knowledge_block = String::new();
    let gotcha_block = String::new();
    let litm_end_block = String::new();

    use crate::core::rules_canonical as rc;
    let tool_bullets = rc::tool_mapping_bullets(rc::Mode::Mcp);
    let read_modes = rc::ctx_read_modes_block();
    let auto_blk = rc::automation_block();
    let cep = rc::cep_block();
    let litm_pref = rc::litm_end_block(rc::Mode::Mcp);

    let mut base = format!(
        "\
CRITICAL: ALWAYS use lean-ctx MCP tools instead of native equivalents for token savings.\n\
\n\
{tool_bullets}\n\
{shell_hint}\
\n\
{read_modes}\n\
\n\
{auto_blk}\n\
\n\
{cep}\n\
\n\
{decoder_block}\n\
\n\
{session_block}\
{knowledge_block}\
{gotcha_block}\
\n\
{origin}\n\
\n\
{litm_pref}\
{litm_end_block}",
        decoder_block =
            crate::core::protocol::instruction_decoder_block(matches!(crp_mode, CrpMode::Tdd)),
        origin = crate::core::integrity::origin_line(),
        litm_end_block = &litm_end_block
    );

    if unified_tool_mode {
        base.push_str("\n\n");
        base.push_str(rc::unified_tool_mode_block());
        base.push('\n');
    }

    let _ = client_name; // keep signature aligned with other builders
    let intelligence_block = build_intelligence_block();

    match crp_mode_suffix(&crp_mode) {
        "" => format!("{base}\n\n{intelligence_block}"),
        crp => format!("{base}\n\n{crp}\n\n{intelligence_block}"),
    }
}

pub fn claude_code_instructions() -> String {
    build_claude_code_instructions()
}

fn build_terse_agent_block_for_client(_crp_mode: &CrpMode, client_name: &str) -> String {
    use crate::core::config::{CompressionLevel, Config};
    let cfg = Config::load();
    let compression = CompressionLevel::effective(&cfg);
    if compression.is_active() {
        let persona = crate::core::persona::Persona::resolve(&cfg);
        return crate::core::terse::agent_prompts::build_prompt_block_for_persona(
            &compression,
            client_name,
            &persona,
        );
    }
    String::new()
}

fn build_intelligence_block() -> String {
    "OUTPUT: never echo tool output, no narration comments, show only changed code.".to_string()
}

fn build_shell_hint() -> String {
    if !cfg!(windows) {
        return String::new();
    }
    // Keep this hint terse: it rides inside the static skeleton, which is
    // budget-capped (#579) — the cap applies on Windows too.
    let name = crate::shell::shell_name();
    let is_posix = matches!(name.as_str(), "bash" | "sh" | "zsh" | "fish");
    if is_posix {
        format!("\nSHELL: {name} (POSIX) — POSIX commands only, no PowerShell cmdlets.\n")
    } else if name.contains("powershell") || name.contains("pwsh") {
        format!("\nSHELL: {name}. Use PowerShell cmdlets.\n")
    } else {
        format!("\nSHELL: {name}.\n")
    }
}

fn should_use_unified(client_name: &str) -> bool {
    if std::env::var("LEAN_CTX_FULL_TOOLS").is_ok() {
        return false;
    }
    if std::env::var("LEAN_CTX_UNIFIED").is_ok() {
        return true;
    }
    let _ = client_name;
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tokens::count_tokens;

    #[test]
    fn guidance_suffix_survives_oversized_base() {
        // Simulate a bloated session/knowledge `base` that alone exceeds the cap.
        let base = "SESSION LINE\n".repeat(4000);
        let suffix = "OUTPUT STYLE: expert-terse\nFn refs only, diff lines only.";
        let out = assemble_within_cap(&base, suffix, INSTRUCTION_CAP_TOKENS);

        assert!(
            out.contains("OUTPUT STYLE: expert-terse"),
            "protected guidance suffix must survive truncation"
        );
        assert!(
            count_tokens(&out) <= INSTRUCTION_CAP_TOKENS,
            "assembled output must respect the token cap"
        );
        assert!(
            out.len() < base.len(),
            "oversized base must have been truncated"
        );
    }

    #[test]
    fn under_cap_keeps_everything() {
        let base = "tool mapping block";
        let suffix = "OUTPUT STYLE: dense";
        let out = assemble_within_cap(base, suffix, INSTRUCTION_CAP_TOKENS);
        assert!(out.contains(base));
        assert!(out.contains(suffix));
    }

    #[test]
    fn empty_suffix_caps_base_only() {
        let base = "x\n".repeat(4000);
        let out = assemble_within_cap(&base, "", INSTRUCTION_CAP_TOKENS);
        assert!(count_tokens(&out) <= INSTRUCTION_CAP_TOKENS);
    }

    #[cfg(windows)]
    #[test]
    fn shell_hint_stays_within_its_budget() {
        // The skeleton budget grants the Windows shell hint exactly
        // STATIC_INSTRUCTION_SHELL_HINT_TOKENS — keep the hint inside it.
        let hint = build_shell_hint();
        let tokens = count_tokens(&hint);
        assert!(
            tokens <= STATIC_INSTRUCTION_SHELL_HINT_TOKENS,
            "shell hint = {tokens} tok, budget {STATIC_INSTRUCTION_SHELL_HINT_TOKENS}: {hint}"
        );
    }

    #[test]
    fn minimal_overhead_instructions_stay_within_budget() {
        // #361 faithful arm: with LEAN_CTX_MINIMAL no session/knowledge blocks
        // ride, so the per-turn instruction prefix must stay within the static
        // skeleton budget plus a small margin. Guards the "~3K tok/turn" critique
        // from regressing via dynamic-block creep.
        const MINIMAL_INSTRUCTION_BUDGET_TOKENS: usize =
            STATIC_INSTRUCTION_BUDGET_TDD_TOKENS + STATIC_INSTRUCTION_SHELL_HINT_TOKENS;
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::set_var("LEAN_CTX_MINIMAL", "1");
        let out = build_instructions(CrpMode::Compact);
        crate::test_env::remove_var("LEAN_CTX_MINIMAL");
        let tokens = count_tokens(&out);
        assert!(
            tokens <= MINIMAL_INSTRUCTION_BUDGET_TOKENS,
            "minimal-overhead instructions = {tokens} tok, budget {MINIMAL_INSTRUCTION_BUDGET_TOKENS}\n---\n{out}\n---"
        );
    }

    #[test]
    fn static_skeleton_stays_within_budget() {
        // #579: the static instruction skeleton (no session/knowledge blocks)
        // rides in EVERY session of EVERY install. Detail documentation
        // belongs in LEAN-CTX.md on disk — this assert stops silent creep.
        // Isolated data dir = default config, like a fresh install (the dev
        // machine's compression_level/profile must not leak into the budget).
        let _iso = crate::core::data_dir::isolated_data_dir();
        for (mode, base_budget) in [
            (CrpMode::Off, STATIC_INSTRUCTION_BUDGET_TOKENS),
            (CrpMode::Compact, STATIC_INSTRUCTION_BUDGET_TOKENS),
            (CrpMode::Tdd, STATIC_INSTRUCTION_BUDGET_TDD_TOKENS),
        ] {
            let budget = base_budget + STATIC_INSTRUCTION_SHELL_HINT_TOKENS;
            let out = build_instructions_for_test(mode);
            let tokens = count_tokens(&out);
            assert!(
                tokens <= budget,
                "static instructions for {mode:?} = {tokens} tok, budget {budget}\n---\n{out}\n---"
            );
        }
    }
}
