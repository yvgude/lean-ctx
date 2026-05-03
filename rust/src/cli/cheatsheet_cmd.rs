pub fn cmd_cheatsheet() {
    let ver = env!("CARGO_PKG_VERSION");
    let ver_pad = format!("v{ver}");
    let header = format!(
        "\x1b[1;36m╔══════════════════════════════════════════════════════════════╗\x1b[0m
\x1b[1;36m║\x1b[0m  \x1b[1;37mlean-ctx Workflow Cheat Sheet\x1b[0m                     \x1b[2m{ver_pad:>6}\x1b[0m  \x1b[1;36m║\x1b[0m
\x1b[1;36m╚══════════════════════════════════════════════════════════════╝\x1b[0m");
    println!(
        "{header}

\x1b[1;33m━━━ BEFORE YOU START ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  ctx_session load               \x1b[2m# restore previous session\x1b[0m
  ctx_overview task=\"...\"         \x1b[2m# task-aware file map\x1b[0m
  ctx_graph action=build          \x1b[2m# index project (first time)\x1b[0m
  ctx_knowledge action=recall     \x1b[2m# check stored project facts\x1b[0m

\x1b[1;32m━━━ WHILE CODING ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  ctx_read mode=full    \x1b[2m# first read (cached, re-reads: 99% saved)\x1b[0m
  ctx_read mode=map     \x1b[2m# context-only files (~93% saved)\x1b[0m
  ctx_read mode=diff    \x1b[2m# after editing (~98% saved)\x1b[0m
  ctx_read mode=sigs    \x1b[2m# API surface of large files (~95%)\x1b[0m
  ctx_multi_read        \x1b[2m# read multiple files at once\x1b[0m
  ctx_search            \x1b[2m# search with compressed results (~70%)\x1b[0m
  ctx_shell             \x1b[2m# run CLI with compressed output (~60-90%)\x1b[0m

\x1b[1;35m━━━ AFTER CODING ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  ctx_session finding \"...\"       \x1b[2m# record what you discovered\x1b[0m
  ctx_session decision \"...\"      \x1b[2m# record architectural choices\x1b[0m
  ctx_knowledge action=remember   \x1b[2m# store permanent project facts\x1b[0m
  ctx_knowledge action=consolidate \x1b[2m# auto-extract session insights\x1b[0m
  ctx_metrics                     \x1b[2m# see session statistics\x1b[0m

\x1b[1;34m━━━ MULTI-AGENT ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  ctx_agent action=register       \x1b[2m# announce yourself\x1b[0m
  ctx_agent action=list           \x1b[2m# see other active agents\x1b[0m
  ctx_agent action=post           \x1b[2m# share findings\x1b[0m
  ctx_agent action=read           \x1b[2m# check messages\x1b[0m

\x1b[1;31m━━━ READ MODE DECISION TREE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  Will edit?  → \x1b[1mfull\x1b[0m (re-reads: 13 tokens)  → after edit: \x1b[1mdiff\x1b[0m
  API only?   → \x1b[1msignatures\x1b[0m
  Deps/exports? → \x1b[1mmap\x1b[0m
  Very large? → \x1b[1mentropy\x1b[0m (information-dense lines)
  Browsing?   → \x1b[1maggressive\x1b[0m (syntax stripped)

\x1b[1;36m━━━ MONITORING ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  lean-ctx gain          \x1b[2m# visual savings dashboard\x1b[0m
  lean-ctx gain --live   \x1b[2m# live auto-updating (Ctrl+C)\x1b[0m
  lean-ctx dashboard     \x1b[2m# web dashboard with charts\x1b[0m
  lean-ctx gain --wrapped \x1b[2m# wrapped savings report\x1b[0m
  lean-ctx discover      \x1b[2m# find uncompressed commands\x1b[0m
  lean-ctx doctor        \x1b[2m# diagnose installation\x1b[0m
  lean-ctx update        \x1b[2m# self-update to latest\x1b[0m

\x1b[2m  Full guide: https://leanctx.com/docs/workflow\x1b[0m"
    );
}

pub fn cmd_terse(args: &[String]) {
    use crate::core::config::{Config, TerseAgent};

    let action = args.first().map(std::string::String::as_str);
    if let Some(level @ ("off" | "lite" | "full" | "ultra")) = action {
        let mut cfg = Config::load();
        cfg.terse_agent = match level {
            "lite" => TerseAgent::Lite,
            "full" => TerseAgent::Full,
            "ultra" => TerseAgent::Ultra,
            _ => TerseAgent::Off,
        };
        if let Err(e) = cfg.save() {
            eprintln!("Error saving config: {e}");
            std::process::exit(1);
        }
        let desc = match level {
            "lite" => "concise responses, bullet points over paragraphs",
            "full" => "maximum density, diff-only code, 1-sentence explanations",
            "ultra" => "expert pair-programmer mode, minimal narration",
            _ => "normal verbose output",
        };
        println!("Terse agent mode: {level} ({desc})");
        println!("Restart your agent/IDE for changes to take effect.");
    } else {
        let cfg = Config::load();
        let effective = TerseAgent::effective(&cfg.terse_agent);
        let name = match &effective {
            TerseAgent::Off => "off",
            TerseAgent::Lite => "lite",
            TerseAgent::Full => "full",
            TerseAgent::Ultra => "ultra",
        };
        println!("Terse agent mode: {name}");
        println!();
        println!("Usage: lean-ctx terse <off|lite|full|ultra>");
        println!("  off   — Normal verbose output (default)");
        println!("  lite  — Concise: bullet points, skip narration");
        println!("  full  — Dense: diff-only, 1-sentence max");
        println!("  ultra — Expert: minimal narration, code speaks");
        println!();
        println!("Override per session: LEAN_CTX_TERSE_AGENT=full");
        println!("Override per project: terse_agent = \"full\" in .lean-ctx.toml");
    }
}
