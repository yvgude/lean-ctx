use crate::core::profiles;

pub fn cmd_profile(args: &[String]) {
    let action = args.first().map_or("list", String::as_str);

    match action {
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
    cmp!(budget.max_context_tokens);
    cmp!(budget.max_shell_invocations);
    cmp!(budget.max_cost_usd);
    cmp!(pipeline.intent);
    cmp!(pipeline.relevance);
    cmp!(pipeline.compression);
    cmp!(pipeline.translation);
    cmp!(autonomy.auto_dedup);
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
    println!("Or add it to your shell config (~/.zshrc, ~/.bashrc).");
}

fn print_profile_help() {
    eprintln!(
        "Usage: lean-ctx profile <command>

Commands:
  list              List available profiles
  show [name]       Show profile details (default: active)
  active            Show the currently active profile
  diff <a> <b>      Compare two profiles side by side
  create <name>     Create a new profile file
    --from <base>   Base on an existing profile
    --global        Create in global dir instead of project
  set <name>        Show how to activate a profile"
    );
}
