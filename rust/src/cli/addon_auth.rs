// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx addon auth <name>` — browser OAuth login for an HTTP MCP server
//! (#1391).
//!
//! Some MCP servers accept only a token from a browser login (OAuth 2.1 with
//! PKCE). This command runs that login once, stores the credentials encrypted,
//! and marks the gateway entry so every later connection attaches the token and
//! refreshes it when it expires.

use crate::core::config::Config;
use crate::core::mcp_catalog::config::TransportKind;
use crate::core::mcp_catalog::oauth;

/// How long to wait for the browser to come back with the code.
const LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(5);

pub(crate) fn cmd_auth(args: &[String]) {
    if let Err(message) = run(args) {
        eprintln!("lean-ctx addon auth: {message}");
        std::process::exit(1);
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let name = args
        .iter()
        .skip_while(|a| a.as_str() != "auth")
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .ok_or("usage: lean-ctx addon auth <name> [--status | --logout] [--no-browser]")?;
    let flag = |f: &str| args.iter().any(|a| a == f);

    let cfg = Config::load();
    let server = cfg
        .gateway
        .servers
        .iter()
        .find(|s| &s.name == name)
        .ok_or_else(|| {
            format!(
                "no gateway server named `{name}` — install the addon first (lean-ctx addon add …)"
            )
        })?
        .clone();
    if server.transport != TransportKind::Http {
        return Err(format!(
            "`{name}` is a stdio server; OAuth applies to HTTP servers only"
        ));
    }
    let url = server.url.trim().to_string();

    if flag("--status") {
        let authorised = oauth::has_credentials(&url)?;
        println!(
            "{name}: {}",
            if authorised {
                "logged in"
            } else {
                "not logged in"
            }
        );
        if authorised {
            println!("  credentials encrypted; key in {}", oauth::key_location());
        }
        return Ok(());
    }

    if flag("--logout") {
        let removed = oauth::forget(&url)?;
        println!(
            "{name}: {}",
            if removed {
                "logged out, stored credentials deleted"
            } else {
                "was not logged in"
            }
        );
        return Ok(());
    }

    let open = !flag("--no-browser");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the async runtime: {e}"))?;
    runtime.block_on(oauth::authorize(
        &url,
        &server.oauth_scopes,
        LOGIN_TIMEOUT,
        |auth_url| {
            println!("Log in to `{name}` in your browser:\n\n  {auth_url}\n");
            if open && open_browser(auth_url).is_err() {
                println!("(Could not open a browser — open the URL above yourself.)");
            }
            println!(
                "Waiting up to {} minutes for the login to finish…",
                LOGIN_TIMEOUT.as_secs() / 60
            );
        },
    ))?;

    if !server.oauth {
        let mut cfg = Config::load();
        if let Some(entry) = cfg.gateway.servers.iter_mut().find(|s| &s.name == name) {
            entry.oauth = true;
        }
        cfg.save().map_err(|e| format!("save config: {e}"))?;
    }
    println!(
        "Logged in to `{name}`. Credentials are stored encrypted; the key is in {}.",
        oauth::key_location()
    );
    println!(
        "The token is refreshed automatically. Log out with: lean-ctx addon auth {name} --logout"
    );
    if !cfg.gateway.enabled {
        println!(
            "Note: the gateway is off — turn it on with `lean-ctx config set gateway.enabled true`."
        );
    }
    Ok(())
}

/// Open `url` in the default browser. Windows goes through `rundll32`, not
/// `cmd /C start`, because cmd splits the command line at every `&` — and an
/// OAuth authorization URL is mostly `&`-separated parameters.
fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}
