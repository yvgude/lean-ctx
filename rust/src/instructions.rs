use crate::tools::CrpMode;

/// Claude Code truncates MCP server instructions at 2048 characters.
/// Full instructions are installed as `$CLAUDE_CONFIG_DIR/rules/lean-ctx.md`
/// (defaulting to `~/.claude/rules/lean-ctx.md`) instead.
/// Session state is dynamically appended to the MCP instructions for continuity.
///
/// Universal instruction cap for all MCP clients (in tokens, not bytes).
/// Enforced via `count_tokens` so truncation is accurate regardless of
/// character mix (ASCII, CJK, emoji).
const INSTRUCTION_CAP_TOKENS: usize = 1200;

pub fn build_instructions(crp_mode: CrpMode) -> String {
    build_instructions_with_client(crp_mode, "")
}

pub fn build_instructions_with_client(crp_mode: CrpMode, client_name: &str) -> String {
    if is_claude_code_client(client_name) {
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
    if is_claude_code_client(client_name) {
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
    if is_claude_code_client(client_name) {
        return build_claude_code_instructions();
    }
    build_full_instructions_for_compiler(crp_mode, client_name, unified_tool_mode)
}

fn is_claude_code_client(client_name: &str) -> bool {
    let lower = client_name.to_lowercase();
    lower.contains("claude") && !lower.contains("cursor")
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

ctx_read modes: full|map|signatures|diff|task|reference|aggressive|entropy|lines:N-M
Auto-selects mode. Re-reads ~13 tok. File refs F1,F2.. persist.

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
            let positioned = crate::core::litm::position_optimize(session);
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
    let compat = rc::compatibility_block();
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
\n\
{compat}\n\
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
--- ORIGIN ---\n\
{origin}\n\
\n\
{litm_pref}\
{litm_end_block}",
        decoder_block = crate::core::protocol::instruction_decoder_block(),
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
    let guidance_suffix = match crp_mode {
        CrpMode::Off => format!("{terse_block}{intelligence_block}"),
        CrpMode::Compact => format!(
            "CRP MODE: compact\n\
Omit filler. Abbreviate: fn,cfg,impl,deps,req,res,ctx,err,ret,arg,val,ty,mod.\n\
Diff lines (+/-) only. TARGET: <=200 tok. Trust tool outputs.\n\n\
{terse_block}{intelligence_block}"
        ),
        CrpMode::Tdd => format!(
            "CRP MODE: tdd\n\
Max density. Every token carries meaning. Fn refs only, diff lines (+/-) only.\n\
Abbreviate: fn,cfg,impl,deps,req,res,ctx,err,ret,arg,val,ty,mod.\n\
+F1:42 param(timeout:Duration) | -F1:10-15 | ~F1:42 old->new\n\
BUDGET: <=150 tok. ZERO NARRATION. Trust tool outputs.\n\n\
{terse_block}{intelligence_block}"
        ),
    };

    assemble_within_cap(&base, &guidance_suffix, INSTRUCTION_CAP_TOKENS)
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
    let compat = rc::compatibility_block();
    let read_modes = rc::ctx_read_modes_block();
    let auto_block = rc::automation_block();
    let cep = rc::cep_block();
    let litm_pref = rc::litm_end_block(rc::Mode::Mcp);

    let mut base = format!(
        "\
CRITICAL: ALWAYS use lean-ctx MCP tools instead of native equivalents for token savings.\n\
\n\
{tool_bullets}\n\
\n\
{compat}\n\
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
--- ORIGIN ---\n\
{origin}\n\
\n\
{litm_pref}\
{litm_end_block}",
        decoder_block = crate::core::protocol::instruction_decoder_block(),
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

    match crp_mode {
        CrpMode::Off => format!("{base}\n\n{terse_block}{intelligence_block}"),
        CrpMode::Compact => {
            format!(
                "{base}\n\n\
CRP MODE: compact\n\
Omit filler. Abbreviate: fn,cfg,impl,deps,req,res,ctx,err,ret,arg,val,ty,mod.\n\
Diff lines (+/-) only. TARGET: <=200 tok. Trust tool outputs.\n\n\
{terse_block}{intelligence_block}"
            )
        }
        CrpMode::Tdd => {
            format!(
                "{base}\n\n\
CRP MODE: tdd\n\
Max density. Every token carries meaning. Fn refs only, diff lines (+/-) only.\n\
Abbreviate: fn,cfg,impl,deps,req,res,ctx,err,ret,arg,val,ty,mod.\n\
+F1:42 param(timeout:Duration) | -F1:10-15 | ~F1:42 old->new\n\
BUDGET: <=150 tok. ZERO NARRATION. Trust tool outputs.\n\n\
{terse_block}{intelligence_block}"
            )
        }
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
    let compat = rc::compatibility_block();
    let read_modes = rc::ctx_read_modes_block();
    let auto_blk = rc::automation_block();
    let cep = rc::cep_block();
    let litm_pref = rc::litm_end_block(rc::Mode::Mcp);

    let mut base = format!(
        "\
CRITICAL: ALWAYS use lean-ctx MCP tools instead of native equivalents for token savings.\n\
\n\
{tool_bullets}\n\
\n\
{compat}\n\
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
--- ORIGIN ---\n\
{origin}\n\
\n\
{litm_pref}\
{litm_end_block}",
        decoder_block = crate::core::protocol::instruction_decoder_block(),
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

    match crp_mode {
        CrpMode::Off => format!("{base}\n\n{intelligence_block}"),
        CrpMode::Compact => {
            format!(
                "{base}\n\n\
CRP MODE: compact\n\
Omit filler. Abbreviate: fn,cfg,impl,deps,req,res,ctx,err,ret,arg,val,ty,mod.\n\
Diff lines (+/-) only. TARGET: <=200 tok. Trust tool outputs.\n\n\
{intelligence_block}"
            )
        }
        CrpMode::Tdd => {
            format!(
                "{base}\n\n\
CRP MODE: tdd\n\
Max density. Every token carries meaning. Fn refs only, diff lines (+/-) only.\n\
Abbreviate: fn,cfg,impl,deps,req,res,ctx,err,ret,arg,val,ty,mod.\n\
+F1:42 param(timeout:Duration) | -F1:10-15 | ~F1:42 old->new\n\
BUDGET: <=150 tok. ZERO NARRATION. Trust tool outputs.\n\n\
{intelligence_block}"
            )
        }
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
    "\
OUTPUT EFFICIENCY:\n\
• Never echo tool output code. Never add narration comments. Show only changed code.\n\
• [TASK:type] and SCOPE hints included. Architecture=thorough, generate=code."
        .to_string()
}

fn build_shell_hint() -> String {
    if !cfg!(windows) {
        return String::new();
    }
    let name = crate::shell::shell_name();
    let is_posix = matches!(name.as_str(), "bash" | "sh" | "zsh" | "fish");
    if is_posix {
        format!(
            "\nSHELL: {name} (POSIX). Use POSIX commands (cat, head, grep, find, ls). \
             Do NOT use PowerShell cmdlets (Get-Content, Select-Object, Get-ChildItem).\n"
        )
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
}
