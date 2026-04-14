use anyhow::Result;
use lean_ctx::{
    cli, cloud_client, core, dashboard, doctor, heatmap, hook_handlers, mcp_stdio, report, setup,
    shell, terminal_ui, tools, tui, uninstall,
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
                let raw = rest.first().map(|a| a == "--raw").unwrap_or(false);
                let cmd_args = if raw { &args[3..] } else { &args[2..] };
                let command = if cmd_args.len() == 1 {
                    cmd_args[0].clone()
                } else {
                    shell::join_command(cmd_args)
                };
                if std::env::var("LEAN_CTX_ACTIVE").is_ok()
                    || std::env::var("LEAN_CTX_DISABLED").is_ok()
                {
                    passthrough(&command);
                }
                if raw {
                    std::env::set_var("LEAN_CTX_RAW", "1");
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
            "watch" => {
                if let Err(e) = tui::run() {
                    eprintln!("TUI error: {e}");
                    std::process::exit(1);
                }
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
            "filter" => {
                cli::cmd_filter(&rest);
                return;
            }
            "heatmap" => {
                heatmap::cmd_heatmap(&rest);
                return;
            }
            "graph" => {
                let mut action = "build";
                let mut path_arg: Option<&str> = None;
                for arg in &rest {
                    if arg == "build" {
                        action = "build";
                    } else {
                        path_arg = Some(arg.as_str());
                    }
                }
                let root = path_arg
                    .map(String::from)
                    .or_else(|| {
                        std::env::current_dir()
                            .ok()
                            .map(|p| p.to_string_lossy().to_string())
                    })
                    .unwrap_or_else(|| ".".to_string());
                match action {
                    "build" => {
                        let index = core::graph_index::load_or_build(&root);
                        println!(
                            "Graph built: {} files, {} edges",
                            index.files.len(),
                            index.edges.len()
                        );
                    }
                    _ => {
                        eprintln!("Usage: lean-ctx graph [build] [path]");
                    }
                }
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
            "cache" => {
                cli::cmd_cache(&rest);
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
            "gotchas" | "bugs" => {
                cmd_gotchas(&rest);
                return;
            }
            "buddy" | "pet" => {
                cmd_buddy(&rest);
                return;
            }
            "hook" => {
                let action = rest.first().map(|s| s.as_str()).unwrap_or("help");
                match action {
                    "rewrite" => hook_handlers::handle_rewrite(),
                    "redirect" => hook_handlers::handle_redirect(),
                    _ => {
                        eprintln!("Usage: lean-ctx hook <rewrite|redirect>");
                        eprintln!("  Internal commands used by agent hooks (Claude, Cursor, etc.)");
                        std::process::exit(1);
                    }
                }
                return;
            }
            "report-issue" | "report" => {
                report::run(&rest);
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
            "cloud" => {
                cmd_cloud(&rest);
                return;
            }
            "upgrade" => {
                cmd_upgrade();
                return;
            }
            "--version" | "-V" => {
                println!("lean-ctx {}", env!("CARGO_PKG_VERSION"));
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

    std::env::set_var("LEAN_CTX_MCP_SERVER", "1");

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();

        tracing::info!(
            "lean-ctx v{} MCP server starting",
            env!("CARGO_PKG_VERSION")
        );

        let server = tools::create_server();
        let transport =
            mcp_stdio::HybridStdioTransport::new_server(tokio::io::stdin(), tokio::io::stdout());
        let service = server.serve(transport).await?;
        service.waiting().await?;

        core::stats::flush();
        core::mode_predictor::ModePredictor::flush();
        core::feedback::FeedbackStore::flush();

        Ok(())
    })
}

fn print_help() {
    println!(
        "lean-ctx {version} — Context Runtime for AI Agents

90+ compression patterns | 42 MCP tools | Context Continuity Protocol

USAGE:
    lean-ctx                       Start MCP server (stdio)
    lean-ctx -c \"command\"          Execute with compressed output
    lean-ctx -c --raw \"command\"    Execute without compression (full output)
    lean-ctx exec \"command\"        Same as -c
    lean-ctx shell                 Interactive shell with compression

COMMANDS:
    gain                           Visual dashboard (colors, bars, sparklines, USD)
    gain --live                    Live mode: auto-refreshes every 1s in-place
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
    filter [list|validate|init]    Manage custom compression filters (~/.lean-ctx/filters/)
    session                        Show adoption statistics
    config                         Show/edit configuration (~/.lean-ctx/config.toml)
    theme [list|set|export|import] Customize terminal colors and themes
    tee [list|clear|show <file>|last] Manage output tee files (~/.lean-ctx/tee/)
    slow-log [list|clear]          Show/clear slow command log (~/.lean-ctx/slow-commands.log)
    update [--check]               Self-update lean-ctx binary from GitHub Releases
    gotchas [list|clear|export|stats] Bug Memory: view/manage auto-detected error patterns
    buddy [show|stats|ascii|json]  Token Guardian: your data-driven coding companion
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
    signatures                     tree-sitter AST extraction (18 languages)
    aggressive                     Syntax-stripped content
    entropy                        Shannon entropy filtered
    diff                           Changed lines only
    lines:N-M                      Specific line ranges (e.g. lines:10-50,80)

ENVIRONMENT:
    LEAN_CTX_DISABLED=1            Bypass ALL compression + prevent shell hook from loading
    LEAN_CTX_ENABLED=0             Prevent shell hook auto-start (lean-ctx-on still works)
    LEAN_CTX_RAW=1                 Same as --raw for current command
    LEAN_CTX_AUTONOMY=false        Disable autonomous features
    LEAN_CTX_COMPRESS=1            Force compression (even for excluded commands)

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

CLOUD:
    cloud status                   Show cloud connection status
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
",
        version = env!("CARGO_PKG_VERSION"),
    );
}

fn cmd_login(args: &[String]) {
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

    if email.is_empty() {
        eprintln!("Usage: lean-ctx login <email> [--password <password>]");
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

    println!("Connecting to LeanCTX Cloud...");

    let result = {
        let login_result = cloud_client::login(&email, &pw);
        match &login_result {
            Ok(_) => login_result,
            Err(e) if e.contains("403") => {
                eprintln!("Please verify your email first. Check your inbox.");
                std::process::exit(1);
            }
            Err(e) if e.contains("Invalid email or password") => login_result,
            Err(_) => cloud_client::register(&email, Some(&pw)),
        }
    };

    match result {
        Ok(r) => {
            if let Err(e) = cloud_client::save_credentials(&r.api_key, &r.user_id, &email) {
                eprintln!("Warning: Could not save credentials: {e}");
                let masked = if r.api_key.len() > 4 {
                    format!("{}…{}", &r.api_key[..4], &r.api_key[r.api_key.len() - 4..])
                } else {
                    "****".to_string()
                };
                eprintln!("Your API key (masked): {masked}");
                return;
            }
            if let Ok(plan) = cloud_client::fetch_plan() {
                let _ = cloud_client::save_plan(&plan);
            }
            println!("Logged in as {email}");
            println!("API key saved to ~/.lean-ctx/cloud/credentials.json");
            if r.verification_sent {
                println!("Verification email sent — please check your inbox.");
            }
            if !r.email_verified {
                println!("Note: Your email is not yet verified.");
            }
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
    let mut entries = Vec::new();
    let cep = &store.cep;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let mut cep_cache_by_day: std::collections::HashMap<String, (u64, u64)> =
        std::collections::HashMap::new();
    for s in &cep.scores {
        if let Some(date) = s.timestamp.get(..10) {
            let entry = cep_cache_by_day.entry(date.to_string()).or_default();
            let calls = s.tool_calls.max(1);
            let hits = (calls as f64 * s.cache_hit_rate as f64 / 100.0).round() as u64;
            entry.0 += calls;
            entry.1 += hits;
        }
    }

    for day in &store.daily {
        let tokens_original = day.input_tokens;
        let tokens_compressed = day.output_tokens;
        let tokens_saved = tokens_original.saturating_sub(tokens_compressed);
        let (day_calls, day_hits) = cep_cache_by_day.get(&day.date).copied().unwrap_or((0, 0));
        let cache_hits = day_hits;
        let cache_misses = day_calls.saturating_sub(day_hits);
        entries.push(serde_json::json!({
            "date": day.date,
            "tokens_original": tokens_original,
            "tokens_compressed": tokens_compressed,
            "tokens_saved": tokens_saved,
            "tool_calls": day.commands,
            "cache_hits": cache_hits,
            "cache_misses": cache_misses,
        }));
    }

    let has_today = entries.iter().any(|e| e["date"].as_str() == Some(&today));
    if !has_today && (cep.total_tokens_original > 0 || store.total_commands > 0) {
        entries.push(serde_json::json!({
            "date": today,
            "tokens_original": cep.total_tokens_original,
            "tokens_compressed": cep.total_tokens_compressed,
            "tokens_saved": cep.total_tokens_original.saturating_sub(cep.total_tokens_compressed),
            "tool_calls": store.total_commands,
            "cache_hits": cep.total_cache_hits,
            "cache_misses": cep.total_cache_reads.saturating_sub(cep.total_cache_hits),
        }));
    }

    entries
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

fn cmd_cloud(args: &[String]) {
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
    println!(
        "  {d}lean-ctx v{}  |  leanctx.com  |  lean-ctx dashboard{r}",
        env!("CARGO_PKG_VERSION")
    );
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

fn cmd_gotchas(args: &[String]) {
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

fn cmd_buddy(args: &[String]) {
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

fn cmd_upgrade() {
    println!("'upgrade' has been renamed to 'update'. Running 'lean-ctx update' instead.\n");
    core::updater::run(&[]);
}
