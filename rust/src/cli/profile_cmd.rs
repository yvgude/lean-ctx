use crate::core::profiles;
use crate::core::tool_profiles::{self, ToolProfile};

pub fn cmd_profile(args: &[String]) {
    let action = args.first().map_or("list", String::as_str);

    match action {
        // Tool profile subcommands
        "tools" => cmd_tool_profile(&args[1..]),
        "minimal" | "min" | "standard" | "std" | "power" | "full" | "all" | "lean" | "lazy"
        | "reset" => {
            cmd_tool_profile_switch(action);
            println!("  \x1b[2mTip: the canonical command is `lean-ctx tools {action}`.\x1b[0m");
        }

        // Existing compression profile subcommands
        "list" | "ls" => cmd_profile_list(),
        "show" => {
            let name = args
                .get(1)
                .map_or_else(profiles::active_profile_name, Clone::clone);
            cmd_profile_show(&name);
        }
        "active" | "current" => cmd_profile_active(),
        "diff" => {
            if args.len() < 3 {
                eprintln!("Usage: lean-ctx profile diff <profile-a> <profile-b>");
                std::process::exit(1);
            }
            cmd_profile_diff(&args[1], &args[2]);
        }
        "create" => {
            if args.len() < 2 {
                eprintln!("Usage: lean-ctx profile create <name> [--from <base>] [--global]");
                std::process::exit(1);
            }
            let name = &args[1];
            let base = args
                .iter()
                .position(|a| a == "--from")
                .and_then(|i| args.get(i + 1))
                .map(String::as_str);
            let global = args.iter().any(|a| a == "--global");
            cmd_profile_create(name, base, global);
        }
        "set" => {
            if args.len() < 2 {
                eprintln!("Usage: lean-ctx profile set <name>");
                eprintln!("  Sets LEAN_CTX_PROFILE for the current shell.");
                std::process::exit(1);
            }
            cmd_profile_set(&args[1]);
        }
        _ => {
            if profiles::load_profile(action).is_some() {
                cmd_profile_show(action);
            } else {
                print_profile_help();
                std::process::exit(1);
            }
        }
    }
}

fn cmd_profile_list() {
    let list = profiles::list_profiles();
    let active = profiles::active_profile_name();

    let header = format!("  {:<16} {:<10} {}", "Name", "Source", "Description");
    let sep = format!("  {}", "\u{2500}".repeat(60));
    println!("Available profiles:\n");
    println!("{header}");
    println!("{sep}");

    for p in &list {
        let marker = if p.name == active { " *" } else { "  " };
        println!("{marker}{:<16} {:<10} {}", p.name, p.source, p.description);
    }

    println!("\n  Active: {active}");
    println!("  Set via: LEAN_CTX_PROFILE=<name> or lean-ctx profile set <name>");
}

fn cmd_profile_show(name: &str) {
    if let Some(profile) = profiles::load_profile(name) {
        println!("Profile: {name}\n");
        println!("{}", profiles::format_as_toml(&profile));
    } else {
        eprintln!("Profile '{name}' not found.");
        eprintln!("Run 'lean-ctx profile list' to see available profiles.");
        std::process::exit(1);
    }
}

fn cmd_profile_active() {
    let name = profiles::active_profile_name();
    let profile = profiles::active_profile();
    println!("Active profile: {name}\n");
    println!("{}", profiles::format_as_toml(&profile));
}

fn cmd_profile_diff(name_a: &str, name_b: &str) {
    let Some(a) = profiles::load_profile(name_a) else {
        eprintln!("Profile '{name_a}' not found.");
        std::process::exit(1);
    };
    let Some(b) = profiles::load_profile(name_b) else {
        eprintln!("Profile '{name_b}' not found.");
        std::process::exit(1);
    };

    println!("Profile diff: {name_a} vs {name_b}\n");

    let diffs = collect_diffs(&a, &b);
    if diffs.is_empty() {
        println!("  No differences.");
    } else {
        println!("  {:<32} {:<20} {:<20}", "Field", name_a, name_b);
        println!("  {}", "\u{2500}".repeat(72));
        for (field, val_a, val_b) in &diffs {
            println!("  {field:<32} {val_a:<20} {val_b:<20}");
        }
    }
}

