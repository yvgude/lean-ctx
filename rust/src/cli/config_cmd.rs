use crate::core::config;
use crate::core::theme;

pub fn cmd_config(args: &[String]) {
    let cfg = config::Config::load();

    if args.is_empty() {
        println!("{}", cfg.show());
        println!(
            "\nTip: this is the full config. For the few knobs most people touch, run\n     `lean-ctx config show` (high-level summary), or change one with\n     `lean-ctx config set <key> <value>`."
        );
        return;
    }

    match args[0].as_str() {
        "init" | "create" => {
            let full = args.iter().any(|a| a == "--full");
            if full {
                let default = config::Config::default();
                match default.save() {
                    Ok(()) => {
                        let path = config::Config::path().map_or_else(
                            || "~/.lean-ctx/config.toml".to_string(),
                            |p| p.to_string_lossy().to_string(),
                        );
                        println!("Created full config at {path}");
                    }
                    Err(e) => eprintln!("Error: {e}"),
                }
            } else {
                match write_simplified_config() {
                    Ok(path) => println!("Created simplified config at {path}"),
                    Err(e) => eprintln!("Error: {e}"),
                }
            }
        }
        "set" => {
            if args.len() < 3 {
                eprintln!("Usage: lean-ctx config set <key> <value>");
                std::process::exit(1);
            }
            let key = &args[1];
            let val = &args[2];

            // Special validation hooks for keys that need custom logic
            // beyond what the schema type system can express.
            match key.as_str() {
                "theme" if theme::from_preset(val).is_none() && val != "custom" => {
                    eprintln!(
                        "Unknown theme '{val}'. Available: {}",
                        theme::PRESET_NAMES.join(", ")
                    );
                    std::process::exit(1);
                }
                "tee_on_error" | "tee_mode" => {
                    let normalized = match val.as_str() {
                        "true" => "failures",
                        "false" => "never",
                        other => other,
                    };
                    match config::setter::set_by_key("tee_mode", normalized) {
                        Ok(_) => println!("Updated {key} = {val}"),
                        Err(e) => {
                            eprintln!("{e}");
                            std::process::exit(1);
                        }
                    }
                    return;
                }
                "project_root" => {
                    let path = std::path::Path::new(val.as_str());
                    if !path.exists() || !path.is_dir() {
                        eprintln!("Error: '{val}' is not an existing directory.");
                        std::process::exit(1);
                    }
                }
                "embedding.model"
                    if crate::core::embeddings::model_registry::EmbeddingModel::from_str_name(
                        val,
                    )
                    .is_none() =>
                {
                    eprintln!(
                        "Unknown embedding model '{val}'. Available: minilm (default), jina-code-v2, nomic."
                    );
                    std::process::exit(1);
                }
                "proxy.anthropic_upstream" | "proxy.openai_upstream" | "proxy.gemini_upstream" => {
                    let normalized = normalize_optional_upstream(val);
                    let effective = normalized.as_deref().unwrap_or("");
                    match config::setter::set_by_key(key, effective) {
                        Ok(_) => println!("Updated {key} = {val}"),
                        Err(e) => {
                            eprintln!("{e}");
                            std::process::exit(1);
                        }
                    }
                    return;
                }
                _ => {}
            }

            // Generic schema-based setter handles all keys
            match config::setter::set_by_key(key, val) {
                Ok(_) => println!("Updated {key} = {val}"),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        "schema" => {
            let schema = config::schema::ConfigSchema::generate();
            println!(
                "{}",
                serde_json::to_string_pretty(&schema).unwrap_or_else(|_| "{}".to_string())
            );
        }
        "validate" => {
            cmd_validate();
        }
        "show" | "effective" => {
            cmd_show_effective();
        }
        "apply" | "reload" => {
            cmd_apply();
        }
        _ => {
            eprintln!("Usage: lean-ctx config [init|set|show|schema|validate|apply]");
            std::process::exit(1);
        }
    }
}

fn cmd_apply() {
    use crate::daemon;
    use crate::ipc;

    println!("Applying config changes…");

    // 1. Validate config first
    println!("\n[1/4] Validating config…");
    let schema = config::schema::ConfigSchema::generate();
    let known = schema.known_keys();
    let cfg = config::Config::load();

    if let Some(path) = config::Config::path() {
        if path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(table) = raw.parse::<toml::Table>() {
                    let mut user_keys = Vec::new();
                    fn collect_flat(table: &toml::Table, prefix: &str, out: &mut Vec<String>) {
                        for (k, v) in table {
                            let full = if prefix.is_empty() {
                                k.clone()
                            } else {
                                format!("{prefix}.{k}")
                            };
                            if let toml::Value::Table(sub) = v {
                                collect_flat(sub, &full, out);
                            } else {
                                out.push(full);
                            }
                        }
                    }
                    collect_flat(&table, "", &mut user_keys);
                    let warnings: Vec<_> = user_keys
                        .iter()
                        .filter(|uk| {
                            !known.contains(uk)
                                && !known.iter().any(|k| uk.starts_with(&format!("{k}.")))
                        })
                        .collect();
                    if warnings.is_empty() {
                        println!("  ✓ All config keys valid.");
                    } else {
                        for w in &warnings {
                            eprintln!("  [WARN] Unknown key: {w}");
                        }
                        eprintln!(
                            "  {} unknown key(s) found. Continuing anyway…",
                            warnings.len()
                        );
                    }
                }
            }
        }
    }

    // 2. Restart processes
    println!("\n[2/4] Restarting processes…");
    crate::proxy_autostart::stop();

    if let Err(e) = daemon::stop_daemon() {
        eprintln!("  Warning: daemon stop: {e}");
    }

    let orphans = ipc::process::kill_all_by_name("lean-ctx");
    if orphans > 0 {
        println!("  Terminated {orphans} orphan process(es).");
    }

    std::thread::sleep(std::time::Duration::from_millis(500));

    let remaining = ipc::process::find_pids_by_name("lean-ctx");
    if !remaining.is_empty() {
        for &pid in &remaining {
            let _ = ipc::process::force_kill(pid);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    daemon::cleanup_daemon_files();
    crate::proxy_autostart::start();

    match daemon::start_daemon(&[]) {
        Ok(()) => println!("  ✓ Daemon restarted."),
        Err(e) => {
            eprintln!("  ✗ Daemon start failed: {e}");
            std::process::exit(1);
        }
    }

    // 3. Safety checks
    println!("\n[3/4] Running safety checks…");
    println!("  RAM guard: max {}% system", cfg.max_ram_percent);

    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        let sessions_dir = data_dir.join("sessions");
        let session_count = std::fs::read_dir(&sessions_dir)
            .map_or(0, |rd| rd.filter_map(std::result::Result::ok).count());
        println!("  Sessions dir: {session_count} files");
    }

    // 4. Summary
    println!("\n[4/4] Config applied successfully.");
    println!("  Theme:       {}", cfg.theme);
    println!("  Ultra compact: {}", cfg.ultra_compact);
    println!("  Checkpoint:  every {} calls", cfg.checkpoint_interval);
    if let Some(ref root) = cfg.project_root {
        println!("  Project root: {root}");
    }
}

fn cmd_validate() {
    let schema = config::schema::ConfigSchema::generate();
    let known = schema.known_keys();

    let path = match config::Config::path() {
        Some(p) if p.exists() => p,
        _ => {
            println!("[OK] No config.toml found — using defaults.");
            return;
        }
    };

    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[ERROR] Cannot read {}: {e}", path.display());
            std::process::exit(1);
        }
    };

    let table: toml::Table = match raw.parse() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[ERROR] Invalid TOML: {e}");
            std::process::exit(1);
        }
    };

    let mut warnings = 0u32;
    let mut validated = 0u32;

    fn collect_keys(table: &toml::Table, prefix: &str, out: &mut Vec<String>) {
        for (k, v) in table {
            let full = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            match v {
                toml::Value::Table(sub) => collect_keys(sub, &full, out),
                toml::Value::Array(arr) => {
                    out.push(full.clone());
                    for item in arr {
                        if let toml::Value::Table(sub) = item {
                            for sk in sub.keys() {
                                out.push(format!("{full}[].{sk}"));
                            }
                        }
                    }
                }
                _ => out.push(full),
            }
        }
    }

    let mut user_keys = Vec::new();
    collect_keys(&table, "", &mut user_keys);

    for uk in &user_keys {
        let base = uk.split("[].").next().unwrap_or(uk);
        let field = uk.rsplit("[].").next().unwrap_or("");
        let check_key = if uk.contains("[].") {
            format!("{base}.{field}")
        } else {
            uk.clone()
        };

        if known.contains(&check_key)
            || known
                .iter()
                .any(|k| check_key.starts_with(&format!("{k}.")))
        {
            validated += 1;
        } else {
            warnings += 1;
            let suggestion = find_closest(&check_key, &known);
            if let Some(sug) = suggestion {
                eprintln!("[WARN] Unknown key '{uk}' -- did you mean '{sug}'?");
            } else {
                eprintln!("[WARN] Unknown key '{uk}' -- this field does not exist");
            }
        }
    }

    let cfg = config::Config::load();
    let budget = cfg.max_disk_mb_effective();
    if budget > 0 {
        let explicit_archive = cfg.archive.max_disk_mb;
        let explicit_bm25 = cfg.bm25_max_cache_mb;
        let sum = explicit_archive + explicit_bm25;
        if sum > budget {
            warnings += 1;
            println!(
                "  ⚠ max_disk_mb={budget} but archive.max_disk_mb({explicit_archive}) + bm25_max_cache_mb({explicit_bm25}) = {sum} exceeds budget"
            );
        }
    }

    let total = validated + warnings;
    if warnings == 0 {
        println!(
            "[OK] All {total} keys validated successfully ({}).",
            path.display()
        );
    } else {
        println!(
            "[RESULT] {validated} of {total} keys validated, {warnings} unknown ({}).",
            path.display()
        );
        std::process::exit(1);
    }
}

