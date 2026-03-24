use std::path::Path;

use crate::core::compressor;
use crate::core::config;
use crate::core::deps as dep_extract;
use crate::core::patterns::deps_cmd;
use crate::core::signatures;
use crate::core::stats;
use crate::core::tokens::count_tokens;
use crate::core::protocol;
use crate::core::entropy;

pub fn cmd_read(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: lean-ctx read <file> [--mode full|map|signatures|aggressive|entropy]");
        std::process::exit(1);
    }

    let path = &args[0];
    let mode = args
        .iter()
        .position(|a| a == "--mode" || a == "-m")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("full");

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let short = protocol::shorten_path(path);
    let line_count = content.lines().count();
    let original_tokens = count_tokens(&content);

    match mode {
        "map" => {
            let sigs = signatures::extract_signatures(&content, ext);
            let dep_info = dep_extract::extract_deps(&content, ext);

            println!("{short} [{line_count}L]");
            if !dep_info.imports.is_empty() {
                println!("  deps: {}", dep_info.imports.join(", "));
            }
            if !dep_info.exports.is_empty() {
                println!("  exports: {}", dep_info.exports.join(", "));
            }
            let key_sigs: Vec<_> = sigs.iter().filter(|s| s.is_exported || s.indent == 0).collect();
            if !key_sigs.is_empty() {
                println!("  API:");
                for sig in &key_sigs {
                    println!("    {}", sig.to_compact());
                }
            }
            let sent = count_tokens(&format!("{short}"));
            print_savings(original_tokens, sent);
        }
        "signatures" => {
            let sigs = signatures::extract_signatures(&content, ext);
            println!("{short} [{line_count}L]");
            for sig in &sigs {
                println!("{}", sig.to_compact());
            }
            let sent = count_tokens(&format!("{short}"));
            print_savings(original_tokens, sent);
        }
        "aggressive" => {
            let compressed = compressor::aggressive_compress(&content);
            println!("{short} [{line_count}L]");
            println!("{compressed}");
            let sent = count_tokens(&compressed);
            print_savings(original_tokens, sent);
        }
        "entropy" => {
            let result = entropy::entropy_compress(&content);
            let avg_h = entropy::analyze_entropy(&content).avg_entropy;
            println!("{short} [{line_count}L] (H̄={avg_h:.1})");
            for tech in &result.techniques {
                println!("{tech}");
            }
            println!("{}", result.output);
            let sent = count_tokens(&result.output);
            print_savings(original_tokens, sent);
        }
        _ => {
            println!("{short} [{line_count}L]");
            println!("{content}");
        }
    }
}

pub fn cmd_diff(args: &[String]) {
    if args.len() < 2 {
        eprintln!("Usage: lean-ctx diff <file1> <file2>");
        std::process::exit(1);
    }

    let content1 = match std::fs::read_to_string(&args[0]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {e}", args[0]);
            std::process::exit(1);
        }
    };

    let content2 = match std::fs::read_to_string(&args[1]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {e}", args[1]);
            std::process::exit(1);
        }
    };

    let diff = compressor::diff_content(&content1, &content2);
    let original = count_tokens(&content1) + count_tokens(&content2);
    let sent = count_tokens(&diff);

    println!("diff {} {}", protocol::shorten_path(&args[0]), protocol::shorten_path(&args[1]));
    println!("{diff}");
    print_savings(original, sent);
}

pub fn cmd_grep(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: lean-ctx grep <pattern> [path]");
        std::process::exit(1);
    }

    let pattern = &args[0];
    let path = args.get(1).map(|s| s.as_str()).unwrap_or(".");

    let command = format!("grep -rn '{}' {}", pattern.replace('\'', "'\\''"), path);
    let code = crate::shell::exec(&command);
    std::process::exit(code);
}

