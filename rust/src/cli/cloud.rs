use crate::{cloud_client, core};

fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) if local.len() > 2 => {
            format!("{}...@{domain}", &local[..local.floor_char_boundary(2)])
        }
        _ => "***".to_string(),
    }
}

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
        tracing::error!("Invalid email address: {email}");
        std::process::exit(1);
    }

    let pw = match password {
        Some(p) => p,
        None => match rpassword::prompt_password("Password: ") {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Could not read password: {e}");
                std::process::exit(1);
            }
        },
    };
    if pw.len() < 8 {
        tracing::error!("Password must be at least 8 characters.");
        std::process::exit(1);
    }
    (email, pw)
}

fn save_and_report(r: &cloud_client::RegisterResult, email: &str) {
    if let Err(e) = cloud_client::save_credentials(&r.api_key, &r.user_id, email) {
        tracing::warn!("Could not save credentials: {e}");
        eprintln!("Please try again.");
        return;
    }
    if let Ok(plan) = cloud_client::fetch_plan() {
        let _ = cloud_client::save_plan(&plan);
    }
    // Upgrade remote auth to OAuth2 client_credentials when supported by the API.
    match cloud_client::oauth_register_client(Some("lean-ctx-cli")) {
        Ok(msg) => tracing::info!("{msg}"),
        Err(e) => tracing::warn!("OAuth upgrade skipped: {e}"),
    }

    println!("Cloud credentials saved (see ~/.lean-ctx/cloud/credentials.json)");
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
            println!("Logged in as {}", mask_email(&email));
        }
        Err(e) if e.contains("403") => {
            tracing::error!("Please verify your email first. Check your inbox.");
            std::process::exit(1);
        }
        Err(e) if e.contains("Invalid email or password") => {
            tracing::error!("Invalid email or password.");
            eprintln!("Forgot your password? Run: lean-ctx forgot-password <email>");
            eprintln!("No account yet? Run: lean-ctx register <email>");
            std::process::exit(1);
        }
        Err(e) => {
            tracing::error!("Login failed: {e}");
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
        Ok(_msg) => {
            println!("Password reset email sent to {}.", mask_email(&email));
            println!("Check your inbox and follow the reset link.");
        }
        Err(e) => {
            tracing::error!("Failed: {e}");
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
            println!("Account created for {}", mask_email(&email));
        }
        Err(e) if e.contains("409") || e.contains("already exists") => {
            tracing::error!("An account with this email already exists.");
            eprintln!("Run: lean-ctx login <email>");
            std::process::exit(1);
        }
        Err(e) => {
            tracing::error!("Registration failed: {e}");
            std::process::exit(1);
        }
    }
}

pub fn cmd_sync(rest: &[String]) {
    if rest.first().map(String::as_str) == Some("index") {
        cmd_sync_index(&rest[1..]);
        return;
    }
    if !cloud_client::is_logged_in() {
        tracing::error!("Not logged in. Run: lean-ctx login <email>");
        std::process::exit(1);
    }

    // Stats roll-up is account-level and stays free for everyone.
    println!("Syncing stats...");
    let store = core::stats::load();
    let entries = build_sync_entries(&store);
    if entries.is_empty() {
        println!("No stats to sync yet.");
    } else {
        match cloud_client::sync_stats(&entries) {
            Ok(_) => println!("  Stats: synced"),
            Err(e) => tracing::error!("Stats sync failed: {e}"),
        }
    }

    // Everything below is the Pro "Personal Cloud" (cross-device sync of your own
    // context). On a Free account the server returns 402; detect it once and show
    // a friendly upgrade hint instead of one failure per surface.
    if sync_personal_cloud(&store) == CloudSyncOutcome::Gated {
        print_pro_upgrade_hint();
        return;
    }

    if let Ok(plan) = cloud_client::fetch_plan() {
        let _ = cloud_client::save_plan(&plan);
    }

    println!("Sync complete.");
}

