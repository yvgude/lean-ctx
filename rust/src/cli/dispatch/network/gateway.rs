//! `lean-ctx gateway …` — the self-hosted org gateway's CLI surface:
//! `serve` (run), `init` (plug-and-play setup), `keys` (identity management),
//! `doctor` (go-live preflight), `report` (printable value report).

/// Extracts `--flag=value` occurrences from args.
fn flag_value<'a>(rest: &'a [String], flag: &str) -> Option<&'a str> {
    let prefix = format!("--{flag}=");
    rest.iter().find_map(|a| a.strip_prefix(prefix.as_str()))
}

/// All `--flag=value` occurrences (repeatable flags like `--person`).
fn flag_values<'a>(rest: &'a [String], flag: &str) -> Vec<&'a str> {
    let prefix = format!("--{flag}=");
    rest.iter()
        .filter_map(|a| a.strip_prefix(prefix.as_str()))
        .collect()
}

fn parse_port(rest: &[String], flag: &str, default: u16) -> u16 {
    flag_value(rest, flag)
        .and_then(|p| p.parse().ok())
        .unwrap_or(default)
}

pub(crate) fn cmd_gateway(rest: &[String]) {
    let sub = rest.first().map_or("help", std::string::String::as_str);
    match sub {
        "serve" => serve(rest),
        "init" => init(&rest[1..]),
        "keys" => keys(&rest[1..]),
        "doctor" => doctor(&rest[1..]),
        "report" => report(&rest[1..]),
        _ => help(),
    }
}

fn serve(rest: &[String]) {
    let port = rest
        .iter()
        .find_map(|p| p.strip_prefix("--port="))
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(crate::proxy_setup::default_port);
    let admin_port: Option<u16> = flag_value(rest, "admin-port").and_then(|p| p.parse().ok());
    let opts = crate::gateway_server::serve::ServeOptions { port, admin_port };
    if let Err(e) = crate::cli::dispatch::run_async(crate::gateway_server::serve::serve(opts)) {
        tracing::error!("Gateway error: {e}");
        std::process::exit(1);
    }
}

fn init(rest: &[String]) {
    let dir = rest
        .iter()
        .find(|a| !a.starts_with("--"))
        .map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from);
    let opts = crate::gateway_server::init::InitOptions {
        org_label: flag_value(rest, "org").unwrap_or_default().to_string(),
        seats: flag_value(rest, "seats").and_then(|s| s.parse().ok()),
        reference_model: flag_value(rest, "reference-model").map(str::to_string),
        persons: flag_values(rest, "person")
            .into_iter()
            .map(str::to_string)
            .collect(),
        proxy_port: parse_port(rest, "port", 8484),
        admin_port: parse_port(rest, "admin-port", 8485),
    };
    match crate::gateway_server::init::run(&dir, &opts) {
        Ok(outcome) => {
            println!(
                "\x1b[32m✓\x1b[0m Gateway instance created in {}",
                dir.display()
            );
            println!();
            for f in &outcome.files {
                println!("    {f}");
            }
            if !outcome.person_keys.is_empty() {
                println!();
                println!(
                    "  Personal keys (shown ONCE — hand them out now, only hashes are stored):"
                );
                for (person, key) in &outcome.person_keys {
                    println!("    {person:<32} {key}");
                }
            }
            println!();
            println!("  Next steps:");
            println!("    cd {} && docker compose up -d", dir.display());
            println!("    lean-ctx gateway doctor --dir {}", dir.display());
            println!(
                "    open http://127.0.0.1:{}/   (admin console — token in .env)",
                opts.admin_port
            );
        }
        Err(e) => {
            eprintln!("\x1b[31m✗\x1b[0m gateway init failed: {e:#}");
            std::process::exit(1);
        }
    }
}

fn keys_file(rest: &[String]) -> std::path::PathBuf {
    flag_value(rest, "file").map_or_else(
        crate::proxy::gateway_identity::GatewayKeys::default_path,
        std::path::PathBuf::from,
    )
}