pub fn cmd_find(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: lean-ctx find <pattern> [path]");
        std::process::exit(1);
    }

    let pattern = &args[0];
    let path = args.get(1).map(|s| s.as_str()).unwrap_or(".");
    let command = format!("find {path} -name \"{pattern}\" -not -path '*/node_modules/*' -not -path '*/.git/*' -not -path '*/target/*'");
    let code = crate::shell::exec(&command);
    std::process::exit(code);
}

pub fn cmd_ls(args: &[String]) {
    let path = args.first().map(|s| s.as_str()).unwrap_or(".");
    let command = format!("ls -la {path}");
    let code = crate::shell::exec(&command);
    std::process::exit(code);
}

pub fn cmd_deps(args: &[String]) {
    let path = args.first().map(|s| s.as_str()).unwrap_or(".");

    match deps_cmd::detect_and_compress(path) {
        Some(result) => println!("{result}"),
        None => {
            eprintln!("No dependency file found in {path}");
            std::process::exit(1);
        }
    }
}

pub fn cmd_discover(_args: &[String]) {
    let history = load_shell_history();
    if history.is_empty() {
        println!("No shell history found.");
        return;
    }

    let compressible_commands = [
        "git ", "npm ", "yarn ", "pnpm ", "cargo ", "docker ",
        "kubectl ", "gh ", "pip ", "pip3 ", "eslint", "prettier",
        "ruff ", "go ", "golangci-lint", "playwright", "cypress",
        "next ", "vite ", "tsc", "curl ", "wget ", "grep ", "rg ",
        "find ", "env", "ls ",
    ];

    let mut missed: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut total_compressible = 0u32;
    let mut via_lean_ctx = 0u32;

    for line in &history {
        let cmd = line.trim().to_lowercase();
        if cmd.starts_with("lean-ctx") {
            via_lean_ctx += 1;
            continue;
        }
        for pattern in &compressible_commands {
            if cmd.starts_with(pattern) {
                total_compressible += 1;
                let key = cmd.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
                *missed.entry(key).or_insert(0) += 1;
                break;
            }
        }
    }

    if missed.is_empty() {
        println!("All compressible commands are already using lean-ctx!");
        return;
    }

    let mut sorted: Vec<(String, u32)> = missed.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    println!("Found {} compressible commands not using lean-ctx:\n", total_compressible);
    for (cmd, count) in sorted.iter().take(15) {
        let est_savings = count * 150;
        println!("  {cmd:<30} (used {count}x, ~{est_savings} tokens saveable)");
    }
    if sorted.len() > 15 {
        println!("  ... +{} more command types", sorted.len() - 15);
    }

    let total_est = total_compressible * 150;
    println!("\nEstimated missed savings: ~{total_est} tokens");
    println!("Already using lean-ctx: {via_lean_ctx} commands");
    println!("\nRun 'lean-ctx init --global' to enable compression for all commands.");
}

pub fn cmd_session() {
    let history = load_shell_history();
    let gain = stats::load_stats();

    let compressible_commands = [
        "git ", "npm ", "yarn ", "pnpm ", "cargo ", "docker ",
        "kubectl ", "gh ", "pip ", "pip3 ", "eslint", "prettier",
        "ruff ", "go ", "golangci-lint", "curl ", "wget ", "grep ",
        "rg ", "find ", "ls ",
    ];

    let mut total = 0u32;
    let mut via_hook = 0u32;

    for line in &history {
        let cmd = line.trim().to_lowercase();
        if cmd.starts_with("lean-ctx") {
            via_hook += 1;
            total += 1;
        } else {
            for p in &compressible_commands {
                if cmd.starts_with(p) {
                    total += 1;
                    break;
                }
            }
        }
    }

    let pct = if total > 0 {
        (via_hook as f64 / total as f64 * 100.0).round() as u32
    } else {
        0
    };

    println!("lean-ctx session statistics\n");
    println!("Adoption:    {}% ({}/{} compressible commands)", pct, via_hook, total);
    println!("Saved:       {} tokens total", gain.total_saved);
    println!("Calls:       {} compressed", gain.total_calls);

    if total > via_hook {
        let missed = total - via_hook;
        let est = missed * 150;
        println!("Missed:      {} commands (~{} tokens saveable)", missed, est);
    }

    println!("\nRun 'lean-ctx discover' for details on missed commands.");
}