/// `lean-ctx sync index <push|pull|status>` — the hosted Personal Index
/// (GL #392): encrypted cross-device sync of the project's retrieval index.
fn cmd_sync_index(args: &[String]) {
    let sub = args.first().map_or("help", String::as_str);
    let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    match sub {
        "push" => match cloud_client::push_index_bundle(&root) {
            Ok((project_hash, bytes)) => {
                println!(
                    "\x1b[32m✓\x1b[0m Index pushed ({:.1} MB encrypted, project {})",
                    bytes as f64 / 1_048_576.0,
                    &project_hash[..12.min(project_hash.len())]
                );
                println!("  Pull on any device: lean-ctx sync index pull");
            }
            Err(e) => {
                eprintln!("\x1b[31m✗\x1b[0m {e}");
                std::process::exit(1);
            }
        },
        "pull" => match cloud_client::pull_index_bundle(&root) {
            Ok(manifest) => {
                println!(
                    "\x1b[32m✓\x1b[0m Index restored ({} files, built {} by v{})",
                    manifest.files.len(),
                    manifest.created_at,
                    manifest.engine_version
                );
                println!("  Semantic search is ready — no local re-index needed.");
            }
            Err(e) => {
                eprintln!("\x1b[31m✗\x1b[0m {e}");
                std::process::exit(1);
            }
        },
        "status" => match cloud_client::index_bundle_status() {
            Ok(v) => {
                let used_mb = v["used_bytes"].as_u64().unwrap_or(0) as f64 / 1_048_576.0;
                let quota_mb = v["quota_mb"].as_u64().unwrap_or(0);
                println!("Hosted Personal Index");
                println!("  Usage: {used_mb:.1} MB / {quota_mb} MB");
                if let Some(line) = render_quota_state(&v["storage"]) {
                    println!("  {line}");
                }
                if let Some(buckets) = v["projects"].as_array() {
                    if buckets.is_empty() {
                        println!("  No project bundles yet. Push one: lean-ctx sync index push");
                    }
                    for b in buckets {
                        println!(
                            "  • {}  {:.1} MB  (updated {})",
                            b["project_hash"].as_str().unwrap_or("?"),
                            b["size_bytes"].as_u64().unwrap_or(0) as f64 / 1_048_576.0,
                            b["updated_at"].as_str().unwrap_or("?")
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("\x1b[31m✗\x1b[0m {e}");
                std::process::exit(1);
            }
        },
        _ => {
            println!("Usage: lean-ctx sync index <push|pull|status>");
            println!("  push    Pack, encrypt and upload this project's retrieval index");
            println!("  pull    Download and restore the hosted index on this device");
            println!("  status  Show hosted buckets and quota usage");
        }
    }
}

/// One human line for the server's billing-plane-v2 `storage` block (GL #392):
/// green/yellow/red by threshold state, with the headroom or overage spelled
/// out. `None` when the server (older deploy) sent no block — print nothing
/// rather than guessing.
fn render_quota_state(storage: &serde_json::Value) -> Option<String> {
    let state = storage["state"].as_str()?;
    let percent = storage["percent"].as_f64();
    let pct = percent.map_or(String::new(), |p| format!(" ({p:.0}% of quota)"));
    Some(match state {
        "ok" => format!("State: \x1b[32mok\x1b[0m{pct}"),
        "warn" => format!("State: \x1b[33mwarn\x1b[0m{pct} — consider pruning old buckets"),
        "critical" => {
            format!("State: \x1b[31mcritical\x1b[0m{pct} — next push may exceed the quota")
        }
        "over" => {
            let over_mb = storage["overage_bytes"].as_u64().unwrap_or(0) as f64 / 1_000_000.0;
            format!(
                "State: \x1b[31mover\x1b[0m{pct} — {over_mb:.1} MB over; pushes are blocked (nothing is billed). Free space: lean-ctx sync index status / delete"
            )
        }
        // "none" (no entitlement) and future states: the usage line above
        // already says everything actionable.
        _ => return None,
    })
}

/// Whether a `cloud_client` error string is the server's Pro gate (HTTP 402),
/// mirroring the existing 403 string-match in `cloud_client::pull_cloud_models`.
fn pro_gate_hit(err: &str) -> bool {
    err.contains("402")
}

#[derive(PartialEq, Eq)]
enum CloudSyncOutcome {
    Done,
    Gated,
}

/// Push the Pro-gated "Personal Cloud" surfaces. Returns [`CloudSyncOutcome::Gated`]
/// at the first 402 (a Free account) so the caller shows a single upgrade hint
/// rather than one error per surface. A self-hosted backend with the gate open
/// (billing unset / `LEANCTX_CLOUD_SYNC_OPEN`) never returns 402, so all sync.
fn sync_personal_cloud(store: &core::stats::StatsStore) -> CloudSyncOutcome {
    println!("Syncing commands...");
    let command_entries = collect_command_entries(store);
    if command_entries.is_empty() {
        println!("  No command data to sync.");
    } else {
        match cloud_client::push_commands(&command_entries) {
            Ok(_) => println!("  Commands: synced"),
            Err(e) if pro_gate_hit(&e) => return CloudSyncOutcome::Gated,
            Err(e) => tracing::error!("Commands sync failed: {e}"),
        }
    }

    println!("Syncing CEP scores...");
    let cep_entries = collect_cep_entries(store);
    if cep_entries.is_empty() {
        println!("  No CEP sessions to sync.");
    } else {
        match cloud_client::push_cep(&cep_entries) {
            Ok(_) => println!("  CEP: synced"),
            Err(e) if pro_gate_hit(&e) => return CloudSyncOutcome::Gated,
            Err(e) => tracing::error!("CEP sync failed: {e}"),
        }
    }

    println!("Syncing knowledge...");
    let knowledge_entries = collect_knowledge_entries();
    if knowledge_entries.is_empty() {
        println!("  No knowledge to sync.");
    } else {
        match cloud_client::push_knowledge(&knowledge_entries) {
            Ok(_) => println!("  Knowledge: synced"),
            Err(e) if pro_gate_hit(&e) => return CloudSyncOutcome::Gated,
            Err(e) => tracing::error!("Knowledge sync failed: {e}"),
        }
    }

    println!("Syncing gotchas...");
    let gotcha_entries = collect_gotcha_entries();
    if gotcha_entries.is_empty() {
        println!("  No gotchas to sync.");
    } else {
        match cloud_client::push_gotchas(&gotcha_entries) {
            Ok(_) => println!("  Gotchas: synced"),
            Err(e) if pro_gate_hit(&e) => return CloudSyncOutcome::Gated,
            Err(e) => tracing::error!("Gotchas sync failed: {e}"),
        }
    }

    println!("Syncing buddy...");
    let buddy = core::buddy::BuddyState::compute();
    let buddy_data = serde_json::to_value(&buddy).unwrap_or_default();
    match cloud_client::push_buddy(&buddy_data) {
        Ok(_) => println!("  Buddy: synced"),
        Err(e) if pro_gate_hit(&e) => return CloudSyncOutcome::Gated,
        Err(e) => tracing::error!("Buddy sync failed: {e}"),
    }

    println!("Syncing feedback thresholds...");
    let feedback_entries = collect_feedback_entries();
    if feedback_entries.is_empty() {
        println!("  No feedback thresholds to sync.");
    } else {
        match cloud_client::push_feedback(&feedback_entries) {
            Ok(_) => println!("  Feedback: synced"),
            Err(e) if pro_gate_hit(&e) => return CloudSyncOutcome::Gated,
            Err(e) => tracing::error!("Feedback sync failed: {e}"),
        }
    }

    CloudSyncOutcome::Done
}

/// Friendly, non-error hint shown when the server gates cloud sync behind Pro.
/// Delegates to the central, entitlement-aware hint helper (#346) so the message
/// reflects the user's actual plan and the cheapest unlocking tier.
fn print_pro_upgrade_hint() {
    super::upgrade_hint::hint_for("cloud_sync");
}

fn build_sync_entries(store: &core::stats::StatsStore) -> Vec<serde_json::Value> {
    crate::cloud_sync::build_sync_entries(store)
}

fn collect_knowledge_entries() -> Vec<serde_json::Value> {
    crate::cloud_sync::collect_knowledge_entries()
}

fn collect_command_entries(store: &core::stats::StatsStore) -> Vec<serde_json::Value> {
    crate::cloud_sync::collect_command_entries(store)
}

fn collect_cep_entries(store: &core::stats::StatsStore) -> Vec<serde_json::Value> {
    crate::cloud_sync::collect_cep_entries(store)
}

fn collect_gotcha_entries() -> Vec<serde_json::Value> {
    crate::cloud_sync::collect_gotcha_entries()
}

fn collect_feedback_entries() -> Vec<serde_json::Value> {
    crate::cloud_sync::collect_feedback_entries()
}

pub fn cmd_contribute() {
    let mut entries = Vec::new();

    // GH #439: mode_stats.json lives in the data dir — read it through the typed
    // resolver (matching cloud_sync) instead of a stale ~/.lean-ctx path.
    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        let mode_stats_path = data_dir.join("mode_stats.json");
        if let Ok(data) = std::fs::read_to_string(&mode_stats_path)
            && let Ok(predictor) = serde_json::from_str::<serde_json::Value>(&data)
            && let Some(history) = predictor["history"].as_object()
        {
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
            tracing::error!("Contribute failed: {e}");
            std::process::exit(1);
        }
    }
}

pub fn cmd_cloud(args: &[String]) {
    let action = args.first().map_or("help", std::string::String::as_str);

    match action {
        "pull-models" => {
            println!("Updating adaptive models...");
            match cloud_client::pull_cloud_models() {
                Ok(data) => {
                    let count = data
                        .get("models")
                        .and_then(|v| v.as_array())
                        .map_or(0, std::vec::Vec::len);

                    if let Err(e) = cloud_client::save_cloud_models(&data) {
                        tracing::warn!("Could not save models: {e}");
                        return;
                    }
                    println!("{count} adaptive models updated.");
                    if let Some(est) = data
                        .get("improvement_estimate")
                        .and_then(serde_json::Value::as_f64)
                    {
                        println!("Estimated compression improvement: +{:.0}%", est * 100.0);
                    }
                }
                Err(e) => {
                    tracing::error!("{e}");
                    std::process::exit(1);
                }
            }
        }
        "status" => cmd_cloud_status(),
        "pull" => cmd_cloud_pull(),
        "autosync" => cmd_cloud_autosync(args.get(1).map(String::as_str)),
        "autoindex" => cmd_cloud_autoindex(args.get(1).map(String::as_str)),
        "upgrade" | "subscribe" => cloud_upgrade(&args[1..]),
        _ => {
            println!("Usage: lean-ctx cloud <command>");
            println!("  pull-models — Update adaptive compression models");
            println!("  pull        — Restore your Personal Cloud knowledge onto this machine");
            println!("  autosync    — on|off|status: daily background Personal Cloud push (Pro)");
            println!("  autoindex   — on|off|status: daily background hosted-index push (Pro)");
            println!("  status      — Show cloud connection status");
            println!(
                "  upgrade     — Subscribe to Pro (Personal Cloud) or Team \
                 [--plan pro|team|business] [--interval monthly|yearly]"
            );
        }
    }
}

/// `lean-ctx cloud status` — your Personal Cloud, from the terminal. Shows the
/// same privacy-preserving footprint as leanctx.com/account/cloud: per-bucket
/// `lean-ctx cloud autosync <on|off|status>` — toggle the daily background
/// Personal-Cloud push (GL #384). The flag lives in `[cloud] auto_sync`.
fn cmd_cloud_autosync(arg: Option<&str>) {
    let mut config = core::config::Config::load();
    match arg {
        Some("on") => {
            config.cloud.auto_sync = true;
            if let Err(e) = config.save() {
                tracing::error!("Could not save config: {e}");
                std::process::exit(1);
            }
            println!(
                "Auto-sync enabled — your Personal Cloud (knowledge, commands, CEP, gotchas, buddy, feedback)"
            );
            println!(
                "is pushed silently once per day at session end. Requires Pro and an active login."
            );
            if !cloud_client::is_logged_in() {
                println!("Note: you are not logged in yet. Run: lean-ctx login <email>");
            }
        }
        Some("off") => {
            config.cloud.auto_sync = false;
            if let Err(e) = config.save() {
                tracing::error!("Could not save config: {e}");
                std::process::exit(1);
            }
            println!("Auto-sync disabled. Manual sync stays available via: lean-ctx sync");
        }
        Some("status") | None => {
            let state = if config.cloud.auto_sync { "on" } else { "off" };
            println!("Auto-sync: {state}");
            match config.cloud.last_auto_sync.as_deref() {
                Some(date) => println!("Last auto-sync: {date}"),
                None => println!("Last auto-sync: never"),
            }
            if !config.cloud.auto_sync {
                println!("Enable with: lean-ctx cloud autosync on");
            }
        }
        Some(other) => {
            tracing::error!("Unknown autosync action: {other}. Use on|off|status.");
            std::process::exit(1);
        }
    }
}

/// `lean-ctx cloud autoindex <on|off|status>` — toggle the daily background
/// hosted-index push (GL #392). Separate flag from `autosync` because index
/// bundles are megabytes, not kilobytes. The flag lives in `[cloud] auto_index`.
fn cmd_cloud_autoindex(arg: Option<&str>) {
    let mut config = core::config::Config::load();
    match arg {
        Some("on") => {
            config.cloud.auto_index = true;
            if let Err(e) = config.save() {
                tracing::error!("Could not save config: {e}");
                std::process::exit(1);
            }
            println!(
                "Auto-index enabled — this project's encrypted retrieval index is pushed \
                 silently once per day when it changes. Requires Pro and an active login."
            );
            if !cloud_client::is_logged_in() {
                println!("Note: you are not logged in yet. Run: lean-ctx login <email>");
            }
        }
        Some("off") => {
            config.cloud.auto_index = false;
            if let Err(e) = config.save() {
                tracing::error!("Could not save config: {e}");
                std::process::exit(1);
            }
            println!(
                "Auto-index disabled. Manual push stays available via: lean-ctx sync index push"
            );
        }
        Some("status") | None => {
            let state = if config.cloud.auto_index { "on" } else { "off" };
            println!("Auto-index: {state}");
            let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let hash = core::index_namespace::namespace_hash(&root);
            match config.cloud.last_index_push.get(&hash) {
                Some(date) => println!("Last push (this project): {date}"),
                None => println!("Last push (this project): never"),
            }
            if !config.cloud.auto_index {
                println!("Enable with: lean-ctx cloud autoindex on");
            }
        }
        Some(other) => {
            tracing::error!("Unknown autoindex action: {other}. Use on|off|status.");
            std::process::exit(1);
        }
    }
}

/// counts + last sync, buddy, and the all-time usage totals. Free accounts see
/// the connection state plus what upgrading unlocks.
fn cmd_cloud_status() {
    if !cloud_client::is_logged_in() {
        println!("Not connected to LeanCTX Cloud.");
        println!("Get started: lean-ctx login <email>");
        return;
    }
    let email = cloud_client::account_email().unwrap_or_default();
    println!("Connected to LeanCTX Cloud as {email}.");

    let d = match cloud_client::fetch_account_cloud() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("Could not fetch cloud status: {e}");
            return;
        }
    };

    let plan = d.get("plan").and_then(|v| v.as_str()).unwrap_or("free");
    println!("Plan: {plan}");

    if d.get("cloud_sync").and_then(serde_json::Value::as_bool) != Some(true) {
        println!("Personal Cloud sync: locked on this plan.");
        super::upgrade_hint::hint_for("cloud_sync");
        return;
    }

    match d.get("last_synced_at").and_then(|v| v.as_str()) {
        Some(ts) => println!("Last synced: {ts}"),
        None => println!("Last synced: never — run `lean-ctx sync` on this machine."),
    }

    // Mirror the website's bucket order and labels.
    const BUCKETS: [(&str, &str, &str); 6] = [
        ("knowledge", "Knowledge & memory", "facts"),
        ("commands", "Learned shell patterns", "patterns"),
        ("cep", "CEP score history", "snapshots"),
        ("gain", "GAIN score history", "snapshots"),
        ("gotchas", "Gotchas", "fixes"),
        ("feedback", "Feedback thresholds", "languages"),
    ];
    println!("\nSynced to your Personal Cloud:");
    for (key, label, unit) in BUCKETS {
        let count = d
            .get("buckets")
            .and_then(|b| b.get(key))
            .and_then(|b| b.get("count"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        if count > 0 {
            println!("  {label:<24} {count} {unit}");
        } else {
            println!("  {label:<24} —");
        }
    }

    if let Some(buddy) = d
        .get("buddy")
        .filter(|b| b.get("present").and_then(serde_json::Value::as_bool) == Some(true))
    {
        let name = buddy
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Buddy");
        let level = buddy
            .get("level")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(1);
        println!("  {:<24} {name} (level {level})", "Buddy");
    }

    if let Some(totals) = d.get("usage").and_then(|u| u.get("totals")) {
        let tokens = totals
            .get("tokens_saved")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let sessions = totals
            .get("sessions")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        if sessions > 0 {
            println!("\nAll-time: {tokens} tokens saved across {sessions} synced sessions.");
        }
    }
    println!("\nFull dashboard: https://leanctx.com/account/cloud/");
}

/// `lean-ctx cloud pull` — the read side of the Pro "Personal Cloud". `lean-ctx
/// sync` pushes your knowledge to the account; this restores it onto the current
/// machine, so your context follows you across devices. Facts are merged into the
/// current project's local store with skip-existing semantics, so a local fact is
/// never clobbered and re-running is idempotent. A Free account hits the 402 gate
/// and gets the same upgrade hint as `sync`.
fn cmd_cloud_pull() {
    if !cloud_client::is_logged_in() {
        eprintln!("Not logged in. Run: lean-ctx login <email>");
        std::process::exit(1);
    }

    let project_root = std::env::current_dir()
        .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string());

    println!("Pulling knowledge from LeanCTX Cloud...");
    let entries = match cloud_client::pull_knowledge() {
        Ok(e) => e,
        Err(e) if pro_gate_hit(&e) => {
            print_pro_upgrade_hint();
            std::process::exit(1);
        }
        Err(e) => {
            tracing::error!("Pull failed: {e}");
            std::process::exit(1);
        }
    };

    if entries.is_empty() {
        println!(
            "No cloud knowledge to restore yet. Run `lean-ctx sync` on another machine first."
        );
        return;
    }

    let facts = match parse_pulled_knowledge(&entries) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("Could not parse pulled knowledge: {e}");
            std::process::exit(1);
        }
    };

    let policy = match crate::tools::knowledge_shared::load_policy_or_error() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let mut knowledge = core::knowledge::ProjectKnowledge::load_or_create(&project_root);
    let result = knowledge.import_facts(
        facts,
        core::knowledge::ImportMerge::SkipExisting,
        "cloud-pull",
        &policy,
    );

    match knowledge.save() {
        Ok(()) => {
            println!(
                "  Knowledge: {} restored, {} already present (into {project_root})",
                result.added, result.skipped
            );
            println!("Pull complete.");
        }
        Err(e) => {
            tracing::error!("Restored {} facts but save failed: {e}", result.added);
            std::process::exit(1);
        }
    }
}