fn find_closest(needle: &str, haystack: &[String]) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for candidate in haystack {
        let d = levenshtein(needle, candidate);
        if d <= 3 && (best.is_none() || d < best.unwrap().0) {
            best = Some((d, candidate));
        }
    }
    if best.is_some() {
        return best.map(|(_, s)| s.to_string());
    }
    let leaf = needle.rsplit('.').next().unwrap_or(needle);
    let mut leaf_best: Option<(usize, &str)> = None;
    for candidate in haystack {
        let cand_leaf = candidate.rsplit('.').next().unwrap_or(candidate);
        let d = levenshtein(leaf, cand_leaf);
        if d <= 2 && (leaf_best.is_none() || d < leaf_best.unwrap().0) {
            leaf_best = Some((d, candidate));
        }
    }
    leaf_best.map(|(_, s)| s.to_string())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for (i, row) in dp.iter_mut().enumerate().take(m + 1) {
        row[0] = i;
    }
    for (j, val) in dp[0].iter_mut().enumerate().take(n + 1) {
        *val = j;
    }
    for i in 1..=m {
        for j in 1..=n {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[m][n]
}

fn normalize_optional_upstream(value: &str) -> Option<String> {
    use crate::core::config::normalize_url_opt;
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
        None
    } else {
        normalize_url_opt(trimmed)
    }
}

