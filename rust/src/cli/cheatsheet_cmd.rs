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
  ctx_knowledge action=consolidate \x1b[2m# import session + run lifecycle\x1b[0m
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
  lean-ctx update        \x1b[2m# self-update (or 'update 3.8.5' to pin)\x1b[0m

\x1b[2m  Full guide: https://leanctx.com/docs/workflow\x1b[0m"
    );
}

pub fn cmd_compression(args: &[String]) {
    use crate::core::config::{CompressionLevel, Config};

    let action = args.first().map(std::string::String::as_str);
    if let Some(level @ ("off" | "lite" | "standard" | "max")) = action {
        if let Err(e) = Config::update_global(|cfg| {
            cfg.compression_level = match level {
                "lite" => CompressionLevel::Lite,
                "standard" => CompressionLevel::Standard,
                "max" => CompressionLevel::Max,
                _ => CompressionLevel::Off,
            };
        }) {
            eprintln!("Error saving config: {e}");
            std::process::exit(1);
        }
        let effective = CompressionLevel::from_str_label(level).unwrap_or(CompressionLevel::Off);
        println!("Compression level: {level} — {}", effective.description());
        let home = dirs::home_dir().unwrap_or_default();
        let result = crate::rules_inject::inject_all_rules(&home);
        if !result.updated.is_empty() {
            println!(
                "Updated {} rules file(s) with compression prompt.",
                result.updated.len()
            );
        }
        println!("Restart your agent/IDE for changes to take effect.");
    } else {
        let cfg = Config::load();
        let effective = CompressionLevel::effective(&cfg);
        println!("Compression level: {}", effective.label());
        println!();
        println!("Usage: lean-ctx compression <off|lite|standard|max>");
        println!("       lean-ctx terse <off|lite|standard|max>  (alias)");
        println!();
        println!("  off      — {}", CompressionLevel::Off.description());
        println!("  lite     — {}", CompressionLevel::Lite.description());
        println!("  standard — {}", CompressionLevel::Standard.description());
        println!("  max      — {}", CompressionLevel::Max.description());
        println!();
        println!("Override per session:  LEAN_CTX_COMPRESSION=standard");
        println!("Override per project:  compression_level = \"standard\" in .lean-ctx.toml");
        println!();

        let (ta, od, crp, tm) = effective.to_components();
        let ta_name = match ta {
            crate::core::config::TerseAgent::Off => "off",
            crate::core::config::TerseAgent::Lite => "lite",
            crate::core::config::TerseAgent::Full => "full",
            crate::core::config::TerseAgent::Ultra => "ultra",
        };
        let od_name = match od {
            crate::core::config::OutputDensity::Normal => "normal",
            crate::core::config::OutputDensity::Terse => "terse",
            crate::core::config::OutputDensity::Ultra => "ultra",
        };
        println!("Active components:");
        println!("  Agent prompt:    {ta_name}");
        println!("  Output density:  {od_name}");
        println!("  CRP mode:        {crp}");
        println!("  Terse session:   {tm}");
    }
}
