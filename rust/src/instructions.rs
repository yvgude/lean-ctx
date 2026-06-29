use crate::core::config::CompressionLevel;
use crate::core::rules_canonical::{self as rc, Wrapper};
use crate::tools::CrpMode;

/// Universal instruction cap for all MCP clients (in tokens, not bytes).
const INSTRUCTION_CAP_TOKENS: usize = 800;

/// Token budget for the static instruction skeleton (no session/knowledge
/// state).  Asserted in CI so instruction creep cannot silently tax every
/// session. Raised (520→540 default, 600→640 TDD) as a deliberate, reviewed
/// bump for the sharpened ctx_* redirects (#1030): the skeleton stays far under
/// the 800-token runtime cap (`INSTRUCTION_CAP_TOKENS`), so the stronger nudge
/// ships in full without truncating anything live — trimming it back would only
/// weaken adoption.
#[cfg(test)]
const STATIC_INSTRUCTION_BUDGET_TOKENS: usize = 540;
#[cfg(test)]
const STATIC_INSTRUCTION_BUDGET_TDD_TOKENS: usize = 640;
/// Windows carries a one-line SHELL hint inside the skeleton.
#[cfg(all(test, windows))]
const STATIC_INSTRUCTION_SHELL_HINT_TOKENS: usize = 25;
#[cfg(all(test, not(windows)))]
const STATIC_INSTRUCTION_SHELL_HINT_TOKENS: usize = 0;

#[must_use]
pub fn build_instructions(crp_mode: CrpMode) -> String {
    build_instructions_with_client(crp_mode, "")
}

#[must_use]
pub fn build_instructions_with_client(crp_mode: CrpMode, client_name: &str) -> String {
    let cfg = crate::core::config::Config::load();
    let minimal = cfg.minimal_overhead_effective_for_client(client_name);
    let shadow = cfg.shadow_mode;
    // Cross-channel dedup: if the client auto-loads compression from its own rule
    // file, skip it here to avoid duplicate billing.
    let level = if client_loads_compression_from_file(client_name) {
        CompressionLevel::Off
    } else {
        CompressionLevel::effective(&cfg)
    };
    build_full_instructions(crp_mode, client_name, minimal, level, shadow)
}

/// Deterministic STATIC Claude Code instructions for the char-budget test: the
/// cold first-contact handshake surface (skeleton + shell hint + decoder +
/// CLAUDE.md pointer + guidance). It pins `minimal=true` (the dynamic
/// session/knowledge/gotcha payload is governed by `INSTRUCTION_CAP_TOKENS`, not
/// the char budget) plus `level=Off`, `shadow=false` (the default template), so
/// the result is independent of the developer's local lean-ctx config and the
/// assertion stays deterministic (#498) for every contributor, not just clean CI.
#[must_use]
pub fn claude_code_static_instructions_for_test() -> String {
    build_full_instructions(CrpMode::Off, "", true, CompressionLevel::Off, false)
}

/// Deterministic variant for tests (no session/knowledge state).
#[must_use]
pub fn build_instructions_for_test(crp_mode: CrpMode) -> String {
    let shadow = false;
    // Resolve the effective compression level from config/env (matches the live
    // build_full_instructions path) so terse/compression env vars are honoured.
    let level = CompressionLevel::effective(&crate::core::config::Config::load());
    let skeleton = rc::render(shadow, Wrapper::Bare, level);
    let shell_hint = build_shell_hint();

    let base = format!(
        "{skeleton}\n\
        {shell_hint}\n\
        {decoder_block}\n\
        {origin}",
        decoder_block =
            crate::core::protocol::instruction_decoder_block(matches!(crp_mode, CrpMode::Tdd)),
        origin = crate::core::integrity::origin_line(),
    );

    match crp_mode_suffix(crp_mode) {
        "" => format!("{base}\n\n{}", rc::INTELLIGENCE),
        crp => format!("{base}\n\n{crp}\n\n{}", rc::INTELLIGENCE),
    }
}

/// Deterministic instruction builder for the Instruction Compiler.
/// Uses shadow mode (COMPACT_SHADOW profile) to avoid duplicating
/// BULLETS/NEVER/CRITICAL that the CLAUDE.md / dedicated rule file carries.
#[must_use]
pub fn build_instructions_with_client_for_compiler(
    crp_mode: CrpMode,
    client_name: &str,
    _unified_tool_mode: bool,
) -> String {
    let skeleton = rc::render(true, Wrapper::Bare, CompressionLevel::Off);
    let shell_hint = build_shell_hint();

    let base = format!(
        "{skeleton}\n\
        {shell_hint}\n\
        {decoder_block}\n\
        {origin}",
        decoder_block =
            crate::core::protocol::instruction_decoder_block(matches!(crp_mode, CrpMode::Tdd)),
        origin = crate::core::integrity::origin_line(),
    );

    let _ = client_name;

    match crp_mode_suffix(crp_mode) {
        "" => format!("{base}\n\n{}", rc::INTELLIGENCE),
        crp => format!("{base}\n\n{crp}\n\n{}", rc::INTELLIGENCE),
    }
}