pub fn cmd_benchmark(args: &[String]) {
    use crate::core::benchmark;
    use crate::core::benchmark_compare;

    let action = args.first().map_or("run", std::string::String::as_str);

    match action {
        "--help" | "-h" => {
            println!("Usage: lean-ctx benchmark run [path] [--json]");
            println!("       lean-ctx benchmark report [path]");
            println!("       lean-ctx benchmark eval [path] [--json]");
            println!("       lean-ctx benchmark compare [--repo path] [--output file.md]");
            println!("       lean-ctx benchmark scorecard [--json] [--output file]");
        }
        "scorecard" => {
            let is_json = args.iter().any(|a| a == "--json");
            let output = parse_flag_value(args, "--output");
            match crate::core::scorecard::run_scorecard() {
                Ok(sc) => {
                    let rendered = if is_json { sc.to_json() } else { sc.to_human() };
                    if let Some(path) = output {
                        if let Err(e) = std::fs::write(&path, &rendered) {
                            eprintln!("Failed to write scorecard to {path}: {e}");
                            std::process::exit(1);
                        }
                        eprintln!("Wrote scorecard to {path}");
                    } else {
                        print!("{rendered}");
                    }
                }
                Err(e) => {
                    eprintln!("Scorecard failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "eval" => {
            let path = args.get(1).map_or(".", std::string::String::as_str);
            let is_json = args.iter().any(|a| a == "--json");
            let root = std::path::Path::new(path);

            let index = crate::core::bm25_index::BM25Index::build_from_directory(root);
            let cfg = crate::core::hybrid_search::HybridConfig::from_config();
            let queries = crate::core::eval_harness::generate_self_eval(&index, 50);

            if queries.is_empty() {
                eprintln!("No symbols found — cannot generate eval queries.");
                std::process::exit(1);
            }

            let scorecard = crate::core::eval_harness::run_eval(root, &queries, &index, &cfg);
            if is_json {
                if let Ok(json) = serde_json::to_string_pretty(&scorecard) {
                    println!("{json}");
                }
            } else {
                print!("{scorecard}");
            }
        }
        "run" => {
            let path = args.get(1).map_or(".", std::string::String::as_str);
            let is_json = args.iter().any(|a| a == "--json");

            let result = benchmark::run_project_benchmark(path);
            if is_json {
                println!("{}", benchmark::format_json(&result));
            } else {
                println!("{}", benchmark::format_terminal(&result));
            }
        }
        "report" => {
            let path = args.get(1).map_or(".", std::string::String::as_str);
            let result = benchmark::run_project_benchmark(path);
            println!("{}", benchmark::format_markdown(&result));
        }
        "compare" => {
            let repo = parse_flag_value(args, "--repo").unwrap_or_else(|| ".".to_string());
            let output = parse_flag_value(args, "--output");

            let root = std::path::Path::new(&repo);
            if !root.exists() {
                eprintln!("Repository path does not exist: {repo}");
                std::process::exit(1);
            }

            let report = benchmark_compare::run_compare(root, output.as_deref());

            println!("{}", benchmark_compare::report::generate_terminal(&report));

            if output.is_none() {
                eprintln!("Tip: use --output BENCHMARKS.md to save the full markdown report");
            }
        }
        _ => {
            if std::path::Path::new(action).exists() {
                let result = benchmark::run_project_benchmark(action);
                println!("{}", benchmark::format_terminal(&result));
            } else {
                eprintln!("Usage: lean-ctx benchmark run [path] [--json]");
                eprintln!("       lean-ctx benchmark report [path]");
                eprintln!("       lean-ctx benchmark eval [path] [--json]");
                eprintln!("       lean-ctx benchmark compare [--repo path] [--output file.md]");
                eprintln!("       lean-ctx benchmark scorecard [--json] [--output file]");
                std::process::exit(1);
            }
        }
    }
}

fn parse_flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

pub fn cmd_stats(args: &[String]) {
    match args.first().map(std::string::String::as_str) {
        Some("reset-cep") => {
            crate::core::stats::reset_cep();
            println!("CEP stats reset. Shell hook data preserved.");
        }
        Some("json") => {
            let store = crate::core::stats::load();
            println!(
                "{}",
                serde_json::to_string_pretty(&store).unwrap_or_else(|_| "{}".to_string())
            );
        }
        _ => {
            let store = crate::core::stats::load();
            let input_saved = store
                .total_input_tokens
                .saturating_sub(store.total_output_tokens);
            let pct = if store.total_input_tokens > 0 {
                input_saved as f64 / store.total_input_tokens as f64 * 100.0
            } else {
                0.0
            };
            println!("Commands:    {}", store.total_commands);
            println!("Input:       {} tokens", store.total_input_tokens);
            println!("Output:      {} tokens", store.total_output_tokens);
            println!("Saved:       {input_saved} tokens ({pct:.1}%)");
            println!();
            println!("CEP sessions:  {}", store.cep.sessions);
            println!(
                "CEP tokens:    {} → {}",
                store.cep.total_tokens_original, store.cep.total_tokens_compressed
            );
            println!();
            println!("Subcommands: stats reset-cep | stats json");
        }
    }
}

pub fn cmd_cache(args: &[String]) {
    use crate::core::cli_cache;
    match args.first().map(std::string::String::as_str) {
        Some("clear") => {
            let count = cli_cache::clear();
            println!("Cleared {count} cached entries.");
        }
        Some("reset") => {
            let project_flag = args.get(1).map(std::string::String::as_str) == Some("--project");
            if project_flag {
                let root =
                    crate::core::session::SessionState::load_latest().and_then(|s| s.project_root);
                if let Some(root) = root {
                    let count = cli_cache::clear_project(&root);
                    println!("Reset {count} cache entries for project: {root}");
                } else {
                    eprintln!("No active project root found. Start a session first.");
                    std::process::exit(1);
                }
            } else {
                let count = cli_cache::clear();
                println!("Reset all {count} cache entries.");
            }
        }
        Some("stats") => {
            let (hits, reads, entries) = cli_cache::stats();
            let rate = if reads > 0 {
                (hits as f64 / reads as f64 * 100.0).round() as u32
            } else {
                0
            };
            println!("CLI Cache Stats (lean-ctx read / lean-ctx grep):");
            println!("  Entries:   {entries}");
            println!("  Reads:     {reads}");
            println!("  Hits:      {hits}");
            println!("  Hit Rate:  {rate}%");

            if let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir() {
                let live_path = dir.join("mcp-live.json");
                if let Ok(content) = std::fs::read_to_string(&live_path) {
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                        let mcp_reads = val
                            .get("total_reads")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        let mcp_hits = val
                            .get("cache_hits")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        let mcp_saved = val
                            .get("tokens_saved")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0);
                        let mcp_rate = if mcp_reads > 0 {
                            (mcp_hits as f64 / mcp_reads as f64 * 100.0).round() as u32
                        } else {
                            0
                        };
                        let updated = val
                            .get("updated_at")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown");
                        println!();
                        println!("MCP Session Cache (ctx_read via AI editor):");
                        println!("  Reads:         {mcp_reads}");
                        println!("  Hits:          {mcp_hits}");
                        println!("  Hit Rate:      {mcp_rate}%");
                        println!("  Tokens Saved:  {mcp_saved}");
                        println!("  Last Updated:  {updated}");
                    }
                } else {
                    println!();
                    println!(
                        "MCP Session Cache: no data yet (start a session with your AI editor)"
                    );
                }
            }
        }
        Some("invalidate") => {
            if args.len() < 2 {
                eprintln!("Usage: lean-ctx cache invalidate <path>");
                std::process::exit(1);
            }
            cli_cache::invalidate(&args[1]);
            println!("Invalidated cache for {}", args[1]);
        }
        Some("prune") => {
            let bm25 = prune_bm25_caches();
            let graph = prune_graph_caches();
            let removed = bm25.removed + graph.removed;
            let freed = bm25.bytes_freed + graph.bytes_freed;
            println!(
                "Pruned {} entries, freed {:.1} MB (BM25: {}, graphs: {})",
                removed,
                freed as f64 / 1_048_576.0,
                bm25.removed,
                graph.removed,
            );
        }
        _ => {
            let (hits, reads, entries) = cli_cache::stats();
            let rate = if reads > 0 {
                (hits as f64 / reads as f64 * 100.0).round() as u32
            } else {
                0
            };
            println!("CLI File Cache: {entries} entries, {hits}/{reads} hits ({rate}%)");
            println!();
            println!("Subcommands:");
            println!("  cache stats       Show detailed stats");
            println!("  cache clear       Clear all cached entries");
            println!("  cache reset       Reset all cache (or --project for current project only)");
            println!("  cache invalidate  Remove specific file from cache");
            println!(
                "  cache prune       Remove oversized, quarantined, and orphaned indexes (BM25 + graphs)"
            );
        }
    }
}

pub struct PruneResult {
    pub scanned: u32,
    pub removed: u32,
    pub bytes_freed: u64,
}

pub fn prune_bm25_caches() -> PruneResult {
    let mut result = PruneResult {
        scanned: 0,
        removed: 0,
        bytes_freed: 0,
    };

    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return result;
    };
    let vectors_dir = data_dir.join("vectors");
    let Ok(entries) = std::fs::read_dir(&vectors_dir) else {
        return result;
    };

    let max_bytes = crate::core::config::Config::load().bm25_max_cache_mb_effective() * 1024 * 1024;

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        result.scanned += 1;

        for q_name in &[
            "bm25_index.json.quarantined",
            "bm25_index.bin.quarantined",
            "bm25_index.bin.zst.quarantined",
        ] {
            let quarantined = dir.join(q_name);
            if quarantined.exists() {
                if let Ok(meta) = std::fs::metadata(&quarantined) {
                    result.bytes_freed += meta.len();
                }
                let _ = std::fs::remove_file(&quarantined);
                result.removed += 1;
                println!("  Removed quarantined: {}", quarantined.display());
            }
        }

        let index_path = if dir.join("bm25_index.bin.zst").exists() {
            dir.join("bm25_index.bin.zst")
        } else if dir.join("bm25_index.bin").exists() {
            dir.join("bm25_index.bin")
        } else {
            dir.join("bm25_index.json")
        };
        if let Ok(meta) = std::fs::metadata(&index_path) {
            if meta.len() > max_bytes {
                result.bytes_freed += meta.len();
                let _ = std::fs::remove_file(&index_path);
                result.removed += 1;
                println!(
                    "  Removed oversized ({:.1} MB): {}",
                    meta.len() as f64 / 1_048_576.0,
                    index_path.display()
                );
            }
        }

        let marker = dir.join("project_root.txt");
        if let Ok(root_str) = std::fs::read_to_string(&marker) {
            let root_path = std::path::Path::new(root_str.trim());
            if !root_path.exists() {
                let freed = dir_size(&dir);
                result.bytes_freed += freed;
                let _ = std::fs::remove_dir_all(&dir);
                result.removed += 1;
                println!(
                    "  Removed orphaned ({:.1} MB, project gone: {}): {}",
                    freed as f64 / 1_048_576.0,
                    root_str.trim(),
                    dir.display()
                );
            }
        }
    }

    result
}

