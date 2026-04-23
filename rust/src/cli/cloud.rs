use crate::{cloud_client, core};

fn parse_auth_args(args: &[String]) -> (String, Option<String>) {
    let mut email = String::new();
    let mut password: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--password" | "-p" => {
                i += 1;
                if i < args.len() {
                    password = Some(args[i].clone());
                }
            }
            _ => {
                if email.is_empty() {
                    email = args[i].trim().to_lowercase();
                }
            }
        }
        i += 1;
    }
    (email, password)
}

fn require_email_and_password(args: &[String], usage: &str) -> (String, String) {
    let (email, password) = parse_auth_args(args);

    if email.is_empty() {
        eprintln!("Usage: {usage}");
        std::process::exit(1);
    }
    if !email.contains('@') || !email.contains('.') {
        eprintln!("Invalid email address: {email}");
        std::process::exit(1);
    }

    let pw = match password {
        Some(p) => p,
        None => match rpassword::prompt_password("Password: ") {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Could not read password: {e}");
                std::process::exit(1);
            }
        },
    };
    if pw.len() < 8 {
        eprintln!("Password must be at least 8 characters.");
        std::process::exit(1);
    }
    (email, pw)
}

fn save_and_report(r: &cloud_client::RegisterResult, email: &str) {
    if let Err(e) = cloud_client::save_credentials(&r.api_key, &r.user_id, email) {
        eprintln!("Warning: Could not save credentials: {e}");
        eprintln!("Please try again.");
        return;
    }
    if let Ok(plan) = cloud_client::fetch_plan() {
        let _ = cloud_client::save_plan(&plan);
    }
    println!("API key saved to ~/.lean-ctx/cloud/credentials.json");
    if r.verification_sent {
        println!("Verification email sent — please check your inbox.");
    }
    if !r.email_verified {
        println!("Note: Your email is not yet verified.");
    }
}

pub fn cmd_login(args: &[String]) {
    let (email, pw) = require_email_and_password(args, "lean-ctx login <email> [--password <pw>]");

    println!("Logging in to LeanCTX Cloud...");

    match cloud_client::login(&email, &pw) {
        Ok(r) => {
            save_and_report(&r, &email);
            println!("Logged in as {email}");
        }
        Err(e) if e.contains("403") => {
            eprintln!("Please verify your email first. Check your inbox.");
            std::process::exit(1);
        }
        Err(e) if e.contains("Invalid email or password") => {
            eprintln!("Invalid email or password.");
            eprintln!("Forgot your password? Run: lean-ctx forgot-password <email>");
            eprintln!("No account yet? Run: lean-ctx register <email>");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Login failed: {e}");
            eprintln!("If you don't have an account yet, run: lean-ctx register <email>");
            std::process::exit(1);
        }
    }
}

pub fn cmd_forgot_password(args: &[String]) {
    let (email, _) = parse_auth_args(args);

    if email.is_empty() {
        eprintln!("Usage: lean-ctx forgot-password <email>");
        std::process::exit(1);
    }

    println!("Sending password reset email...");

    match cloud_client::forgot_password(&email) {
        Ok(msg) => {
            println!("{msg}");
            println!("Check your inbox and follow the reset link.");
        }
        Err(e) => {
            eprintln!("Failed: {e}");
            std::process::exit(1);
        }
    }
}

pub fn cmd_register(args: &[String]) {
    let (email, pw) =
        require_email_and_password(args, "lean-ctx register <email> [--password <pw>]");

    println!("Creating LeanCTX Cloud account...");

    match cloud_client::register(&email, Some(&pw)) {
        Ok(r) => {
            save_and_report(&r, &email);
            println!("Account created for {email}");
        }
        Err(e) if e.contains("409") || e.contains("already exists") => {
            eprintln!("An account with this email already exists.");
            eprintln!("Run: lean-ctx login <email>");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Registration failed: {e}");
            std::process::exit(1);
        }
    }
}

