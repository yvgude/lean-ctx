use std::path::Path;

use crate::core::compressor;
use crate::core::deps as dep_extract;
use crate::core::entropy;
use crate::core::patterns::deps_cmd;
use crate::core::protocol;
use crate::core::signatures;
use crate::core::stats;
use crate::core::tokens::count_tokens;

use super::common::print_savings;

pub fn cmd_read(args: &[String]) {
    if args.is_empty() {
        eprintln!(
            "Usage: lean-ctx read <file> [--mode full|map|signatures|aggressive|entropy] [--fresh]"
        );
        std::process::exit(1);
    }

    let path = &args[0];
    let mode = args
        .iter()
        .position(|a| a == "--mode" || a == "-m")
        .and_then(|i| args.get(i + 1))
        .map_or("full", std::string::String::as_str);
    let force_fresh = args.iter().any(|a| a == "--fresh" || a == "--no-cache");

    let short = protocol::shorten_path(path);

    if !force_fresh && mode == "full" {
        use crate::core::cli_cache::{self, CacheResult};
        match cli_cache::check_and_read(path) {
            CacheResult::Hit { entry, file_ref } => {
                let msg = cli_cache::format_hit(&entry, &file_ref, &short);
                println!("{msg}");
                stats::record("cli_read", entry.original_tokens, count_tokens(&msg));
                return;
            }
            CacheResult::Miss { content } if content.is_empty() => {
                eprintln!("Error: could not read {path}");
                std::process::exit(1);
            }
            CacheResult::Miss { content } => {
                let line_count = content.lines().count();
                println!("{short} [{line_count}L]");
                println!("{content}");
                stats::record("cli_read", count_tokens(&content), count_tokens(&content));
                return;
            }
        }
    }

    let content = match crate::tools::ctx_read::read_file_lossy(path) {
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
    let line_count = content.lines().count();
    let original_tokens = count_tokens(&content);

    let mode = if mode == "auto" {
        let sig = crate::core::mode_predictor::FileSignature::from_path(path, original_tokens);
        let predictor = crate::core::mode_predictor::ModePredictor::new();
        predictor
            .predict_best_mode(&sig)
            .unwrap_or_else(|| "full".to_string())
    } else {
        mode.to_string()
    };
    let mode = mode.as_str();

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
            let key_sigs: Vec<_> = sigs
                .iter()
                .filter(|s| s.is_exported || s.indent == 0)
                .collect();
            if !key_sigs.is_empty() {
                println!("  API:");
                for sig in &key_sigs {
                    println!("    {}", sig.to_compact());
                }
            }
            let sent = count_tokens(&short.clone());
            print_savings(original_tokens, sent);
        }
        "signatures" => {
            let sigs = signatures::extract_signatures(&content, ext);
            println!("{short} [{line_count}L]");
            for sig in &sigs {
                println!("{}", sig.to_compact());
            }
            let sent = count_tokens(&short.clone());
            print_savings(original_tokens, sent);
        }
        "aggressive" => {
            let compressed = compressor::aggressive_compress(&content, Some(ext));
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

    let content1 = match crate::tools::ctx_read::read_file_lossy(&args[0]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {e}", args[0]);
            std::process::exit(1);
        }
    };

    let content2 = match crate::tools::ctx_read::read_file_lossy(&args[1]) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {e}", args[1]);
            std::process::exit(1);
        }
    };

    let diff = compressor::diff_content(&content1, &content2);
    let original = count_tokens(&content1) + count_tokens(&content2);
    let sent = count_tokens(&diff);

    println!(
        "diff {} {}",
        protocol::shorten_path(&args[0]),
        protocol::shorten_path(&args[1])
    );
    println!("{diff}");
    print_savings(original, sent);
}

pub fn cmd_grep(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: lean-ctx grep <pattern> [path]");
        std::process::exit(1);
    }

    let pattern = &args[0];
    let path = args.get(1).map_or(".", std::string::String::as_str);

    let re = match regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Invalid regex pattern: {e}");
            std::process::exit(1);
        }
    };

    let mut found = false;
    for entry in ignore::WalkBuilder::new(path)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .max_depth(Some(10))
        .build()
        .flatten()
    {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let file_path = entry.path();
        if let Ok(content) = std::fs::read_to_string(file_path) {
            for (i, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    println!("{}:{}:{}", file_path.display(), i + 1, line);
                    found = true;
                }
            }
        }
    }

    if !found {
        std::process::exit(1);
    }
}

pub fn cmd_find(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: lean-ctx find <pattern> [path]");
        std::process::exit(1);
    }

    let raw_pattern = &args[0];
    let path = args.get(1).map_or(".", std::string::String::as_str);

    let is_glob = raw_pattern.contains('*') || raw_pattern.contains('?');
    let glob_matcher = if is_glob {
        glob::Pattern::new(&raw_pattern.to_lowercase()).ok()
    } else {
        None
    };
    let substring = raw_pattern.to_lowercase();

    let mut found = false;
    for entry in ignore::WalkBuilder::new(path)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .max_depth(Some(10))
        .build()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let matches = if let Some(ref g) = glob_matcher {
            g.matches(&name)
        } else {
            name.contains(&substring)
        };
        if matches {
            println!("{}", entry.path().display());
            found = true;
        }
    }

    if !found {
        std::process::exit(1);
    }
}

pub fn cmd_ls(args: &[String]) {
    let path = args.first().map_or(".", std::string::String::as_str);
    let command = if cfg!(windows) {
        format!("dir {}", path.replace('/', "\\"))
    } else {
        format!("ls {path}")
    };
    let code = crate::shell::exec(&command);
    std::process::exit(code);
}

pub fn cmd_deps(args: &[String]) {
    let path = args.first().map_or(".", std::string::String::as_str);

    if let Some(result) = deps_cmd::detect_and_compress(path) {
        println!("{result}");
    } else {
        eprintln!("No dependency file found in {path}");
        std::process::exit(1);
    }
}