pub fn prune_graph_caches() -> PruneResult {
    let mut result = PruneResult {
        scanned: 0,
        removed: 0,
        bytes_freed: 0,
    };

    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return result;
    };
    let graphs_dir = data_dir.join("graphs");
    let Ok(entries) = std::fs::read_dir(&graphs_dir) else {
        return result;
    };

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        result.scanned += 1;

        let index_path = dir.join("index.json.zst");
        let index_json = dir.join("index.json");

        let has_index = index_path.exists() || index_json.exists();
        if !has_index {
            continue;
        }

        let idx_file = if index_path.exists() {
            &index_path
        } else {
            &index_json
        };

        let root_from_index = try_read_project_root_from_graph(idx_file);
        if let Some(root) = root_from_index {
            if !root.is_empty() && !std::path::Path::new(&root).exists() {
                let freed = dir_size(&dir);
                result.bytes_freed += freed;
                let _ = std::fs::remove_dir_all(&dir);
                result.removed += 1;
                println!(
                    "  Removed orphaned graph ({:.1} MB, project gone: {}): {}",
                    freed as f64 / 1_048_576.0,
                    root,
                    dir.display()
                );
                continue;
            }
        }

        if let Ok(meta) = std::fs::metadata(idx_file) {
            if meta.len() > 100 * 1024 * 1024 {
                result.bytes_freed += meta.len();
                let _ = std::fs::remove_file(idx_file);
                result.removed += 1;
                println!(
                    "  Removed oversized graph ({:.1} MB): {}",
                    meta.len() as f64 / 1_048_576.0,
                    idx_file.display()
                );
            }
        }
    }

    result
}