fn collect_diffs(a: &profiles::Profile, b: &profiles::Profile) -> Vec<(String, String, String)> {
    let mut diffs = Vec::new();

    macro_rules! cmp {
        ($section:ident . $field:ident) => {
            let va = format!("{:?}", a.$section.$field);
            let vb = format!("{:?}", b.$section.$field);
            if va != vb {
                diffs.push((
                    format!("{}.{}", stringify!($section), stringify!($field)),
                    va,
                    vb,
                ));
            }
        };
    }

    cmp!(read.default_mode);
    cmp!(read.max_tokens_per_file);
    cmp!(read.prefer_cache);
    cmp!(compression.crp_mode);
    cmp!(compression.output_density);
    cmp!(compression.entropy_threshold);
    cmp!(translation.enabled);
    cmp!(translation.ruleset);
    cmp!(layout.enabled);
    cmp!(layout.min_lines);
    cmp!(budget.max_context_tokens);
    cmp!(budget.max_shell_invocations);
    cmp!(budget.max_cost_usd);
    cmp!(pipeline.intent);
    cmp!(pipeline.relevance);
    cmp!(pipeline.compression);
    cmp!(pipeline.translation);
    cmp!(autonomy.enabled);
    cmp!(autonomy.auto_preload);
    cmp!(autonomy.auto_dedup);
    cmp!(autonomy.auto_related);
    cmp!(autonomy.silent_preload);
    cmp!(autonomy.auto_prefetch);
    cmp!(autonomy.auto_response);
    cmp!(autonomy.dedup_threshold);
    cmp!(autonomy.prefetch_max_files);
    cmp!(autonomy.prefetch_budget_tokens);
    cmp!(autonomy.response_min_tokens);
    cmp!(autonomy.checkpoint_interval);

    diffs
}

fn cmd_profile_create(name: &str, base: Option<&str>, global: bool) {
    let base_profile = base
        .and_then(profiles::load_profile)
        .unwrap_or_else(profiles::active_profile);

    let mut new_profile = base_profile;
    new_profile.profile.name = name.to_string();
    new_profile.profile.inherits = base.map(String::from);
    new_profile.profile.description = String::new();

    let dir = if global {
        let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
            eprintln!("Cannot determine global data directory.");
            std::process::exit(1);
        };
        data_dir.join("profiles")
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(".lean-ctx")
            .join("profiles")
    };

    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("Cannot create directory {}: {e}", dir.display());
        std::process::exit(1);
    }

    let path = dir.join(format!("{name}.toml"));
    let toml_content = profiles::format_as_toml(&new_profile);

    if let Err(e) = std::fs::write(&path, &toml_content) {
        eprintln!("Error writing {}: {e}", path.display());
        std::process::exit(1);
    }

    println!("Created profile '{name}' at {}", path.display());
    if let Some(b) = base {
        println!("  Based on: {b}");
    }
    println!("\nEdit the file to customize, then activate with:");
    println!("  LEAN_CTX_PROFILE={name}");
}

fn cmd_profile_set(name: &str) {
    if profiles::load_profile(name).is_none() {
        eprintln!("Profile '{name}' not found. Available profiles:");
        for p in profiles::list_profiles() {
            eprintln!("  {}", p.name);
        }
        std::process::exit(1);
    }

    println!("To activate profile '{name}', run:\n");
    println!("  export LEAN_CTX_PROFILE={name}\n");
    println!(
        "Or add it to your shell config ({}).",
        crate::shell_hook::shell_rc_file()
    );
}

// ─── Tool Profile Commands ───────────────────────────────────────────────

fn cmd_tool_profile(args: &[String]) {
    let action = args.first().map_or("show", String::as_str);

    match action {
        "list" | "ls" => cmd_tool_profile_list(),
        "show" | "current" => cmd_tool_profile_show(),
        "minimal" | "min" | "standard" | "std" | "power" | "full" | "all" | "lean" | "lazy"
        | "reset" => {
            cmd_tool_profile_switch(action);
        }
        _ => {
            if ToolProfile::parse(action).is_some() {
                cmd_tool_profile_switch(action);
            } else {
                eprintln!("Unknown tool profile '{action}'.");
                eprintln!("Available: lean (default), minimal, standard, power");
                std::process::exit(1);
            }
        }
    }
}

fn cmd_tool_profile_show() {
    let cfg = crate::core::config::Config::load();
    let profile = cfg.tool_profile_effective();
    let registry_count = crate::server::registry::tool_count();
    let pinned = cfg.tool_profile.is_some()
        || std::env::var("LEAN_CTX_TOOL_PROFILE").is_ok()
        || !cfg.tools_enabled.is_empty();

    if !pinned {
        let lazy_count = crate::tool_defs::core_tool_names().len();
        println!("Tool Profile: lean (default)");
        println!("  Tools advertised: {lazy_count} (lazy core)");
        println!("  All {registry_count} registered tools stay callable via ctx_call.");
        println!("\n  Advertised tools:");
        for name in crate::tool_defs::core_tool_names() {
            println!("    {name}");
        }
        println!("\n  Switch with: lean-ctx tools <minimal|standard|power>");
        return;
    }

    let count_str = match &profile {
        ToolProfile::Power => format!("{registry_count}"),
        ToolProfile::Custom(list) => format!("{}", list.len()),
        other => format!("{}", other.tool_count()),
    };

    println!("Tool Profile: {}", profile.as_str());
    println!("  Tools exposed: {count_str}");
    println!("  Description:   {}", profile.description());

    if let Some(ref cfg_val) = cfg.tool_profile {
        println!("  Source:         config.toml (tool_profile = \"{cfg_val}\")");
    }
    if std::env::var("LEAN_CTX_TOOL_PROFILE").is_ok() {
        println!("  Source:         LEAN_CTX_TOOL_PROFILE env var (overrides config)");
    }

    if !matches!(profile, ToolProfile::Power) {
        println!("\n  Enabled tools:");
        let names = profile.tool_names();
        for name in &names {
            println!("    {name}");
        }
    }

    println!("\n  Switch with: lean-ctx tools <lean|minimal|standard|power>");
    if matches!(profile, ToolProfile::Power) {
        println!("  Tip: `lean-ctx tools lean` advertises only the lazy core (lowest overhead).");
    }
}

