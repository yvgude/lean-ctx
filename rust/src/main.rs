use anyhow::Result;
use lean_ctx::{cli, core, dashboard, doctor, shell, tools};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 {
        let rest = args[2..].to_vec();

        match args[1].as_str() {
            "-c" | "exec" => {
                let command = shell_join(&args[2..]);
                if std::env::var("LEAN_CTX_ACTIVE").is_ok() {
                    passthrough(&command);
                }
                let code = shell::exec(&command);
                std::process::exit(code);
            }
            "shell" | "--shell" => {
                shell::interactive();
                return;
            }
            "gain" => {
                if rest.iter().any(|a| a == "--live" || a == "--watch") {
                    core::stats::gain_live();
                    return;
                }
                let output = if rest.iter().any(|a| a == "--graph") {
                    core::stats::format_gain_graph()
                } else if rest.iter().any(|a| a == "--daily") {
                    core::stats::format_gain_daily()
                } else if rest.iter().any(|a| a == "--json") {
                    core::stats::format_gain_json()
                } else {
                    core::stats::format_gain()
                };
                println!("{output}");
                return;
            }
            "cep" => {
                println!("{}", core::stats::format_cep_report());
                return;
            }
            "dashboard" => {
                let port = rest
                    .first()
                    .and_then(|p| p.strip_prefix("--port=").or_else(|| p.strip_prefix("-p=")))
                    .and_then(|p| p.parse().ok());
                run_async(dashboard::start(port));
                return;
            }
            "init" => {
                cli::cmd_init(&rest);
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
            "tee" => {
                cli::cmd_tee(&rest);
                return;
            }
            "doctor" => {
                doctor::run();
                return;
            }
            "--version" | "-V" => {
                println!("lean-ctx 2.3.3");
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

        tracing::info!("lean-ctx v2.3.3 MCP server starting");

        let server = tools::create_server();
        let transport = rmcp::transport::io::stdio();
        let service = server.serve(transport).await?;
        service.waiting().await?;

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
        "lean-ctx 2.3.3 — The Cognitive Filter for AI Engineering

90+ compression patterns | 21 MCP tools | Context Continuity Protocol

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
    dashboard [--port=N]           Open web dashboard (default: http://localhost:3333)
    wrapped [--week|--month|--all] Savings report card (shareable)
    sessions [list|show|cleanup]   Manage CCP sessions (~/.lean-ctx/sessions/)
    benchmark run [path] [--json]  Run real benchmark on project files
    benchmark report [path]        Generate shareable Markdown report
    init [--global]                Install shell aliases (zsh/bash/fish/PowerShell)
    init --agent pi                Install Pi Coding Agent extension (pi-lean-ctx)
    read <file> [-m mode]          Read file with compression
    diff <file1> <file2>           Compressed file diff
    grep <pattern> [path]          Search with compressed output
    find <pattern> [path]          Find files with compressed output
    ls [path]                      Directory listing with compression
    deps [path]                    Show project dependencies
    discover                       Find uncompressed commands in shell history
    session                        Show adoption statistics
    config                         Show/edit configuration (~/.lean-ctx/config.toml)
    tee [list|clear|show <file>]   Manage error log files (~/.lean-ctx/tee/)
    doctor                         Run installation and environment diagnostics

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
    lean-ctx wrapped               Weekly savings report card
    lean-ctx wrapped --month       Monthly savings report card
    lean-ctx sessions list         List all CCP sessions
    lean-ctx sessions show         Show latest session state
    lean-ctx discover              Find missed savings in shell history
    lean-ctx init --global         Install shell aliases
    lean-ctx init --agent pi       Install Pi Coding Agent extension
    lean-ctx doctor                Check PATH, config, MCP, and dashboard port
    lean-ctx read src/main.rs -m map
    lean-ctx grep \"pub fn\" src/
    lean-ctx deps .

WEBSITE: https://leanctx.com
GITHUB:  https://github.com/yvgude/lean-ctx
"
    );
}
