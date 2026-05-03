pub fn cmd_tee(args: &[String]) {
    let tee_dir = if let Some(h) = dirs::home_dir() {
        h.join(".lean-ctx").join("tee")
    } else {
        eprintln!("Cannot determine home directory");
        std::process::exit(1);
    };

    let action = args.first().map_or("list", std::string::String::as_str);
    match action {
        "list" | "ls" => {
            if !tee_dir.exists() {
                println!("No tee logs found (~/.lean-ctx/tee/ does not exist)");
                return;
            }
            let mut entries: Vec<_> = std::fs::read_dir(&tee_dir)
                .unwrap_or_else(|e| {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                })
                .filter_map(std::result::Result::ok)
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("log"))
                .collect();
            entries.sort_by_key(std::fs::DirEntry::file_name);

            if entries.is_empty() {
                println!("No tee logs found.");
                return;
            }

            println!("Tee logs ({}):\n", entries.len());
            for entry in &entries {
                let size = entry.metadata().map_or(0, |m| m.len());
                let name = entry.file_name();
                let size_str = if size > 1024 {
                    format!("{}K", size / 1024)
                } else {
                    format!("{size}B")
                };
                println!("  {:<60} {}", name.to_string_lossy(), size_str);
            }
            println!("\nUse 'lean-ctx tee clear' to delete all logs.");
        }
        "clear" | "purge" => {
            if !tee_dir.exists() {
                println!("No tee logs to clear.");
                return;
            }
            let mut count = 0u32;
            if let Ok(entries) = std::fs::read_dir(&tee_dir) {
                for entry in entries.flatten() {
                    if entry.path().extension().and_then(|x| x.to_str()) == Some("log")
                        && std::fs::remove_file(entry.path()).is_ok()
                    {
                        count += 1;
                    }
                }
            }
            println!("Cleared {count} tee log(s) from {}", tee_dir.display());
        }
        "show" => {
            let Some(filename) = args.get(1) else {
                eprintln!("Usage: lean-ctx tee show <filename>");
                std::process::exit(1);
            };
            let path = tee_dir.join(filename);
            match crate::tools::ctx_read::read_file_lossy(&path.to_string_lossy()) {
                Ok(content) => print!("{content}"),
                Err(e) => {
                    eprintln!("Error reading {}: {e}", path.display());
                    std::process::exit(1);
                }
            }
        }
        "last" => {
            if !tee_dir.exists() {
                println!("No tee logs found.");
                return;
            }
            let mut entries: Vec<_> = std::fs::read_dir(&tee_dir)
                .ok()
                .into_iter()
                .flat_map(|d| d.filter_map(std::result::Result::ok))
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("log"))
                .collect();
            entries.sort_by_key(|e| {
                e.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            });
            match entries.last() {
                Some(entry) => {
                    let path = entry.path();
                    println!(
                        "--- {} ---\n",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                    match crate::tools::ctx_read::read_file_lossy(&path.to_string_lossy()) {
                        Ok(content) => print!("{content}"),
                        Err(e) => eprintln!("Error: {e}"),
                    }
                }
                None => println!("No tee logs found."),
            }
        }
        _ => {
            eprintln!("Usage: lean-ctx tee [list|clear|show <file>|last]");
            std::process::exit(1);
        }
    }
}

pub fn cmd_filter(args: &[String]) {
    let action = args.first().map_or("list", std::string::String::as_str);
    match action {
        "list" | "ls" => {
            if let Some(engine) = crate::core::filters::FilterEngine::load() {
                let rules = engine.list_rules();
                println!("Loaded {} filter rule(s):\n", rules.len());
                for rule in &rules {
                    println!("{rule}");
                }
            } else {
                println!("No custom filters found.");
                println!("Create one: lean-ctx filter init");
            }
        }
        "validate" => {
            let Some(path) = args.get(1) else {
                eprintln!("Usage: lean-ctx filter validate <file.toml>");
                std::process::exit(1);
            };
            match crate::core::filters::validate_filter_file(path) {
                Ok(count) => println!("Valid: {count} rule(s) parsed successfully."),
                Err(e) => {
                    eprintln!("Validation failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "init" => match crate::core::filters::create_example_filter() {
            Ok(path) => {
                println!("Created example filter: {path}");
                println!("Edit it to add your custom compression rules.");
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        },
        _ => {
            eprintln!("Usage: lean-ctx filter [list|validate <file>|init]");
            std::process::exit(1);
        }
    }
}

pub fn cmd_slow_log(args: &[String]) {
    use crate::core::slow_log;

    let action = args.first().map_or("list", std::string::String::as_str);
    match action {
        "list" | "ls" | "" => println!("{}", slow_log::list()),
        "clear" | "purge" => println!("{}", slow_log::clear()),
        _ => {
            eprintln!("Usage: lean-ctx slow-log [list|clear]");
            std::process::exit(1);
        }
    }
}