fn cmd_tool_profile_list() {
    let cfg = crate::core::config::Config::load();
    let active = cfg.tool_profile_effective();
    let registry_count = crate::server::registry::tool_count();
    // A persisted unpin alias (`tool_profile = "lean"`) or `…=lean` in the env
    // is NOT a pin — otherwise `show` would report "power" for the default (#431).
    let pinned = cfg
        .tool_profile
        .as_deref()
        .is_some_and(|p| !tool_profiles::is_unpinned_alias(p))
        || std::env::var("LEAN_CTX_TOOL_PROFILE")
            .is_ok_and(|v| !v.trim().is_empty() && !tool_profiles::is_unpinned_alias(v.trim()))
        || !cfg.tools_enabled.is_empty();
    let active_name = if pinned { active.as_str() } else { "lean" };
    let lazy_count = crate::tool_defs::core_tool_names().len();

    println!("Tool Profiles:\n");
    println!("  {:<12} {:<8} Description", "Name", "Tools");
    println!("  {}", "\u{2500}".repeat(60));

    let lean_marker = if active_name == "lean" { "* " } else { "  " };
    println!(
        "{lean_marker}{:<12} {lazy_count:<8} Lazy core advertised, all tools via ctx_call (default)",
        "lean"
    );
    for info in tool_profiles::list_profiles() {
        let marker = if info.name == active_name { "* " } else { "  " };
        let count = if info.name == "power" {
            format!("{registry_count}")
        } else {
            info.tool_count.to_string()
        };
        println!(
            "{marker}{:<12} {:<8} {}",
            info.name, count, info.description
        );
    }

    println!("\n  Active: {active_name}");
    println!("  Switch: lean-ctx profile <name>");
    println!("  Env:    LEAN_CTX_TOOL_PROFILE=<name>");
}

fn cmd_tool_profile_switch(name: &str) {
    // "lean" is not a pinned profile — it removes the config key, restoring
    // the default: lazy core advertised (~13 schemas), everything reachable
    // through ctx_call (#575).
    if tool_profiles::is_unpinned_alias(name) {
        if let Err(e) = tool_profiles::clear_profile_in_config() {
            eprintln!("Error saving profile: {e}");
            std::process::exit(1);
        }
        let lazy_count = crate::tool_defs::core_tool_names().len();
        println!("Tool profile set to: lean (default)");
        println!("  Tools advertised: {lazy_count} (lazy core)");
        println!("  All other tools stay callable via ctx_call.");
        println!("\n  Restart your AI tool / IDE for changes to take effect.");
        return;
    }

    let Some(profile) = ToolProfile::parse(name) else {
        eprintln!("Unknown tool profile '{name}'.");
        eprintln!("Available: lean (default), minimal, standard, power");
        std::process::exit(1);
    };

    let canonical = profile.as_str();

    if let Err(e) = tool_profiles::set_profile_in_config(canonical) {
        eprintln!("Error saving profile: {e}");
        std::process::exit(1);
    }

    let registry_count = crate::server::registry::tool_count();
    let count_str = match &profile {
        ToolProfile::Power => format!("{registry_count}"),
        other => format!("{}", other.tool_count()),
    };

    println!("Tool profile set to: {canonical}");
    println!("  Tools exposed: {count_str}");
    println!("  Description:   {}", profile.description());

    if !matches!(profile, ToolProfile::Power) {
        println!("\n  Enabled tools:");
        for name in profile.tool_names() {
            println!("    {name}");
        }
    }

    println!("\n  Restart your AI tool / IDE for changes to take effect.");
}

fn print_profile_help() {
    eprintln!(
        "lean-ctx has two kinds of profiles — here is which command to use:

TOOL PROFILES — how many MCP tools your agent sees:
  lean-ctx tools                Show current tool profile
  lean-ctx tools lean           Lazy core advertised, all via ctx_call (default)
  lean-ctx tools minimal        6 essential tools
  lean-ctx tools standard       22 balanced tools
  lean-ctx tools power          All tools (highest context overhead)
  lean-ctx tools list           List tool profiles with counts

CONTEXT PROFILES — how lean-ctx compresses and reads (this command):
  lean-ctx profile list         List available context profiles
  lean-ctx profile show [name]  Show context profile details (default: active)
  lean-ctx profile active       Show the currently active context profile
  lean-ctx profile diff <a> <b> Compare two context profiles side by side
  lean-ctx profile create <name> [--from <base>] [--global]
  lean-ctx profile set <name>   Show how to activate a context profile"
    );
}