pub fn cmd_sync() {
    if !cloud_client::is_logged_in() {
        eprintln!("Not logged in. Run: lean-ctx login <email>");
        std::process::exit(1);
    }

    println!("Syncing stats...");
    let store = core::stats::load();
    let entries = build_sync_entries(&store);
    if entries.is_empty() {
        println!("No stats to sync yet.");
    } else {
        match cloud_client::sync_stats(&entries) {
            Ok(_) => println!("  Stats: synced"),
            Err(e) => eprintln!("  Stats sync failed: {e}"),
        }
    }

    println!("Syncing commands...");
    let command_entries = collect_command_entries(&store);
    if command_entries.is_empty() {
        println!("  No command data to sync.");
    } else {
        match cloud_client::push_commands(&command_entries) {
            Ok(_) => println!("  Commands: synced"),
            Err(e) => eprintln!("  Commands sync failed: {e}"),
        }
    }

    println!("Syncing CEP scores...");
    let cep_entries = collect_cep_entries(&store);
    if cep_entries.is_empty() {
        println!("  No CEP sessions to sync.");
    } else {
        match cloud_client::push_cep(&cep_entries) {
            Ok(_) => println!("  CEP: synced"),
            Err(e) => eprintln!("  CEP sync failed: {e}"),
        }
    }

    println!("Syncing knowledge...");
    let knowledge_entries = collect_knowledge_entries();
    if knowledge_entries.is_empty() {
        println!("  No knowledge to sync.");
    } else {
        match cloud_client::push_knowledge(&knowledge_entries) {
            Ok(_) => println!("  Knowledge: synced"),
            Err(e) => eprintln!("  Knowledge sync failed: {e}"),
        }
    }

    println!("Syncing gotchas...");
    let gotcha_entries = collect_gotcha_entries();
    if gotcha_entries.is_empty() {
        println!("  No gotchas to sync.");
    } else {
        match cloud_client::push_gotchas(&gotcha_entries) {
            Ok(_) => println!("  Gotchas: synced"),
            Err(e) => eprintln!("  Gotchas sync failed: {e}"),
        }
    }

    println!("Syncing buddy...");
    let buddy = core::buddy::BuddyState::compute();
    let buddy_data = serde_json::to_value(&buddy).unwrap_or_default();
    match cloud_client::push_buddy(&buddy_data) {
        Ok(_) => println!("  Buddy: synced"),
        Err(e) => eprintln!("  Buddy sync failed: {e}"),
    }

    println!("Syncing feedback thresholds...");
    let feedback_entries = collect_feedback_entries();
    if feedback_entries.is_empty() {
        println!("  No feedback thresholds to sync.");
    } else {
        match cloud_client::push_feedback(&feedback_entries) {
            Ok(_) => println!("  Feedback: synced"),
            Err(e) => eprintln!("  Feedback sync failed: {e}"),
        }
    }

    if let Ok(plan) = cloud_client::fetch_plan() {
        let _ = cloud_client::save_plan(&plan);
    }

    println!("Sync complete.");
}

fn build_sync_entries(store: &core::stats::StatsStore) -> Vec<serde_json::Value> {
    crate::cloud_sync::build_sync_entries(store)
}

fn collect_knowledge_entries() -> Vec<serde_json::Value> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };
    let knowledge_dir = home.join(".lean-ctx").join("knowledge");
    if !knowledge_dir.is_dir() {
        return Vec::new();
    }

    let mut entries = Vec::new();

    for project_entry in std::fs::read_dir(&knowledge_dir).into_iter().flatten() {
        let project_entry = match project_entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }

        for file_entry in std::fs::read_dir(&project_path).into_iter().flatten() {
            let file_entry = match file_entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let file_path = file_entry.path();
            if file_path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let data = match std::fs::read_to_string(&file_path) {
                Ok(d) => d,
                Err(_) => continue,
            };
            let parsed: serde_json::Value = match serde_json::from_str(&data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if let Some(facts) = parsed["facts"].as_array() {
                for fact in facts {
                    let cat = fact["category"].as_str().unwrap_or("general");
                    let key = fact["key"].as_str().unwrap_or("");
                    let val = fact["value"]
                        .as_str()
                        .or_else(|| fact["description"].as_str())
                        .unwrap_or("");
                    if !key.is_empty() {
                        entries.push(serde_json::json!({
                            "category": cat,
                            "key": key,
                            "value": val,
                        }));
                    }
                }
            }

            if let Some(gotchas) = parsed["gotchas"].as_array() {
                for g in gotchas {
                    let pattern = g["pattern"].as_str().unwrap_or("");
                    let fix = g["fix"].as_str().unwrap_or("");
                    if !pattern.is_empty() {
                        entries.push(serde_json::json!({
                            "category": "gotcha",
                            "key": pattern,
                            "value": fix,
                        }));
                    }
                }
            }
        }
    }

    entries
}

fn collect_command_entries(store: &core::stats::StatsStore) -> Vec<serde_json::Value> {
    store
        .commands
        .iter()
        .map(|(name, stats)| {
            let tokens_saved = stats.input_tokens.saturating_sub(stats.output_tokens);
            serde_json::json!({
                "command": name,
                "source": if name.starts_with("ctx_") { "mcp" } else { "hook" },
                "count": stats.count,
                "input_tokens": stats.input_tokens,
                "output_tokens": stats.output_tokens,
                "tokens_saved": tokens_saved,
            })
        })
        .collect()
}

fn complexity_to_float(s: &str) -> f64 {
    match s.to_lowercase().as_str() {
        "trivial" => 0.1,
        "simple" => 0.3,
        "moderate" => 0.5,
        "complex" => 0.7,
        "architectural" => 0.9,
        other => other.parse::<f64>().unwrap_or(0.5),
    }
}