pub fn cmd_config(args: &[String]) {
    let cfg = config::Config::load();

    if args.is_empty() {
        println!("{}", cfg.show());
        return;
    }

    match args[0].as_str() {
        "init" | "create" => {
            let default = config::Config::default();
            match default.save() {
                Ok(()) => {
                    let path = config::Config::path()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| "~/.lean-ctx/config.toml".to_string());
                    println!("Created default config at {path}");
                }
                Err(e) => eprintln!("Error: {e}"),
            }
        }
        "set" => {
            if args.len() < 3 {
                eprintln!("Usage: lean-ctx config set <key> <value>");
                std::process::exit(1);
            }
            let mut cfg = cfg;
            let key = &args[1];
            let val = &args[2];
            match key.as_str() {
                "ultra_compact" => cfg.ultra_compact = val == "true",
                "tee_on_error" => cfg.tee_on_error = val == "true",
                "checkpoint_interval" => {
                    cfg.checkpoint_interval = val.parse().unwrap_or(15);
                }
                _ => {
                    eprintln!("Unknown config key: {key}");
                    std::process::exit(1);
                }
            }
            match cfg.save() {
                Ok(()) => println!("Updated {key} = {val}"),
                Err(e) => eprintln!("Error saving config: {e}"),
            }
        }
        _ => {
            eprintln!("Usage: lean-ctx config [init|set <key> <value>]");
            std::process::exit(1);
        }
    }
}