fn keys(rest: &[String]) {
    let action = rest.first().map_or("list", std::string::String::as_str);
    let path = keys_file(rest);
    match action {
        "add" => {
            let Some(person) = flag_value(rest, "person") else {
                eprintln!(
                    "Usage: lean-ctx gateway keys add --person=<id> [--team=..] [--project=..] [--file=..] [--allow-multiple]"
                );
                std::process::exit(2);
            };
            let allow_multiple = rest.iter().any(|a| a == "--allow-multiple");
            match crate::gateway_server::keys_cli::add_key(
                &path,
                person,
                flag_value(rest, "team"),
                flag_value(rest, "project"),
                allow_multiple,
            ) {
                Ok(key) => {
                    println!(
                        "\x1b[32m✓\x1b[0m Key created for {person} in {}",
                        path.display()
                    );
                    println!();
                    println!("  {key}");
                    println!();
                    println!("  Shown ONCE — only the SHA-256 hash is stored.");
                    println!("  Client setup: ANTHROPIC_AUTH_TOKEN={key}");
                    println!("  Reload a running gateway: docker compose restart gateway");
                }
                Err(e) => {
                    eprintln!("\x1b[31m✗\x1b[0m {e:#}");
                    std::process::exit(1);
                }
            }
        }
        "list" => match crate::gateway_server::keys_cli::list_keys(&path) {
            Ok(entries) if entries.is_empty() => {
                println!("No keys in {} — add one:", path.display());
                println!("  lean-ctx gateway keys add --person=alice@example.com");
            }
            Ok(entries) => {
                println!("{} identities in {}:", entries.len(), path.display());
                for e in entries {
                    println!(
                        "  {:<32} team={:<14} project={:<16} sha256:{}…",
                        e.person,
                        e.team.as_deref().unwrap_or("—"),
                        e.default_project.as_deref().unwrap_or("—"),
                        e.sha_prefix
                    );
                }
            }
            Err(e) => {
                eprintln!("\x1b[31m✗\x1b[0m {e:#}");
                std::process::exit(1);
            }
        },
        "revoke" => {
            let Some(person) = flag_value(rest, "person") else {
                eprintln!("Usage: lean-ctx gateway keys revoke --person=<id> [--file=..]");
                std::process::exit(2);
            };
            match crate::gateway_server::keys_cli::revoke_keys(&path, person) {
                Ok(0) => println!("No keys for {person} in {}.", path.display()),
                Ok(removed) => {
                    println!("\x1b[32m✓\x1b[0m Revoked {removed} key(s) of {person}.");
                    println!("  Reload a running gateway: docker compose restart gateway");
                }
                Err(e) => {
                    eprintln!("\x1b[31m✗\x1b[0m {e:#}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            println!(
                "Usage: lean-ctx gateway keys <add|list|revoke> [--person=..] [--team=..] [--project=..] [--file=..]"
            );
        }
    }
}

fn doctor(rest: &[String]) {
    let dir = flag_value(rest, "dir")
        .map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from);
    let proxy_port = parse_port(rest, "port", 8484);
    let admin_port = parse_port(rest, "admin-port", 8485);
    let results = match crate::cli::dispatch::run_async(async {
        anyhow::Ok(crate::gateway_server::doctor::run_checks(&dir, proxy_port, admin_port).await)
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\x1b[31m✗\x1b[0m doctor failed: {e:#}");
            std::process::exit(1);
        }
    };
    println!("lean-ctx gateway doctor — {}", dir.display());
    println!();
    for r in &results {
        println!("{r}");
    }
    println!();
    if crate::gateway_server::doctor::has_failures(&results) {
        println!("\x1b[31mNot go-live ready — fix the FAIL items above.\x1b[0m");
        std::process::exit(1);
    }
    println!("\x1b[32mGo-live ready (warnings are optional improvements).\x1b[0m");
}

fn report(rest: &[String]) {
    let Ok(database_url) = std::env::var(crate::gateway_server::serve::DATABASE_URL_ENV) else {
        eprintln!(
            "\x1b[31m✗\x1b[0m {} not set — the report reads usage_events (Postgres).",
            crate::gateway_server::serve::DATABASE_URL_ENV
        );
        eprintln!(
            "  Compose instance: export $(grep DATABASE_URL .env) and replace host with 127.0.0.1"
        );
        std::process::exit(2);
    };
    let to = flag_value(rest, "to")
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map_or_else(chrono::Utc::now, |d| d.with_timezone(&chrono::Utc));
    let from = flag_value(rest, "from")
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map_or_else(
            || to - chrono::Duration::days(30),
            |d| d.with_timezone(&chrono::Utc),
        );
    let out_path = flag_value(rest, "out")
        .unwrap_or("lean-ctx-report.html")
        .to_string();

    let cfg = crate::core::config::Config::load();
    let meta = crate::gateway_server::report::ReportMeta {
        org_label: cfg.gateway_server.org_label.clone(),
        seats: cfg.gateway_server.seats,
        reference_model: cfg.proxy.baseline.reference_model.clone(),
    };
    let html = crate::cli::dispatch::run_async(async move {
        let pool = crate::gateway_server::store::pool_from_database_url(&database_url)?;
        crate::gateway_server::report::generate(&pool, from, to, &meta).await
    });
    match html {
        Ok(html) => {
            if let Err(e) = std::fs::write(&out_path, html) {
                eprintln!("\x1b[31m✗\x1b[0m write {out_path}: {e}");
                std::process::exit(1);
            }
            println!("\x1b[32m✓\x1b[0m Report written: {out_path}");
            println!("  Window: {} → {}", from.to_rfc3339(), to.to_rfc3339());
            println!("  Print to PDF from any browser for the CTO deck.");
        }
        Err(e) => {
            eprintln!("\x1b[31m✗\x1b[0m report failed: {e:#}");
            std::process::exit(1);
        }
    }
}

fn help() {
    use crate::gateway_server::serve::{ADMIN_TOKEN_ENV, DATABASE_URL_ENV};
    println!("lean-ctx gateway — self-hosted org gateway (proxy + usage store + admin console)");
    println!();
    println!("Usage:");
    println!("  lean-ctx gateway serve  [--port=8484] [--admin-port=8485]");
    println!("  lean-ctx gateway init   [dir] [--org=\"Acme AG\"] [--seats=800]");
    println!("                          [--reference-model=claude-opus-4.5] [--person=a@x …]");
    println!(
        "  lean-ctx gateway keys   <add|list|revoke> [--person=..] [--team=..] [--project=..] [--file=..]"
    );
    println!("  lean-ctx gateway doctor [--dir=.] [--port=8484] [--admin-port=8485]");
    println!("  lean-ctx gateway report [--from=ISO] [--to=ISO] [--out=report.html]");
    println!();
    println!("Environment:");
    println!(
        "  {DATABASE_URL_ENV}     Postgres for usage_events (off → metering disabled, fail-open)"
    );
    println!("  {ADMIN_TOKEN_ENV}  Bearer token of the admin console (off → admin disabled)");
    println!("  LEAN_CTX_PROXY_TOKEN            proxy Bearer token (else session token)");
    println!();
    println!("Config ([gateway_server] in config.toml): seats, org_label, admin_url.");
    println!(
        "Bind host/allowlist/rate limit: proxy_bind_host, proxy_allowed_hosts, proxy_max_rps."
    );
}
