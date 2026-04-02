use anyhow::Result;
use lean_ctx::{
    cli, cloud_client, core, dashboard, doctor, setup, shell, terminal_ui, tools, uninstall,
};

fn main() {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("lean-ctx: unexpected error (your command was not affected)");
        eprintln!("  Disable temporarily: lean-ctx-off");
        eprintln!("  Full uninstall:      lean-ctx uninstall");
        if let Some(msg) = info.payload().downcast_ref::<&str>() {
            eprintln!("  Details: {msg}");
        } else if let Some(msg) = info.payload().downcast_ref::<String>() {
            eprintln!("  Details: {msg}");
        }
        if let Some(loc) = info.location() {
            eprintln!("  Location: {}:{}", loc.file(), loc.line());
        }
    }));

    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 {
        let rest = args[2..].to_vec();

        match args[1].as_str() {
            "-c" | "exec" => {
                let command = if args.len() == 3 {
                    args[2].clone()
                } else {
                    shell_join(&args[2..])
                };
                if std::env::var("LEAN_CTX_ACTIVE").is_ok() {
                    passthrough(&command);
                }
                let code = shell::exec(&command);
                core::stats::flush();
                std::process::exit(code);
            }
            "shell" | "--shell" => {
                shell::interactive();
                return;
            }
            "gain" => {
                if rest.iter().any(|a| a == "--reset") {
                    core::stats::reset_all();
                    println!("Stats reset. All token savings data cleared.");
                    return;
                }
                if rest.iter().any(|a| a == "--live" || a == "--watch") {
                    core::stats::gain_live();
                    return;
                }
                if rest.iter().any(|a| a == "--graph") {
                    println!("{}", core::stats::format_gain_graph());
                } else if rest.iter().any(|a| a == "--daily") {
                    println!("{}", core::stats::format_gain_daily());
                } else if rest.iter().any(|a| a == "--json") {
                    println!("{}", core::stats::format_gain_json());
                } else {
                    print_gain_with_logo();
                }
                return;
            }
            "cep" => {
                println!("{}", core::stats::format_cep_report());
                return;
            }
            "dashboard" => {
                let port = rest
                    .iter()
                    .find_map(|p| p.strip_prefix("--port=").or_else(|| p.strip_prefix("-p=")))
                    .and_then(|p| p.parse().ok());
                let host = rest
                    .iter()
                    .find_map(|p| p.strip_prefix("--host=").or_else(|| p.strip_prefix("-H=")))
                    .map(String::from);
                run_async(dashboard::start(port, host));
                return;
            }
            "init" => {
                cli::cmd_init(&rest);
                return;
            }
            "setup" => {
                setup::run_setup();
                return;
            }
            "read" => {
                cli::cmd_read(&rest);
                return;
            }
            "diff" => {
                cli::cmd_diff(&rest);
                return;
            }
            "grep" => {
                cli::cmd_grep(&rest);
                return;
            }
            "find" => {
                cli::cmd_find(&rest);
                return;
            }
            "ls" => {
                cli::cmd_ls(&rest);
                return;
            }
            "deps" => {
                cli::cmd_deps(&rest);
                return;
            }
            "discover" => {
                cli::cmd_discover(&rest);
                return;
            }
            "session" => {
                cli::cmd_session();
                return;
            }
            "wrapped" => {
                cli::cmd_wrapped(&rest);
                return;
            }
            "sessions" => {
                cli::cmd_sessions(&rest);
                return;
            }
            "benchmark" => {
                cli::cmd_benchmark(&rest);
                return;
            }
            "config" => {
                cli::cmd_config(&rest);
                return;
            }
            "stats" => {
                cli::cmd_stats(&rest);
                return;
            }
            "theme" => {
                cli::cmd_theme(&rest);
                return;
            }
            "tee" => {
                cli::cmd_tee(&rest);
                return;
            }
            "slow-log" => {
                cli::cmd_slow_log(&rest);
                return;
            }
            "update" | "--self-update" => {
                core::updater::run(&rest);
                return;
            }
            "doctor" => {
                doctor::run();
                return;
            }
            "uninstall" => {
                uninstall::run();
                return;
            }
            "cheat" | "cheatsheet" | "cheat-sheet" => {
                cli::cmd_cheatsheet();
                return;
            }
            "login" => {
                cmd_login(&rest);
                return;
            }
            "sync" => {
                cmd_sync();
                return;
            }
            "contribute" => {
                cmd_contribute();
                return;
            }
            "team" => {
                cmd_team(&rest);
                return;
            }
            "cloud" => {
                cmd_cloud(&rest);
                return;
            }
            "upgrade" => {
                cmd_upgrade();
                return;
            }
            "--version" | "-V" => {
                println!("lean-ctx 2.14.0");
                return;
            }
            "--help" | "-h" => {
                print_help();
                return;
            }
            "mcp" => {
                // fall through to MCP server startup below
            }
            _ => {
                eprintln!("lean-ctx: unknown command '{}'\n", args[1]);
                print_help();
                std::process::exit(1);
            }
        }
    }

    if let Err(e) = run_mcp_server() {
        eprintln!("lean-ctx: {e}");
        std::process::exit(1);
    }
}