pub fn cmd_init(args: &[String]) {
    let global = args.iter().any(|a| a == "--global" || a == "-g");

    let shell_name = std::env::var("SHELL").unwrap_or_default();
    let is_zsh = shell_name.contains("zsh");
    let is_fish = shell_name.contains("fish");

    let binary = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "lean-ctx".to_string());

    if is_fish {
        let config = dirs::home_dir()
            .map(|h| h.join(".config/fish/config.fish"))
            .unwrap_or_default();

        let aliases = "\n# lean-ctx shell hook — transparent CLI compression (60+ patterns)\n\
            if not set -q LEAN_CTX_ACTIVE\n\
            \talias git 'lean-ctx -c git'\n\
            \talias npm 'lean-ctx -c npm'\n\
            \talias pnpm 'lean-ctx -c pnpm'\n\
            \talias yarn 'lean-ctx -c yarn'\n\
            \talias cargo 'lean-ctx -c cargo'\n\
            \talias docker 'lean-ctx -c docker'\n\
            \talias docker-compose 'lean-ctx -c docker-compose'\n\
            \talias kubectl 'lean-ctx -c kubectl'\n\
            \talias k 'lean-ctx -c kubectl'\n\
            \talias gh 'lean-ctx -c gh'\n\
            \talias pip 'lean-ctx -c pip'\n\
            \talias pip3 'lean-ctx -c pip3'\n\
            \talias ruff 'lean-ctx -c ruff'\n\
            \talias go 'lean-ctx -c go'\n\
            \talias golangci-lint 'lean-ctx -c golangci-lint'\n\
            \talias eslint 'lean-ctx -c eslint'\n\
            \talias prettier 'lean-ctx -c prettier'\n\
            \talias tsc 'lean-ctx -c tsc'\n\
            \talias ls 'lean-ctx -c ls'\n\
            \talias find 'lean-ctx -c find'\n\
            \talias grep 'lean-ctx -c grep'\n\
            \talias curl 'lean-ctx -c curl'\n\
            \talias wget 'lean-ctx -c wget'\n\
            end\n";

        if let Ok(existing) = std::fs::read_to_string(&config) {
            if existing.contains("lean-ctx") {
                println!("lean-ctx already configured in {}", config.display());
                return;
            }
        }

        match std::fs::OpenOptions::new().append(true).create(true).open(&config) {
            Ok(mut f) => {
                use std::io::Write;
                let _ = f.write_all(aliases.as_bytes());
                println!("Added lean-ctx aliases to {}", config.display());
            }
            Err(e) => eprintln!("Error writing {}: {e}", config.display()),
        }
    } else {
        let rc_file = if is_zsh {
            dirs::home_dir().map(|h| h.join(".zshrc")).unwrap_or_default()
        } else {
            dirs::home_dir().map(|h| h.join(".bashrc")).unwrap_or_default()
        };

        let aliases = r#"
# lean-ctx shell hook — transparent CLI compression (60+ patterns)
if [ -z "$LEAN_CTX_ACTIVE" ]; then
alias git='lean-ctx -c git'
alias npm='lean-ctx -c npm'
alias pnpm='lean-ctx -c pnpm'
alias yarn='lean-ctx -c yarn'
alias cargo='lean-ctx -c cargo'
alias docker='lean-ctx -c docker'
alias docker-compose='lean-ctx -c docker-compose'
alias kubectl='lean-ctx -c kubectl'
alias k='lean-ctx -c kubectl'
alias gh='lean-ctx -c gh'
alias pip='lean-ctx -c pip'
alias pip3='lean-ctx -c pip3'
alias ruff='lean-ctx -c ruff'
alias go='lean-ctx -c go'
alias golangci-lint='lean-ctx -c golangci-lint'
alias eslint='lean-ctx -c eslint'
alias prettier='lean-ctx -c prettier'
alias tsc='lean-ctx -c tsc'
alias ls='lean-ctx -c ls'
alias find='lean-ctx -c find'
alias grep='lean-ctx -c grep'
alias curl='lean-ctx -c curl'
alias wget='lean-ctx -c wget'
fi
"#;

        if let Ok(existing) = std::fs::read_to_string(&rc_file) {
            if existing.contains("lean-ctx shell hook") {
                println!("lean-ctx already configured in {}", rc_file.display());
                return;
            }
        }

        match std::fs::OpenOptions::new().append(true).create(true).open(&rc_file) {
            Ok(mut f) => {
                use std::io::Write;
                let _ = f.write_all(aliases.as_bytes());
                println!("Added lean-ctx aliases to {}", rc_file.display());
            }
            Err(e) => eprintln!("Error writing {}: {e}", rc_file.display()),
        }
    }

    let lean_dir = dirs::home_dir().map(|h| h.join(".lean-ctx"));
    if let Some(dir) = lean_dir {
        if !dir.exists() {
            let _ = std::fs::create_dir_all(&dir);
            println!("Created {}", dir.display());
        }
    }

    if global {
        println!("\nRestart your shell or run: source ~/{}", if is_zsh { ".zshrc" } else { ".bashrc" });
    }

    println!("\nlean-ctx init complete. (23 aliases installed)");
    println!("Binary: {binary}");
    println!("\nRun 'lean-ctx gain' after using some commands to see your savings.");
    println!("Run 'lean-ctx discover' to find missed savings in your shell history.");
}

fn load_shell_history() -> Vec<String> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };

    let history_file = if shell.contains("zsh") {
        home.join(".zsh_history")
    } else if shell.contains("fish") {
        home.join(".local/share/fish/fish_history")
    } else {
        home.join(".bash_history")
    };

    match std::fs::read_to_string(&history_file) {
        Ok(content) => {
            content
                .lines()
                .filter_map(|l| {
                    let trimmed = l.trim();
                    if trimmed.starts_with(':') {
                        trimmed.split(';').nth(1).map(|s| s.to_string())
                    } else {
                        Some(trimmed.to_string())
                    }
                })
                .filter(|l| !l.is_empty())
                .collect()
        }
        Err(_) => Vec::new(),
    }
}

fn print_savings(original: usize, sent: usize) {
    let saved = original.saturating_sub(sent);
    if original > 0 && saved > 0 {
        let pct = (saved as f64 / original as f64 * 100.0).round() as usize;
        println!("[{saved} tok saved ({pct}%)]");
    }
}