/// Map the server's `{category, key, value, updated_by, updated_at}` rows onto the
/// import schema (`value` + `source`/`timestamp` provenance) and reuse the
/// battle-tested [`parse_import_data`] importer rather than re-deriving the
/// `KnowledgeFact` shape here.
fn parse_pulled_knowledge(
    entries: &[serde_json::Value],
) -> Result<Vec<core::knowledge::KnowledgeFact>, String> {
    let str_field = |e: &serde_json::Value, k: &str| {
        e.get(k)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let simple: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "category": str_field(e, "category"),
                "key": str_field(e, "key"),
                "value": str_field(e, "value"),
                "source": e.get("updated_by").and_then(serde_json::Value::as_str),
                "timestamp": e.get("updated_at").and_then(serde_json::Value::as_str),
            })
        })
        .collect();
    let data = serde_json::to_string(&simple).map_err(|e| e.to_string())?;
    core::knowledge::parse_import_data(&data)
}

/// `lean-ctx cloud upgrade [--plan pro|team|business] [--interval monthly|yearly]`
/// — start a hosted Stripe Checkout for the logged-in account and print the URL
/// to open. Defaults to Pro monthly (the self-serve Personal Cloud tier).
fn cloud_upgrade(args: &[String]) {
    if !cloud_client::is_logged_in() {
        eprintln!("Not logged in. Run: lean-ctx login <email>");
        std::process::exit(1);
    }
    let (plan, interval) = match parse_upgrade_args(args) {
        Ok(pi) => pi,
        Err(e) => {
            eprintln!("{e}");
            eprintln!(
                "Usage: lean-ctx cloud upgrade [--plan pro|team|business] [--interval monthly|yearly]"
            );
            std::process::exit(1);
        }
    };

    println!("Starting {plan} checkout ({interval})...");
    match cloud_client::start_checkout(&plan, &interval) {
        Ok(url) => {
            println!();
            println!("Open this link to complete your subscription:");
            println!("  {url}");
        }
        Err(e) => {
            tracing::error!("Could not start checkout: {e}");
            std::process::exit(1);
        }
    }
}