fn passthrough(command: &str) -> ! {
    let (shell, flag) = shell::shell_and_flag();
    let status = std::process::Command::new(&shell)
        .arg(&flag)
        .arg(command)
        .env("LEAN_CTX_ACTIVE", "1")
        .status()
        .map(|s| s.code().unwrap_or(1))
        .unwrap_or(127);
    std::process::exit(status);
}

fn run_async<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Runtime::new()
        .expect("failed to create async runtime")
        .block_on(future)
}

fn run_mcp_server() -> Result<()> {
    use rmcp::ServiceExt;
    use tracing_subscriber::EnvFilter;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();

        tracing::info!("lean-ctx v2.9.3 MCP server starting");

        let server = tools::create_server();
        let transport = rmcp::transport::io::stdio();
        let service = server.serve(transport).await?;
        service.waiting().await?;

        core::stats::flush();
        core::mode_predictor::ModePredictor::flush();
        core::feedback::FeedbackStore::flush();

        Ok(())
    })
}

fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+%^".contains(&b))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn print_help() {
    println!(
        "lean-ctx 2.14.0 — The Intelligence Layer for AI Coding

90+ compression patterns | 24 MCP tools | Context Continuity Protocol

USAGE:
    lean-ctx                       Start MCP server (stdio)
    lean-ctx -c \"command\"          Execute with compressed output
    lean-ctx exec \"command\"        Same as -c
    lean-ctx shell                 Interactive shell with compression

COMMANDS:
    gain                           Visual dashboard (colors, bars, sparklines, USD)
    gain --live                    Live mode: auto-refreshes every 2s in-place
    gain --graph                   30-day savings chart
    gain --daily                   Bordered day-by-day table with USD
    gain --json                    Raw JSON export of all stats
    cep                            CEP impact report (score trends, cache, modes)
    dashboard [--port=N] [--host=H] Open web dashboard (default: http://localhost:3333)
    wrapped [--week|--month|--all] Savings report card (shareable)
    sessions [list|show|cleanup]   Manage CCP sessions (~/.lean-ctx/sessions/)
    benchmark run [path] [--json]  Run real benchmark on project files
    benchmark report [path]        Generate shareable Markdown report
    cheatsheet                     Command cheat sheet & workflow quick reference
    setup                          One-command setup: shell + editor + verify
    init [--global]                Install shell aliases (zsh/bash/fish/PowerShell)
    init --agent <name>            Configure MCP for specific editor/agent
    read <file> [-m mode]          Read file with compression
    diff <file1> <file2>           Compressed file diff
    grep <pattern> [path]          Search with compressed output
    find <pattern> [path]          Find files with compressed output
    ls [path]                      Directory listing with compression
    deps [path]                    Show project dependencies
    discover                       Find uncompressed commands in shell history
    session                        Show adoption statistics
    config                         Show/edit configuration (~/.lean-ctx/config.toml)
    theme [list|set|export|import] Customize terminal colors and themes
    tee [list|clear|show <file>]   Manage error log files (~/.lean-ctx/tee/)
    slow-log [list|clear]          Show/clear slow command log (~/.lean-ctx/slow-commands.log)
    update [--check]               Self-update lean-ctx binary from GitHub Releases
    doctor                         Run installation and environment diagnostics
    uninstall                      Remove shell hook, MCP configs, and data directory

SHELL HOOK PATTERNS (90+):
    git       status, log, diff, add, commit, push, pull, fetch, clone,
              branch, checkout, switch, merge, stash, tag, reset, remote
    docker    build, ps, images, logs, compose, exec, network
    npm/pnpm  install, test, run, list, outdated, audit
    cargo     build, test, check, clippy
    gh        pr list/view/create, issue list/view, run list/view
    kubectl   get pods/services/deployments, logs, describe, apply
    python    pip install/list/outdated, ruff check/format, poetry, uv
    linters   eslint, biome, prettier, golangci-lint
    builds    tsc, next build, vite build
    ruby      rubocop, bundle install/update, rake test, rails test
    tests     jest, vitest, pytest, go test, playwright, rspec, minitest
    iac       terraform, make, maven, gradle, dotnet, flutter, dart
    utils     curl, grep/rg, find, ls, wget, env
    data      JSON schema extraction, log deduplication

READ MODES:
    full (default)                 Full content (cached re-reads = 13 tokens)
    map                            Dependency graph + API signatures
    signatures                     tree-sitter AST extraction (14 languages)
    aggressive                     Syntax-stripped content
    entropy                        Shannon entropy filtered
    diff                           Changed lines only
    lines:N-M                      Specific line ranges (e.g. lines:10-50,80)

OPTIONS:
    --version, -V                  Show version
    --help, -h                     Show this help

EXAMPLES:
    lean-ctx -c \"git status\"       Compressed git output
    lean-ctx -c \"kubectl get pods\" Compressed k8s output
    lean-ctx -c \"gh pr list\"       Compressed GitHub CLI output
    lean-ctx gain                  Visual terminal dashboard
    lean-ctx gain --live           Live auto-updating terminal dashboard
    lean-ctx gain --graph          30-day savings chart
    lean-ctx gain --daily          Day-by-day breakdown with USD
    lean-ctx dashboard             Open web dashboard at localhost:3333
    lean-ctx dashboard --host=0.0.0.0  Bind to all interfaces (remote access)
    lean-ctx wrapped               Weekly savings report card
    lean-ctx wrapped --month       Monthly savings report card
    lean-ctx sessions list         List all CCP sessions
    lean-ctx sessions show         Show latest session state
    lean-ctx discover              Find missed savings in shell history
    lean-ctx setup                 One-command setup (shell + editors + verify)
    lean-ctx init --global         Install shell aliases (includes lean-ctx-on/off/status)
    lean-ctx-on                    Enable all compression aliases (after init)
    lean-ctx-off                   Disable all compression aliases (human-readable mode)
    lean-ctx-status                Show whether compression is active
    lean-ctx init --agent pi       Install Pi Coding Agent extension
    lean-ctx doctor                Check PATH, config, MCP, and dashboard port
    lean-ctx read src/main.rs -m map
    lean-ctx grep \"pub fn\" src/
    lean-ctx deps .

PRO:
    upgrade                        Upgrade to Pro ($9/mo — adaptive compression)
    cloud status                   Show cloud connection status
    cloud pull-models              Update adaptive models (Pro only)
    login <email>                  Register/login to LeanCTX Cloud
    sync                           Upload local stats to cloud dashboard
    contribute                     Share anonymized compression data

TROUBLESHOOTING:
    Commands broken?     lean-ctx-off             (fixes current session)
    Permanent fix?       lean-ctx uninstall       (removes all hooks)
    Manual fix?          Edit ~/.zshrc, remove the \"lean-ctx shell hook\" block
    Binary missing?      Aliases auto-fallback to original commands (safe)
    Preview init?        lean-ctx init --global --dry-run

WEBSITE: https://leanctx.com
GITHUB:  https://github.com/yvgude/lean-ctx
"
    );
}