fn collect_cep_entries(store: &core::stats::StatsStore) -> Vec<serde_json::Value> {
    store
        .cep
        .scores
        .iter()
        .map(|s| {
            serde_json::json!({
                "recorded_at": s.timestamp,
                "score": s.score as f64 / 100.0,
                "cache_hit_rate": s.cache_hit_rate as f64 / 100.0,
                "mode_diversity": s.mode_diversity as f64 / 100.0,
                "compression_rate": s.compression_rate as f64 / 100.0,
                "tool_calls": s.tool_calls,
                "tokens_saved": s.tokens_saved,
                "complexity": complexity_to_float(&s.complexity),
            })
        })
        .collect()
}

fn collect_gotcha_entries() -> Vec<serde_json::Value> {
    let mut all_gotchas = core::gotcha_tracker::load_universal_gotchas();

    if let Some(home) = dirs::home_dir() {
        let knowledge_dir = home.join(".lean-ctx").join("knowledge");
        if let Ok(entries) = std::fs::read_dir(&knowledge_dir) {
            for entry in entries.flatten() {
                let gotcha_path = entry.path().join("gotchas.json");
                if gotcha_path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&gotcha_path) {
                        if let Ok(store) =
                            serde_json::from_str::<core::gotcha_tracker::GotchaStore>(&content)
                        {
                            for g in store.gotchas {
                                if !all_gotchas
                                    .iter()
                                    .any(|existing| existing.trigger == g.trigger)
                                {
                                    all_gotchas.push(g);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    all_gotchas
        .iter()
        .map(|g| {
            serde_json::json!({
                "pattern": g.trigger,
                "fix": g.resolution,
                "severity": format!("{:?}", g.severity).to_lowercase(),
                "category": format!("{:?}", g.category).to_lowercase(),
                "occurrences": g.occurrences,
                "prevented_count": g.prevented_count,
                "confidence": g.confidence,
            })
        })
        .collect()
}

fn collect_feedback_entries() -> Vec<serde_json::Value> {
    let store = core::feedback::FeedbackStore::load();
    store
        .learned_thresholds
        .iter()
        .map(|(lang, thresholds)| {
            serde_json::json!({
                "language": lang,
                "entropy": thresholds.entropy,
                "jaccard": thresholds.jaccard,
                "sample_count": thresholds.sample_count,
                "avg_efficiency": thresholds.avg_efficiency,
            })
        })
        .collect()
}

pub fn cmd_contribute() {
    let mut entries = Vec::new();

    if let Some(home) = dirs::home_dir() {
        let mode_stats_path = home.join(".lean-ctx").join("mode_stats.json");
        if let Ok(data) = std::fs::read_to_string(&mode_stats_path) {
            if let Ok(predictor) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(history) = predictor["history"].as_object() {
                    for (_sig_key, outcomes) in history {
                        if let Some(arr) = outcomes.as_array() {
                            for outcome in arr.iter().rev().take(5) {
                                let ext = outcome["ext"].as_str().unwrap_or("unknown");
                                let mode = outcome["mode"].as_str().unwrap_or("full");
                                let tokens_in = outcome["tokens_in"].as_u64().unwrap_or(0);
                                let tokens_out = outcome["tokens_out"].as_u64().unwrap_or(0);
                                let ratio = if tokens_in > 0 {
                                    1.0 - tokens_out as f64 / tokens_in as f64
                                } else {
                                    0.0
                                };
                                let bucket = match tokens_in {
                                    0..=500 => "0-500",
                                    501..=2000 => "500-2k",
                                    2001..=10000 => "2k-10k",
                                    _ => "10k+",
                                };
                                entries.push(serde_json::json!({
                                    "file_ext": format!(".{ext}"),
                                    "size_bucket": bucket,
                                    "best_mode": mode,
                                    "compression_ratio": (ratio * 100.0).round() / 100.0,
                                }));
                                if entries.len() >= 500 {
                                    break;
                                }
                            }
                        }
                        if entries.len() >= 500 {
                            break;
                        }
                    }
                }
            }
        }
    }

    if entries.is_empty() {
        let stats_data = core::stats::format_gain_json();
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stats_data) {
            let original = parsed["cep"]["total_tokens_original"].as_u64().unwrap_or(0);
            let compressed = parsed["cep"]["total_tokens_compressed"]
                .as_u64()
                .unwrap_or(0);
            let overall_ratio = if original > 0 {
                1.0 - compressed as f64 / original as f64
            } else {
                0.0
            };

            if let Some(modes) = parsed["cep"]["modes"].as_object() {
                let read_modes = ["full", "map", "signatures", "auto", "aggressive", "entropy"];
                for (mode, count) in modes {
                    if !read_modes.contains(&mode.as_str()) || count.as_u64().unwrap_or(0) == 0 {
                        continue;
                    }
                    entries.push(serde_json::json!({
                        "file_ext": "mixed",
                        "size_bucket": "mixed",
                        "best_mode": mode,
                        "compression_ratio": (overall_ratio * 100.0).round() / 100.0,
                    }));
                }
            }
        }
    }

    if entries.is_empty() {
        println!("No compression data to contribute yet. Use lean-ctx for a while first.");
        return;
    }

    println!("Contributing {} data points...", entries.len());
    match cloud_client::contribute(&entries) {
        Ok(msg) => println!("{msg}"),
        Err(e) => {
            eprintln!("Contribute failed: {e}");
            std::process::exit(1);
        }
    }
}

pub fn cmd_cloud(args: &[String]) {
    let action = args.first().map(|s| s.as_str()).unwrap_or("help");

    match action {
        "pull-models" => {
            println!("Updating adaptive models...");
            match cloud_client::pull_cloud_models() {
                Ok(data) => {
                    let count = data
                        .get("models")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);

                    if let Err(e) = cloud_client::save_cloud_models(&data) {
                        eprintln!("Warning: Could not save models: {e}");
                        return;
                    }
                    println!("{count} adaptive models updated.");
                    if let Some(est) = data.get("improvement_estimate").and_then(|v| v.as_f64()) {
                        println!("Estimated compression improvement: +{:.0}%", est * 100.0);
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        "status" => {
            if cloud_client::is_logged_in() {
                println!("Connected to LeanCTX Cloud.");
            } else {
                println!("Not connected to LeanCTX Cloud.");
                println!("Get started: lean-ctx login <email>");
            }
        }
        _ => {
            println!("Usage: lean-ctx cloud <command>");
            println!("  pull-models — Update adaptive compression models");
            println!("  status      — Show cloud connection status");
        }
    }
}

pub fn cmd_gotchas(args: &[String]) {
    let action = args.first().map(|s| s.as_str()).unwrap_or("list");
    let project_root = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());

    match action {
        "list" | "ls" => {
            let store = core::gotcha_tracker::GotchaStore::load(&project_root);
            println!("{}", store.format_list());
        }
        "clear" => {
            let mut store = core::gotcha_tracker::GotchaStore::load(&project_root);
            let count = store.gotchas.len();
            store.clear();
            let _ = store.save(&project_root);
            println!("Cleared {count} gotchas.");
        }
        "export" => {
            let store = core::gotcha_tracker::GotchaStore::load(&project_root);
            match serde_json::to_string_pretty(&store.gotchas) {
                Ok(json) => println!("{json}"),
                Err(e) => eprintln!("Export failed: {e}"),
            }
        }
        "stats" => {
            let store = core::gotcha_tracker::GotchaStore::load(&project_root);
            println!("Bug Memory Stats:");
            println!("  Active gotchas:      {}", store.gotchas.len());
            println!(
                "  Errors detected:     {}",
                store.stats.total_errors_detected
            );
            println!(
                "  Fixes correlated:    {}",
                store.stats.total_fixes_correlated
            );
            println!("  Bugs prevented:      {}", store.stats.total_prevented);
            println!("  Promoted to knowledge: {}", store.stats.gotchas_promoted);
            println!("  Decayed/archived:    {}", store.stats.gotchas_decayed);
            println!("  Session logs:        {}", store.error_log.len());
        }
        _ => {
            println!("Usage: lean-ctx gotchas [list|clear|export|stats]");
        }
    }
}

pub fn cmd_buddy(args: &[String]) {
    let cfg = core::config::Config::load();
    if !cfg.buddy_enabled {
        println!("Buddy is disabled. Enable with: lean-ctx config buddy_enabled true");
        return;
    }

    let action = args.first().map(|s| s.as_str()).unwrap_or("show");
    let buddy = core::buddy::BuddyState::compute();
    let theme = core::theme::load_theme(&cfg.theme);

    match action {
        "show" | "status" => {
            println!("{}", core::buddy::format_buddy_full(&buddy, &theme));
        }
        "stats" => {
            println!("{}", core::buddy::format_buddy_full(&buddy, &theme));
        }
        "ascii" => {
            for line in &buddy.ascii_art {
                println!("  {line}");
            }
        }
        "json" => match serde_json::to_string_pretty(&buddy) {
            Ok(json) => println!("{json}"),
            Err(e) => eprintln!("JSON error: {e}"),
        },
        _ => {
            println!("Usage: lean-ctx buddy [show|stats|ascii|json]");
        }
    }
}

pub fn cmd_upgrade() {
    println!("'upgrade' has been renamed to 'update'. Running 'lean-ctx update' instead.\n");
    core::updater::run(&[]);
}
