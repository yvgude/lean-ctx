use anyhow::Result;
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;

mod cli;
mod core;
mod dashboard;
mod server;
mod shell;
mod tools;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 {
        let rest = args[2..].to_vec();

        match args[1].as_str() {
            "-c" => {
                let command = shell_join(&args[2..]);
                let code = shell::exec(&command);
                std::process::exit(code);
            }
            "exec" => {
                let command = shell_join(&args[2..]);
                let code = shell::exec(&command);
                std::process::exit(code);
            }
            "shell" | "--shell" => {
                shell::interactive();
                return Ok(());
            }
            "gain" => {
                println!("{}", core::stats::format_gain());
                return Ok(());
            }
            "dashboard" => {
                let port = rest.first()
                    .and_then(|p| p.strip_prefix("--port=").or_else(|| p.strip_prefix("-p=")))
                    .and_then(|p| p.parse().ok());
                dashboard::start(port).await;
                return Ok(());
            }
            "init" => {
                cli::cmd_init(&rest);
                return Ok(());
            }
            "read" => {
                cli::cmd_read(&rest);
                return Ok(());
            }
            "diff" => {
                cli::cmd_diff(&rest);
                return Ok(());
            }
            "grep" => {
                cli::cmd_grep(&rest);
                return Ok(());
            }
            "find" => {
                cli::cmd_find(&rest);
                return Ok(());
            }
            "ls" => {
                cli::cmd_ls(&rest);
                return Ok(());
            }
            "deps" => {
                cli::cmd_deps(&rest);
                return Ok(());
            }
            "--version" | "-V" => {
                println!("lean-ctx 1.3.1");
                return Ok(());
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
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

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("lean-ctx v1.3.1 MCP server starting");

    let server = tools::create_server();
    let transport = rmcp::transport::io::stdio();
    let service = server.serve(transport).await?;
    service.waiting().await?;

    Ok(())
}

fn shell_join(args: &[String]) -> String {
    args.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")
}

fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+%^".contains(&b)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn print_help() {
    println!(
        "lean-ctx 1.3.1 — Hybrid Context Optimizer with TDD (Shell Hook + MCP Server)

USAGE:
    lean-ctx                       Start MCP server (stdio)
    lean-ctx -c \"command\"          Execute with compressed output
    lean-ctx exec \"command\"        Same as -c
    lean-ctx shell                 Interactive shell with compression

COMMANDS:
    gain                           Show persistent token savings stats
    dashboard [--port=N]           Open web dashboard (default: http://localhost:3333)
    init [--global]                Install shell aliases (.zshrc/.bashrc)
    read <file> [-m mode]          Read file with compression
    diff <file1> <file2>           Compressed file diff
    grep <pattern> [path]          Search with compressed output
    find <pattern> [path]          Find files with compressed output
    ls [path]                      Directory listing with compression
    deps [path]                    Show project dependencies

READ MODES:
    full (default)                 Full content
    map                            Dependency graph + API signatures
    signatures                     Function/class signatures only
    aggressive                     Syntax-stripped content
    entropy                        Shannon entropy filtered

OPTIONS:
    --version, -V                  Show version
    --help, -h                     Show this help

EXAMPLES:
    lean-ctx -c \"git status\"       Compressed git output
    lean-ctx gain                  Show savings statistics
    lean-ctx dashboard             Open web dashboard at localhost:3333
    lean-ctx init --global         Install shell aliases
    lean-ctx read src/main.rs -m map
    lean-ctx grep \"pub fn\" src/
    lean-ctx find \"*.rs\" src/
    lean-ctx deps .
"
    );
}