fn cmd_login(args: &[String]) {
    let email = match args.first() {
        Some(e) => e.trim().to_lowercase(),
        None => {
            eprintln!("Usage: lean-ctx login <email>");
            std::process::exit(1);
        }
    };

    if !email.contains('@') || !email.contains('.') {
        eprintln!("Invalid email address: {email}");
        std::process::exit(1);
    }

    println!("Registering with LeanCTX Cloud...");
    match cloud_client::register(&email) {
        Ok((api_key, user_id)) => {
            if let Err(e) = cloud_client::save_credentials(&api_key, &user_id, &email) {
                eprintln!("Warning: Could not save credentials: {e}");
                eprintln!("Your API key: {api_key}");
                return;
            }
            println!("Logged in as {email}");
            println!("API key saved to ~/.lean-ctx/cloud/credentials.json");
        }
        Err(e) => {
            eprintln!("Login failed: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_sync() {
    if !cloud_client::is_logged_in() {
        eprintln!("Not logged in. Run: lean-ctx login <email>");
        std::process::exit(1);
    }
    if !cloud_client::check_pro() {
        println!("Stats sync is a Pro feature.\n");
        println!("With Pro, your stats sync to the cloud dashboard");
        println!("so you can track savings over time.\n");
        println!("  lean-ctx upgrade    Upgrade to Pro ($9/month)");
        std::process::exit(0);
    }

    let stats_data = core::stats::format_gain_json();
    let parsed: serde_json::Value = match serde_json::from_str(&stats_data) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Failed to read local stats: {e}");
            std::process::exit(1);
        }
    };

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let entry = serde_json::json!({
        "date": today,
        "tokens_original": parsed["total_original_tokens"].as_i64().unwrap_or(0),
        "tokens_compressed": parsed["total_compressed_tokens"].as_i64().unwrap_or(0),
        "tokens_saved": parsed["total_saved_tokens"].as_i64().unwrap_or(0),
        "tool_calls": parsed["total_calls"].as_i64().unwrap_or(0),
        "cache_hits": parsed["cache_hits"].as_i64().unwrap_or(0),
        "cache_misses": parsed["cache_misses"].as_i64().unwrap_or(0),
    });

    match cloud_client::sync_stats(&[entry]) {
        Ok(msg) => println!("{msg}"),
        Err(e) => {
            eprintln!("Sync failed: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_contribute() {
    let mut entries = Vec::new();

    // Try mode_stats.json first (per-extension, per-size-bucket data from ModePredictor)
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

    // Fall back to stats.json CEP data (aggregated mode counts + overall compression)
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

fn cmd_team(args: &[String]) {
    let action = args.first().map(|s| s.as_str()).unwrap_or("help");

    match action {
        "push" => {
            if !cloud_client::is_logged_in() {
                eprintln!("Not logged in. Run: lean-ctx login <email>");
                std::process::exit(1);
            }
            let knowledge_dir = dirs::home_dir()
                .unwrap_or_default()
                .join(".lean-ctx")
                .join("knowledge");
            if !knowledge_dir.exists() {
                println!("No local knowledge to push.");
                return;
            }

            let mut entries = Vec::new();
            if let Ok(files) = std::fs::read_dir(&knowledge_dir) {
                for entry in files.flatten() {
                    if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
                        if let Ok(content) = std::fs::read_to_string(entry.path()) {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                                let category =
                                    json["category"].as_str().unwrap_or("general").to_string();
                                let key = json["key"].as_str().unwrap_or("").to_string();
                                let value = json["value"].as_str().unwrap_or("").to_string();
                                if !key.is_empty() {
                                    entries.push(serde_json::json!({
                                        "category": category,
                                        "key": key,
                                        "value": value,
                                        "updated_by": "",
                                        "updated_at": "",
                                    }));
                                }
                            }
                        }
                    }
                }
            }

            if entries.is_empty() {
                println!("No knowledge entries to push.");
                return;
            }

            match cloud_client::push_knowledge(&entries) {
                Ok(msg) => println!("{msg}"),
                Err(e) => {
                    eprintln!("Push failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "pull" => {
            if !cloud_client::is_logged_in() {
                eprintln!("Not logged in. Run: lean-ctx login <email>");
                std::process::exit(1);
            }
            match cloud_client::pull_knowledge() {
                Ok(entries) => {
                    if entries.is_empty() {
                        println!("No team knowledge found.");
                        return;
                    }
                    println!("{} team knowledge entries:", entries.len());
                    for e in &entries {
                        let cat = e["category"].as_str().unwrap_or("?");
                        let key = e["key"].as_str().unwrap_or("?");
                        let by = e["updated_by"].as_str().unwrap_or("?");
                        println!("  [{cat}] {key} (by {by})");
                    }
                }
                Err(e) => {
                    eprintln!("Pull failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            println!("Usage: lean-ctx team <push|pull>");
            println!("  push — Upload local knowledge to team cloud");
            println!("  pull — Download team knowledge from cloud");
        }
    }
}

fn cmd_cloud(args: &[String]) {
    let action = args.first().map(|s| s.as_str()).unwrap_or("help");

    match action {
        "pull-models" => {
            if !cloud_client::check_pro() {
                println!("Adaptive models are available with Pro.\n");
                println!("Your AI tools get smarter with Pro — the engine learns");
                println!("from thousands of developers which compression works best.\n");
                println!("  lean-ctx upgrade    Upgrade to Pro ($9/month)");
                return;
            }
            println!("Updating adaptive models...");
            match cloud_client::pull_pro_models() {
                Ok(data) => {
                    let count = data["models"].as_array().map(|a| a.len()).unwrap_or(0);

                    if let Err(e) = cloud_client::save_pro_models(&data) {
                        eprintln!("Warning: Could not save models: {e}");
                        return;
                    }
                    println!("{count} adaptive models updated.");
                    if let Some(est) = data["improvement_estimate"].as_f64() {
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
            if cloud_client::check_pro() {
                println!("Plan: Pro (adaptive models active)");
            } else if cloud_client::is_logged_in() {
                println!("Plan: Free");
                println!("Upgrade for adaptive models: lean-ctx upgrade");
            } else {
                println!("Not connected to LeanCTX Cloud.");
                println!("Get started: lean-ctx upgrade");
            }
        }
        _ => {
            println!("Usage: lean-ctx cloud <command>");
            println!("  pull-models — Update adaptive compression models (Pro)");
            println!("  status      — Show cloud connection status");
        }
    }
}

fn print_gain_with_logo() {
    let t = core::theme::load_theme(&core::config::Config::load().theme);
    terminal_ui::print_logo_animated_themed(&t);

    if let Some(banner) = core::version_check::get_update_banner() {
        println!("{banner}");
        println!();
    }

    animate_kpi_countup(&t);

    let output = core::stats::format_gain();
    print!("{output}");
    let d = core::theme::dim();
    let r = core::theme::rst();
    println!("  {d}lean-ctx v2.14.0  |  leanctx.com  |  lean-ctx dashboard{r}");
    if !cloud_client::check_pro() {
        println!("  {d}Save ~25% more with Pro \u{2192} lean-ctx upgrade{r}");
    }
    println!();

    core::version_check::check_background();
}

fn animate_kpi_countup(t: &core::theme::Theme) {
    use std::io::{IsTerminal, Write};

    if !std::io::stdout().is_terminal() || core::theme::no_color() {
        return;
    }

    let store = core::stats::load();
    if store.total_commands == 0 {
        return;
    }

    let input_saved = store
        .total_input_tokens
        .saturating_sub(store.total_output_tokens);
    let cost_model = core::stats::CostModel::default();
    let cost = cost_model.calculate(&store);
    let total_saved = input_saved + cost.output_tokens_saved;

    let frames = core::theme::animate_countup(total_saved, 10);
    let r = core::theme::rst();
    let b = core::theme::bold();
    let mut stdout = std::io::stdout();

    for (i, frame) in frames.iter().enumerate() {
        if i > 0 {
            print!("\x1b[1A\x1b[K");
        }
        let _ = writeln!(
            stdout,
            "  {c}{b}{frame}{r} tokens saved",
            c = t.success.fg(),
        );
        let _ = stdout.flush();
        if i < frames.len() - 1 {
            std::thread::sleep(std::time::Duration::from_millis(45));
        }
    }
    print!("\x1b[1A\x1b[K");
    let _ = stdout.flush();
}

fn cmd_upgrade() {
    let stats_data = core::stats::format_gain_json();
    let weekly_savings = if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stats_data)
    {
        let total_saved = parsed["total_saved_tokens"].as_f64().unwrap_or(0.0);
        let total_calls = parsed["total_calls"].as_f64().unwrap_or(1.0);
        let days_active = parsed["days_active"].as_f64().unwrap_or(1.0);
        let daily_rate = total_saved / days_active.max(1.0);
        let weekly_usd = daily_rate * 7.0 / 1_000_000.0 * core::stats::DEFAULT_INPUT_PRICE_PER_M;
        let weekly_pro = weekly_usd * 1.25;
        (weekly_usd, weekly_pro, total_calls as i64)
    } else {
        (0.0, 0.0, 0)
    };

    println!();
    println!("  LeanCTX Pro — $9/month");
    println!();
    println!("  Your AI tools get smarter with Pro:");
    println!();
    if weekly_savings.2 > 0 {
        println!("    Your current savings:  ${:.2}/week", weekly_savings.0);
        println!(
            "    Estimated with Pro:    ${:.2}/week (+25%)",
            weekly_savings.1
        );
        println!();
    }
    println!("    \u{2713} Adaptive compression (learns from 2,300+ developers)");
    println!("    \u{2713} Automatic optimization (nothing to configure)");
    println!("    \u{2713} Cloud dashboard (track savings over time)");
    println!("    \u{2713} Cancel anytime — your local engine keeps working");
    println!();

    if cloud_client::check_pro() {
        println!("  You already have Pro! Everything is working automatically.");
        println!("  Dashboard: https://leanctx.com/dashboard");
        return;
    }

    print!("  Enter your email to start: ");
    use std::io::Write;
    std::io::stdout().flush().ok();
    let mut email = String::new();
    if std::io::stdin().read_line(&mut email).is_err() {
        eprintln!("  Could not read input.");
        return;
    }
    let email = email.trim().to_lowercase();

    if !email.contains('@') || !email.contains('.') {
        eprintln!("  Please enter a valid email address.");
        return;
    }

    println!("  Creating account...");
    match cloud_client::register(&email) {
        Ok((api_key, user_id)) => {
            if let Err(e) = cloud_client::save_credentials(&api_key, &user_id, &email) {
                eprintln!("  Warning: Could not save credentials: {e}");
            }
        }
        Err(e) => {
            eprintln!("  {e}");
            return;
        }
    }

    let checkout_url = format!("https://leanctx.com/checkout?email={}", email);
    println!();
    println!("  Opening checkout in your browser...");

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(&checkout_url)
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(&checkout_url)
            .spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", &checkout_url])
            .spawn();
    }

    println!("  If your browser didn't open, visit:");
    println!("  {checkout_url}");
    println!();
    println!("  After payment, Pro activates automatically.");
    println!("  Your engine will start learning from the community.");
}