fn try_read_project_root_from_graph(path: &std::path::Path) -> Option<String> {
    let data = if path.extension().and_then(|e| e.to_str()) == Some("zst") {
        let compressed = std::fs::read(path).ok()?;
        zstd::decode_all(compressed.as_slice()).ok()?
    } else {
        std::fs::read(path).ok()?
    };
    let content = String::from_utf8(data).ok()?;
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;
    val.get("project_root")?.as_str().map(String::from)
}

pub const SIMPLIFIED_TEMPLATE: &str = r#"# lean-ctx — Simplified Configuration
# Full reference: https://leanctx.com/docs/configuration
# For all settings: lean-ctx config init --full

# ── High-Level Knobs ─────────────────────────────────────────────────
# These auto-adjust advanced settings. Override individual values below
# only if you need fine-grained control.

# Output style for the model's prose (not tool-output compression):
#   off    — no style guidance
#   lite   — plain-English concise (default; readable, still token-saving)
#   standard / max — denser symbolic "power modes" (opt-in)
compression_level = "lite"

# RAM/feature trade-off: low | balanced | performance
memory_profile = "balanced"

# Maximum % of system RAM lean-ctx may use (1-50)
max_ram_percent = 5

# Total disk budget in MB (0 = use individual limits).
# Distributes proportionally: archive ~25%, BM25 cache ~10%.
# max_disk_mb = 2000

