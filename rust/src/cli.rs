use std::path::Path;

use crate::core::compressor;
use crate::core::deps as dep_extract;
use crate::core::patterns::deps_cmd;
use crate::core::signatures;
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

        let aliases = format!(
            "\n# lean-ctx shell hook\nalias git 'lean-ctx -c git'\nalias npm 'lean-ctx -c npm'\nalias cargo 'lean-ctx -c cargo'\nalias docker 'lean-ctx -c docker'\nalias ls 'lean-ctx -c ls'\nalias find 'lean-ctx -c find'\nalias grep 'lean-ctx -c grep'\nalias curl 'lean-ctx -c curl'\n"
        );

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

        let aliases = format!(
            r#"
# lean-ctx shell hook — transparent CLI compression
alias git='lean-ctx -c git'
alias npm='lean-ctx -c npm'
alias cargo='lean-ctx -c cargo'
alias docker='lean-ctx -c docker'
alias ls='lean-ctx -c ls'
alias find='lean-ctx -c find'
alias grep='lean-ctx -c grep'
alias curl='lean-ctx -c curl'
"#
        );

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

    println!("\nlean-ctx init complete.");
    println!("Binary: {binary}");
    println!("\nRun 'lean-ctx gain' after using some commands to see your savings.");
}

fn print_savings(original: usize, sent: usize) {
    let saved = original.saturating_sub(sent);
    if original > 0 && saved > 0 {
        let pct = (saved as f64 / original as f64 * 100.0).round() as usize;
        println!("[{saved} tok saved ({pct}%)]");
    }
}
