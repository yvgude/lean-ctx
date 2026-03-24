use anyhow::Result;

mod cli;
mod core;
mod dashboard;
mod server;
mod shell;
mod tools;

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
            "config" => {
                cli::cmd_config(&rest);
                return;
            }
            "--version" | "-V" => {
                println!("lean-ctx 1.6.0");
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
    let status = std::process::Command::new("/bin/sh")
        .arg("-c")
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

        tracing::info!("lean-ctx v1.6.0 MCP server starting");

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
        "lean-ctx 1.6.0 — Hybrid Context Optimizer with TDD (Shell Hook + MCP Server)

60+ compression patterns | 9 MCP tools | Token Dense Dialect

USAGE:
    lean-ctx                       Start MCP server (stdio)
    lean-ctx -c \"command\"          Execute with compressed output
    lean-ctx exec \"command\"        Same as -c
    lean-ctx shell                 Interactive shell with compression

COMMANDS:
    gain                           Show persistent token savings stats
    gain --graph                   ASCII chart of last 30 days
    gain --daily                   Day-by-day breakdown
    gain --json                    Raw JSON export of all stats
    dashboard [--port=N]           Open web dashboard (default: http://localhost:3333)
    init [--global]                Install shell aliases (.zshrc/.bashrc)
    read <file> [-m mode]          Read file with compression
    diff <file1> <file2>           Compressed file diff
    grep <pattern> [path]          Search with compressed output
    find <pattern> [path]          Find files with compressed output
    ls [path]                      Directory listing with compression
    deps [path]                    Show project dependencies
    discover                       Find uncompressed commands in shell history
    session                        Show adoption statistics
    config                         Show/edit configuration (~/.lean-ctx/config.toml)

SHELL HOOK PATTERNS (60+):
    git       status, log, diff, add, commit, push, pull, fetch, clone,
              branch, checkout, switch, merge, stash, tag, reset, remote
    docker    build, ps, images, logs, compose, exec, network
    npm/pnpm  install, test, run, list, outdated, audit
    cargo     build, test, check, clippy
    gh        pr list/view/create, issue list/view, run list/view
    kubectl   get pods/services/deployments, logs, describe, apply
    python    pip install/list/outdated, ruff check/format
    linters   eslint, biome, prettier, golangci-lint
    builds    tsc, next build, vite build
    ruby      rubocop, bundle install/update, rake test, rails test
    tests     jest, vitest, pytest, go test, playwright, rspec, minitest
    utils     curl, grep/rg, find, ls, wget, env
    data      JSON schema extraction, log deduplication

READ MODES:
    full (default)                 Full content
    map                            Dependency graph + API signatures
    signatures                     Function/class signatures only
    aggressive                     Syntax-stripped content
    entropy                        Shannon entropy filtered
    diff                           Changed lines only

OPTIONS:
    --version, -V                  Show version
    --help, -h                     Show this help

EXAMPLES:
    lean-ctx -c \"git status\"       Compressed git output
    lean-ctx -c \"kubectl get pods\" Compressed k8s output
    lean-ctx -c \"gh pr list\"       Compressed GitHub CLI output
    lean-ctx gain                  Show savings statistics
    lean-ctx gain --graph          ASCII savings chart
    lean-ctx gain --daily          Day-by-day breakdown
    lean-ctx dashboard             Open web dashboard at localhost:3333
    lean-ctx discover              Find missed savings in shell history
    lean-ctx init --global         Install shell aliases
    lean-ctx read src/main.rs -m map
    lean-ctx grep \"pub fn\" src/
    lean-ctx deps .

WEBSITE: https://leanctx.com
GITHUB:  https://github.com/yvgude/lean-ctx
"
    );
}