/// LITM calibration manifest rotation (#539).
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

/// Display path for the Claude config directory (respected by CLAUDE_CONFIG_DIR).
#[must_use]
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

// ── MCP per-session instructions builder ──────────────────────

fn build_full_instructions(
    crp_mode: CrpMode,
    client_name: &str,
    minimal: bool,
    level: CompressionLevel,
    shadow: bool,
) -> String {
    let profile = crate::core::litm::LitmProfile::from_client_name(client_name);
    let loaded_session = if minimal {
        None
    } else {
        crate::core::session::SessionState::load_latest()
    };

    let (session_block, litm_end_block) = match loaded_session {
        Some(ref session) => {
            rotate_wakeup_manifest(session, profile.name);
            let share = crate::core::litm_calibration::begin_share(profile.name);
            let mut positioned = crate::core::litm::position_optimize_with_share(session, share);
            // #962: hard token ceiling so the re-injected ACTIVE SESSION block can
            // never crowd out the user's task (deterministic, generous default).
            positioned.enforce_token_budget(crate::core::litm::active_session_budget());
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

    let health_block = match &project_root_for_blocks {
        Some(root) => {
            let block = crate::core::code_health::persist::format_session_block(root);
            if block.is_empty() {
                String::new()
            } else {
                format!("\n{block}\n")
            }
        }
        None => String::new(),
    };

    let shell_hint = build_shell_hint();

    // Skeleton includes tool-mapping rules + compression prompt (if level active).
    // Shadow mode omits BULLETS/NEVER/CRITICAL automatically.
    let skeleton = rc::render(shadow, Wrapper::Bare, level);

    // Pointer to the full rule file (honours CLAUDE_CONFIG_DIR): agents load the
    // detailed instructions on demand from there instead of inlining them.
    let config_dir = claude_config_dir_display();

    let base = format!(
        "{skeleton}\n\
        {shell_hint}\n\
        {decoder_block}\n\
        Full instructions at {config_dir}/CLAUDE.md\n\
        {session_block}\n\
        {knowledge_block}\n\
        {gotcha_block}\n\
        {health_block}\n\
        {origin}\n\
        {litm_end_block}",
        decoder_block =
            crate::core::protocol::instruction_decoder_block(matches!(crp_mode, CrpMode::Tdd)),
        origin = crate::core::integrity::origin_line(),
        litm_end_block = litm_end_block
    );

    // Guidance suffix: CRP mode + general output rule.
    // This is the operational contract — protected from truncation.
    let guidance_suffix = match crp_mode_suffix(crp_mode) {
        "" => rc::INTELLIGENCE.to_string(),
        crp => format!("{crp}\n\n{}", rc::INTELLIGENCE),
    };

    assemble_within_cap(&base, &guidance_suffix, INSTRUCTION_CAP_TOKENS)
}

fn crp_mode_suffix(crp_mode: CrpMode) -> &'static str {
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
    let byte_approx = cap_tokens * 4;
    let safe = s.floor_char_boundary(byte_approx.min(s.len()));
    s[..safe].to_string()
}

/// Backward-compat alias kept for external callers.
#[must_use]
pub fn claude_code_instructions() -> String {
    build_instructions(CrpMode::Off)
}

fn client_loads_compression_from_file(client_name: &str) -> bool {
    crate::core::home::resolve_home_dir().is_some_and(|home| {
        crate::core::rules_channel::client_autoloads_compression(client_name, &home)
    })
}

fn build_shell_hint() -> String {
    if !cfg!(windows) {
        return String::new();
    }
    let name = crate::shell::shell_name();
    let is_posix = matches!(name.as_str(), "bash" | "sh" | "zsh" | "fish");
    if is_posix {
        format!("\nSHELL: {name} (POSIX) — no PowerShell cmdlets.\n")
    } else if name.contains("powershell") || name.contains("pwsh") {
        format!("\nSHELL: {name}. Use PowerShell cmdlets.\n")
    } else {
        format!("\nSHELL: {name}.\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tokens::count_tokens;

    #[test]
    fn guidance_suffix_survives_oversized_base() {
        let base = "SESSION LINE\n".repeat(4000);
        let suffix = "OUTPUT STYLE: expert-terse\nFn refs only, diff lines only.";
        let out = assemble_within_cap(&base, suffix, INSTRUCTION_CAP_TOKENS);
        assert!(out.contains("OUTPUT STYLE: expert-terse"));
        assert!(count_tokens(&out) <= INSTRUCTION_CAP_TOKENS);
        assert!(out.len() < base.len());
    }

    #[test]
    fn empty_client_never_dedups_compression() {
        assert!(!client_loads_compression_from_file(""));
        assert!(!client_loads_compression_from_file("totally-unknown-agent"));
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
        let hint = build_shell_hint();
        let tokens = count_tokens(&hint);
        assert!(
            tokens <= STATIC_INSTRUCTION_SHELL_HINT_TOKENS,
            "shell hint = {tokens} tok, budget {STATIC_INSTRUCTION_SHELL_HINT_TOKENS}: {hint}"
        );
    }

    #[test]
    fn minimal_overhead_instructions_stay_within_budget() {
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