/// Parse the optional `--plan` / `--interval` flags for `cloud upgrade`. Defaults
/// are Pro + monthly. Only `pro`/`team`/`business` and `monthly`/`yearly` are
/// accepted; an unknown value is an error (so a typo never silently buys the
/// wrong plan). Enterprise stays sales-assisted and is deliberately absent.
fn parse_upgrade_args(args: &[String]) -> Result<(String, String), String> {
    let mut plan = "pro".to_string();
    let mut interval = "monthly".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--plan" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or("--plan needs a value (pro|team|business)")?;
                if !matches!(v.as_str(), "pro" | "team" | "business") {
                    return Err(format!("unknown plan '{v}' (use pro, team or business)"));
                }
                plan.clone_from(v);
            }
            "--interval" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or("--interval needs a value (monthly|yearly)")?;
                if !matches!(v.as_str(), "monthly" | "yearly") {
                    return Err(format!("unknown interval '{v}' (use monthly or yearly)"));
                }
                interval.clone_from(v);
            }
            "--yearly" => interval = "yearly".to_string(),
            "--monthly" => interval = "monthly".to_string(),
            other => return Err(format!("unknown option '{other}'")),
        }
        i += 1;
    }
    Ok((plan, interval))
}

pub fn cmd_gotchas(args: &[String]) {
    let action = args.first().map_or("list", std::string::String::as_str);
    let project_root = std::env::current_dir()
        .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string());

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
                Err(e) => tracing::error!("Export failed: {e}"),
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

    let action = args.first().map_or("show", std::string::String::as_str);
    let buddy = core::buddy::BuddyState::compute();
    let theme = core::theme::load_theme(&cfg.theme);

    match action {
        "show" | "status" | "stats" => {
            println!("{}", core::buddy::format_buddy_full(&buddy, &theme));
        }
        "ascii" => {
            for line in &buddy.ascii_art {
                println!("  {line}");
            }
        }
        "json" => match serde_json::to_string_pretty(&buddy) {
            Ok(json) => println!("{json}"),
            Err(e) => tracing::error!("JSON error: {e}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pro_gate_hit_detects_402_only() {
        // The server's Pro gate surfaces as a 402 inside the error string.
        assert!(pro_gate_hit(
            "Push failed: http status 402 Payment Required"
        ));
        // Other failures must NOT be treated as the gate (they stay errors).
        assert!(!pro_gate_hit("Push failed: http status 500"));
        assert!(!pro_gate_hit("Push failed: connection refused"));
        assert!(!pro_gate_hit("401 Unauthorized"));
    }

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_string()).collect()
    }

    #[test]
    fn upgrade_args_default_to_pro_monthly() {
        assert_eq!(
            parse_upgrade_args(&[]).unwrap(),
            ("pro".to_string(), "monthly".to_string())
        );
    }

    #[test]
    fn upgrade_args_accept_team_and_yearly() {
        assert_eq!(
            parse_upgrade_args(&s(&["--plan", "team", "--interval", "yearly"])).unwrap(),
            ("team".to_string(), "yearly".to_string())
        );
        // Shorthand cadence flags.
        assert_eq!(
            parse_upgrade_args(&s(&["--yearly"])).unwrap(),
            ("pro".to_string(), "yearly".to_string())
        );
        // Business is self-serve too (GL #533).
        assert_eq!(
            parse_upgrade_args(&s(&["--plan", "business"])).unwrap(),
            ("business".to_string(), "monthly".to_string())
        );
    }

    #[test]
    fn upgrade_args_reject_unknown_values() {
        // A typo'd plan must error, never silently fall back to a purchase.
        assert!(parse_upgrade_args(&s(&["--plan", "enterprise"])).is_err());
        assert!(parse_upgrade_args(&s(&["--interval", "weekly"])).is_err());
        assert!(parse_upgrade_args(&s(&["--plan"])).is_err());
        assert!(parse_upgrade_args(&s(&["--bogus"])).is_err());
    }

    #[test]
    fn parse_pulled_knowledge_maps_server_rows() {
        // The GET /api/sync/knowledge contract: {category, key, value,
        // updated_by, updated_at}. The pull path must map these onto facts and
        // carry provenance (updated_by -> source_session).
        let rows = vec![
            serde_json::json!({
                "category": "architecture",
                "key": "db",
                "value": "PostgreSQL 16 with pgvector",
                "updated_by": "me@example.com",
                "updated_at": "2026-01-02T03:04:05Z"
            }),
            serde_json::json!({
                "category": "decision",
                "key": "auth",
                "value": "JWT RS256"
            }),
        ];
        let facts = parse_pulled_knowledge(&rows).expect("rows must parse");
        assert_eq!(facts.len(), 2);
        assert_eq!(facts[0].category, "architecture");
        assert_eq!(facts[0].key, "db");
        assert_eq!(facts[0].value, "PostgreSQL 16 with pgvector");
        assert_eq!(facts[0].source_session, "me@example.com");
        // Rows without updated_by fall back to the importer's default source.
        assert_eq!(facts[1].value, "JWT RS256");
    }

    #[test]
    fn parse_pulled_knowledge_handles_empty() {
        assert!(parse_pulled_knowledge(&[]).unwrap().is_empty());
    }
}