# Auto-purge data older than N days (0 = disabled).
# Flows into archive.max_age_hours.
# max_staleness_days = 30

# Explicit project paths to scan/index (default: auto-detect).
# [ide_paths]
# cursor = ["/home/user/projects/app1"]

# ── Proxy ────────────────────────────────────────────────────────────
# proxy_enabled = false
# proxy_port = 3128
"#;

fn write_simplified_config() -> Result<String, String> {
    let path = config::Config::path().ok_or_else(|| "Cannot determine config path".to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{e}"))?;
    }
    std::fs::write(&path, SIMPLIFIED_TEMPLATE).map_err(|e| format!("{e}"))?;
    Ok(path.to_string_lossy().to_string())
}

fn cmd_show_effective() {
    let cfg = config::Config::load();
    let compression = config::CompressionLevel::effective(&cfg);
    let policy = cfg.memory_policy_effective().unwrap_or_default();

    println!("╭─── Simplified (high-level) ───────────────────────────────╮");
    println!(
        "│ compression_level   = {:10}  {}",
        format!("{compression:?}"),
        source_hint(
            "LEAN_CTX_COMPRESSION",
            cfg.compression_level != config::CompressionLevel::Off
        )
    );
    println!(
        "│ max_disk_mb         = {:10}  {}",
        cfg.max_disk_mb_effective(),
        source_hint("LEAN_CTX_MAX_DISK_MB", cfg.max_disk_mb > 0)
    );
    println!(
        "│ max_ram_percent     = {:10}  {}",
        cfg.max_ram_percent,
        source_hint("LEAN_CTX_MAX_RAM_PERCENT", cfg.max_ram_percent != 5)
    );
    println!(
        "│ max_staleness_days  = {:10}  {}",
        cfg.max_staleness_days_effective(),
        source_hint("LEAN_CTX_MAX_STALENESS_DAYS", cfg.max_staleness_days > 0)
    );
    println!(
        "│ memory_profile      = {:10}  {}",
        format!("{:?}", cfg.memory_profile),
        source_hint("LEAN_CTX_MEMORY_PROFILE", false)
    );
    println!("╰────────────────────────────────────────────────────────────╯");

    println!();
    println!("╭─── Derived effective limits ────────────────────────────────╮");
    println!(
        "│ archive_max_disk_mb    = {:>6} MB",
        cfg.archive_max_disk_mb_effective()
    );
    println!(
        "│ bm25_max_cache_mb      = {:>6} MB",
        cfg.bm25_max_cache_mb_effective()
    );
    println!(
        "│ archive_max_age_hours  = {:>6} h",
        cfg.archive_max_age_hours_effective()
    );
    println!(
        "│ graph_index_max_files  = {:>6}",
        cfg.graph_index_max_files
    );
    println!("│");
    println!(
        "│ memory.knowledge.max_facts     = {:>6}",
        policy.knowledge.max_facts
    );
    println!(
        "│ memory.knowledge.max_patterns  = {:>6}",
        policy.knowledge.max_patterns
    );
    println!(
        "│ memory.episodic.max_episodes   = {:>6}",
        policy.episodic.max_episodes
    );
    println!(
        "│ memory.procedural.max_procedures = {:>4}",
        policy.procedural.max_procedures
    );
    println!("╰────────────────────────────────────────────────────────────╯");

    if cfg.max_disk_mb_effective() > 0 {
        println!();
        println!(
            "  ℹ  max_disk_mb={} → limits scaled proportionally (factor: {:.1}x)",
            cfg.max_disk_mb_effective(),
            (cfg.max_disk_mb_effective() as f64 / 500.0).clamp(0.5, 10.0)
        );
    }
}

fn source_hint(env_var: &str, config_set: bool) -> &'static str {
    if std::env::var(env_var).is_ok() {
        "← env"
    } else if config_set {
        "← config"
    } else {
        "← default"
    }
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                total += std::fs::metadata(&p).map_or(0, |m| m.len());
            } else if p.is_dir() {
                total += dir_size(&p);
            }
        }
    }
    total
}
