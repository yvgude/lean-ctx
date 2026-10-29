pub fn cmd_cheatsheet() {
    let ver = env!("CARGO_PKG_VERSION");
    // Right-aligned in the 31 columns the title leaves inside the 62-column
    // frame, minus the 2-space gutter before the border. A fixed literal run of
    // spaces here drifts as soon as the version string changes length.
    // ponytail: overflows past a 29-char version; cargo versions never get there.
    let ver_pad = format!("v{ver}");
    let header = format!(
        "\x1b[1;36m╔══════════════════════════════════════════════════════════════╗\x1b[0m
\x1b[1;36m║\x1b[0m  \x1b[1;37mlean-ctx Context Cheat Sheet\x1b[0m\x1b[2m{ver_pad:>30}\x1b[0m  \x1b[1;36m║\x1b[0m
\x1b[1;36m╚══════════════════════════════════════════════════════════════╝\x1b[0m");
    println!(
        "{header}

\x1b[1;33m━━━ BEFORE YOU START ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  ctx_session load               \x1b[2m# restore previous session\x1b[0m
  ctx_compose task=\"...\"          \x1b[2m# ranked files + symbols for a task\x1b[0m
  ctx_graph action=build          \x1b[2m# index project (first time)\x1b[0m
  ctx_knowledge action=recall     \x1b[2m# check stored project facts\x1b[0m

\x1b[1;32m━━━ WHILE CODING ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  ctx_read mode=full    \x1b[2m# exact source when you need it\x1b[0m
  ctx_read mode=map     \x1b[2m# structure and dependencies first\x1b[0m
  ctx_read mode=diff    \x1b[2m# inspect changes after editing\x1b[0m
  ctx_read mode=sigs    \x1b[2m# API surface of large files\x1b[0m
  ctx_search            \x1b[2m# focused search results with references\x1b[0m
  ctx_shell             \x1b[2m# shape large CLI output with recovery\x1b[0m
  ctx_read raw=true     \x1b[2m# escape hatch: exact bytes (CLI: lean-ctx raw \"cmd\")\x1b[0m

\x1b[1;35m━━━ AFTER CODING ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  ctx_session finding \"...\"       \x1b[2m# record what you discovered\x1b[0m
  ctx_session decision \"...\"      \x1b[2m# record architectural choices\x1b[0m
  ctx_knowledge action=remember   \x1b[2m# store permanent project facts\x1b[0m
  ctx_knowledge action=consolidate \x1b[2m# import session + run lifecycle\x1b[0m
  ctx_metrics                     \x1b[2m# see session statistics\x1b[0m

\x1b[1;34m━━━ EXPERIMENTAL LOCAL COLLABORATION (OPT-IN) ━━━━━━━━━━━━━━━\x1b[0m
  ctx_agent action=register       \x1b[2m# local development helper; not default MCP\x1b[0m
  ctx_agent action=list           \x1b[2m# inspect local registrations\x1b[0m
  ctx_agent action=post           \x1b[2m# share a local finding\x1b[0m
  ctx_agent action=read           \x1b[2m# inspect local messages\x1b[0m

\x1b[1;31m━━━ READ MODE DECISION TREE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  Will edit?  → \x1b[1mfull\x1b[0m (eligible re-reads use cache references)  → after edit: \x1b[1mdiff\x1b[0m
  API only?   → \x1b[1msignatures\x1b[0m
  Deps/exports? → \x1b[1mmap\x1b[0m
  Very large? → \x1b[1mentropy\x1b[0m (information-dense lines)
  Browsing?   → \x1b[1maggressive\x1b[0m (syntax stripped)

\x1b[1;36m━━━ MONITORING ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\x1b[0m
  lean-ctx cockpit       \x1b[2m# Savings · Spend · Proof (localhost:3333)\x1b[0m
  lean-ctx gain          \x1b[2m# terminal summary of local context-usage estimates\x1b[0m
  lean-ctx gain --wrapped \x1b[2m# local usage summary (recap: Cockpit → #report)\x1b[0m
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
