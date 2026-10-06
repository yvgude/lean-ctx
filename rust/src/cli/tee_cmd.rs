pub fn cmd_tee(args: &[String]) {
    let tee_dir = match crate::core::paths::state_dir() {
        Ok(d) => d.join("tee"),
        Err(e) => {
            eprintln!("Cannot determine state directory: {e}");
            std::process::exit(1);
        }
    };

    let action = args.first().map_or("list", std::string::String::as_str);
    if !matches!(action, "clear" | "purge") {
        match crate::core::policy::runtime::with_source_view(
            crate::core::policy::runtime::is_active,
        ) {
            Ok(false) => {}
            Ok(true) => {
                eprintln!(
                    "Stored shell output has no source authority; repeat the original authorized operation."
                );
                std::process::exit(1);
            }
            Err(_) => {
                eprintln!("Current policy cannot be verified; retry the authorized operation.");
                std::process::exit(1);
            }
        }
    }
    match action {
        "list" | "ls" => {
            // Filenames can contain command text. Build the entire listing
            // under one view and publish only after its authority recheck.
            let listing = crate::core::policy::runtime::with_source_view(|| {
                if crate::core::policy::runtime::is_active() {
                    return Err("Stored shell output has no source authority.".to_string());
                }
                if !tee_dir.exists() {
                    return Ok("No tee logs found (~/.lean-ctx/tee/ does not exist)\n".to_string());
                }
                let mut entries: Vec<_> = std::fs::read_dir(&tee_dir)
                    .map_err(|_| "Tee metadata is unavailable".to_string())?
                    .filter_map(std::result::Result::ok)
                    .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("log"))
                    .collect();
                entries.sort_by_key(std::fs::DirEntry::file_name);

                if entries.is_empty() {
                    return Ok("No tee logs found.\n".to_string());
                }

                let mut output = format!("Tee logs ({}):\n\n", entries.len());
                for entry in &entries {
                    let size = entry.metadata().map_or(0, |m| m.len());
                    let name = entry.file_name();
                    let size_str = if size > 1024 {
                        format!("{}K", size / 1024)
                    } else {
                        format!("{size}B")
                    };
                    output.push_str(&format!("  {:<60} {}\n", name.to_string_lossy(), size_str));
                }
                output.push_str("\nUse 'lean-ctx tee clear' to delete all logs.\n");
                Ok(output)
            })
            .and_then(std::convert::identity);
            if let Ok(output) = listing {
                print!("{output}");
            } else {
                eprintln!("Tee metadata is unavailable or current policy cannot be verified.");
                std::process::exit(1);
            }
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
            let fname = filename.as_str();
            let basename = std::path::Path::new(fname).file_name().unwrap_or_default();
            if basename.is_empty()
                || basename != fname
                || fname == "."
                || fname == ".."
                || fname.contains(std::path::MAIN_SEPARATOR)
            {
                eprintln!("Error: filename must be a plain basename (no path separators or '..')");
                std::process::exit(1);
            }
            let path = tee_dir.join(basename);
            match crate::proxy::ccr::read_tee_detailed(&path) {
                Ok(content) => print!("{content}"),
                Err(reason) => {
                    eprintln!("{reason}");
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
                    match crate::proxy::ccr::read_tee_detailed(&path) {
                        Ok(content) => println!(
                            "--- {} ---\n\n{content}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ),
                        Err(reason) => {
                            eprintln!("{reason}");
                            std::process::exit(1);
                        }
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
