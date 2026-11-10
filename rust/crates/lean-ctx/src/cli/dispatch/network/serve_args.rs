// SPDX-License-Identifier: Apache-2.0
//! The arguments of `lean-ctx serve`, parsed before anything starts.
//!
//! Every flag is in [`SERVE_FLAGS`], and [`SERVE_USAGE`] lists each of them.
//! An unknown flag, a stray argument, a missing value or a value that is not
//! a number where one is needed is an error, and `serve` then exits 2 having
//! started nothing. Earlier builds skipped what they did not know and started
//! an HTTP server on port 8080, so a flag from a newer contract
//! (`--print-socket` on 3.12.1) opened a listener instead of failing.

use crate::http_server::HttpServerConfig;

/// Whether a flag takes a value, and which kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arity {
    Switch,
    Text,
    Number,
}

/// Every flag `serve` takes: its names (long first) and its arity.
const SERVE_FLAGS: &[(&[&str], Arity)] = &[
    (&["--help", "-h"], Arity::Switch),
    (&["--daemon", "-d"], Arity::Switch),
    (&["--stop"], Arity::Switch),
    (&["--status"], Arity::Switch),
    (&["--print-socket"], Arity::Switch),
    (&["--_foreground-daemon"], Arity::Switch),
    (&["--host", "-H"], Arity::Text),
    (&["--port", "-p"], Arity::Number),
    (&["--project-root"], Arity::Text),
    (&["--root"], Arity::Text),
    (&["--rrf-k"], Arity::Number),
    (&["--auth-token-file"], Arity::Text),
    (&["--auth-token"], Arity::Text),
    (&["--a2a-signing-key-file"], Arity::Text),
    (&["--a2a-signing-key"], Arity::Text),
    (&["--a2a-recipient-id"], Arity::Text),
    (&["--a2a-tenant-id"], Arity::Text),
    (&["--a2a-project-id"], Arity::Text),
    (&["--a2a-task-authority"], Arity::Text),
    (&["--a2a-peers"], Arity::Text),
    (&["--stateful"], Arity::Switch),
    (&["--stateless"], Arity::Switch),
    (&["--json"], Arity::Switch),
    (&["--sse"], Arity::Switch),
    (&["--max-body-bytes"], Arity::Number),
    (&["--max-concurrency"], Arity::Number),
    (&["--max-rps"], Arity::Number),
    (&["--rate-burst"], Arity::Number),
    (&["--request-timeout-ms"], Arity::Number),
    (&["--allowed-host"], Arity::Text),
    (&["--disable-host-check"], Arity::Switch),
];

pub(super) const SERVE_USAGE: &str = "\
Usage: lean-ctx serve [--host H] [--port N] [--project-root DIR] [--daemon] [--stop] [--status]

Starts the MCP server over Streamable HTTP. An unknown flag or argument is an
error (exit 2) and starts nothing. Flags with a value also take --flag=VALUE.

Options:
  --help, -h            Show this help; starts nothing
  --daemon, -d          Start as background daemon (UDS)
  --stop                Stop the daemon of the active data dir (LEAN_CTX_DATA_DIR, else
                        the default); exit 1 and nothing stopped when none runs there
  --status              Show the status of the active data dir's daemon
  --print-socket        Print the daemon socket path (--json: socket, listening, pid); starts nothing
  --_foreground-daemon  Internal: the daemon process --daemon starts
  --host, -H            Bind host (default: 127.0.0.1)
  --port, -p            Bind port (default: 8080)
  --project-root        Resolve relative paths against this root (default: cwd)
  --root PATH[:ALIAS]   Add a repo root for multi-repo mode (repeatable; ALIAS only with --root=)
  --rrf-k N             RRF fusion parameter (default: 60.0)
  --a2a-signing-key-file  File with the key that verifies remote A2A envelope HMACs (must differ
                        from auth token; env LEAN_CTX_A2A_SIGNING_KEY_FILE, or the key in LEAN_CTX_A2A_SIGNING_KEY)
  --a2a-signing-key     Deprecated: the key itself, visible in the process list
  --a2a-recipient-id    Bind incoming A2A envelopes to this server identity
  --a2a-tenant-id       Require signed tenant scope on incoming A2A envelopes
  --a2a-project-id      Require signed project scope on incoming A2A envelopes
  --a2a-task-authority  JSON peer/grant trust policy for signed A2A tasks (env LEAN_CTX_A2A_TASK_AUTHORITY)
  --a2a-peers           JSON bounded multi-peer relay table (env LEAN_CTX_A2A_PEERS)
  --auth-token-file     File with the token Authorization: Bearer <token> must carry (required
                        for non-loopback binds; env LEAN_CTX_HTTP_TOKEN_FILE, or the token in LEAN_CTX_HTTP_TOKEN)
  --auth-token          Deprecated: the token itself, visible in the process list
  --stateful/--stateless  Streamable HTTP session mode (default: stateless)
  --json/--sse          Response framing in stateless mode (default: json)
  --max-body-bytes      Max request body size in bytes (default: 2097152)
  --max-concurrency     Max concurrent requests (default: 32)
  --max-rps             Max requests/sec (global, default: 50)
  --rate-burst          Rate limiter burst (global, default: 100)
  --request-timeout-ms  REST tool-call timeout (default: 30000)
  --allowed-host        Add allowed Host header (repeatable)
  --disable-host-check  Disable Host header validation (unsafe)";

/// What `serve` was asked to do.
#[derive(Debug)]
pub(super) struct ServeArgs {
    pub help: bool,
    pub cfg: HttpServerConfig,
    pub daemon_mode: bool,
    pub stop_mode: bool,
    pub status_mode: bool,
    pub print_socket: bool,
    /// `--json` was given (also the `--print-socket` document form).
    pub json: bool,
    pub foreground_daemon: bool,
    pub multi_roots: Vec<(String, Option<String>)>,
    pub rrf_k: Option<f64>,
    pub a2a_task_authority_path: Option<String>,
    pub a2a_peers_path: Option<String>,
    pub a2a_signing_key_file: Option<String>,
    pub auth_token_file: Option<String>,
}

fn number<T: std::str::FromStr>(flag: &str, raw: &str) -> Result<T, String> {
    raw.parse::<T>()
        .map_err(|_| format!("{flag} needs a number, got {raw:?}"))
}

/// Parse `rest` (the arguments after `serve`). `--help` anywhere wins and
/// skips every other check, so asking for help never fails.
pub(super) fn parse(rest: &[String]) -> Result<ServeArgs, String> {
    let mut out = ServeArgs {
        help: false,
        cfg: HttpServerConfig::default(),
        daemon_mode: false,
        stop_mode: false,
        status_mode: false,
        print_socket: false,
        json: false,
        foreground_daemon: false,
        multi_roots: Vec::new(),
        rrf_k: None,
        a2a_task_authority_path: None,
        a2a_peers_path: None,
        a2a_signing_key_file: None,
        auth_token_file: None,
    };
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        out.help = true;
        return Ok(out);
    }
    let mut args = rest.iter();
    while let Some(arg) = args.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if arg.starts_with("--") => (name, Some(value)),
            _ => (arg.as_str(), None),
        };
        let Some(&(names, arity)) = SERVE_FLAGS.iter().find(|(names, _)| names.contains(&name))
        else {
            return Err(if arg.starts_with('-') {
                format!("unknown flag {name}")
            } else {
                format!("unexpected argument {arg:?}")
            });
        };
        let canonical = names[0];
        let value = match (arity, inline) {
            (Arity::Switch, None) => String::new(),
            (Arity::Switch, Some(_)) => return Err(format!("{name} takes no value")),
            (_, Some(value)) if !value.is_empty() => value.to_string(),
            (_, Some(_)) => return Err(format!("{name} needs a value")),
            (_, None) => args
                .next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))?,
        };
        let cfg = &mut out.cfg;
        match canonical {
            "--daemon" => out.daemon_mode = true,
            "--stop" => out.stop_mode = true,
            "--status" => out.status_mode = true,
            "--print-socket" => out.print_socket = true,
            "--_foreground-daemon" => out.foreground_daemon = true,
            "--host" => cfg.host = value,
            "--port" => cfg.port = number(name, &value)?,
            "--project-root" => cfg.project_root = std::path::PathBuf::from(value),
            "--root" => out.multi_roots.push(match (inline, value.split_once(':')) {
                // The alias form is `--root=PATH:ALIAS` only, so a
                // separate value keeps a drive letter (`C:\repo`) whole.
                (Some(_), Some((path, alias))) => (path.to_string(), Some(alias.to_string())),
                _ => (value, None),
            }),
            "--rrf-k" => out.rrf_k = Some(number(name, &value)?),
            "--auth-token-file" => out.auth_token_file = Some(value),
            "--auth-token" => cfg.auth_token = Some(value),
            "--a2a-signing-key-file" => out.a2a_signing_key_file = Some(value),
            "--a2a-signing-key" => cfg.a2a_signing_key = Some(value),
            "--a2a-recipient-id" => cfg.a2a_recipient_id = Some(value),
            "--a2a-tenant-id" => cfg.a2a_tenant_id = Some(value),
            "--a2a-project-id" => cfg.a2a_project_id = Some(value),
            "--a2a-task-authority" => out.a2a_task_authority_path = Some(value),
            "--a2a-peers" => out.a2a_peers_path = Some(value),
            "--stateful" => cfg.stateful_mode = true,
            "--stateless" => cfg.stateful_mode = false,
            "--json" => {
                cfg.json_response = true;
                out.json = true;
            }
            "--sse" => cfg.json_response = false,
            "--max-body-bytes" => cfg.max_body_bytes = number(name, &value)?,
            "--max-concurrency" => cfg.max_concurrency = number(name, &value)?,
            "--max-rps" => cfg.max_rps = number(name, &value)?,
            "--rate-burst" => cfg.rate_burst = number(name, &value)?,
            "--request-timeout-ms" => cfg.request_timeout_ms = number(name, &value)?,
            "--allowed-host" => cfg.allowed_hosts.push(value),
            "--disable-host-check" => cfg.disable_host_check = true,
            other => return Err(format!("{other} is listed but not handled")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /// Every flag is in the usage and is handled (a valid value parses),
    /// so the table, the usage and the handler cannot drift apart.
    #[test]
    fn every_flag_is_documented_and_handled() {
        for (names, arity) in SERVE_FLAGS {
            for name in *names {
                assert!(
                    SERVE_USAGE.contains(name),
                    "{name} missing from serve --help"
                );
                let mut call = vec![(*name).to_string()];
                if *arity != Arity::Switch {
                    call.push("1".into());
                }
                parse(&call).unwrap_or_else(|e| panic!("{name}: {e}"));
            }
        }
    }

    /// Unknown flags, stray arguments, missing and bad values are errors;
    /// `--help` anywhere wins; known flags keep their meaning.
    #[test]
    fn unknown_and_malformed_arguments_are_errors() {
        for (bad, why) in [
            (&["--print-sockets"][..], "unknown flag --print-sockets"),
            (&["--port", "9000", "--bogus=1"], "unknown flag --bogus"),
            (&["-x"], "unknown flag -x"),
            (&["start"], "unexpected argument \"start\""),
            (&["--port"], "--port needs a value"),
            (
                &["--port", "eighty"],
                "--port needs a number, got \"eighty\"",
            ),
            (&["--port=70000"], "--port needs a number, got \"70000\""),
            (&["--host="], "--host needs a value"),
            (&["--daemon=yes"], "--daemon takes no value"),
            (&["--rrf-k", "x"], "--rrf-k needs a number, got \"x\""),
        ] {
            assert_eq!(parse(&args(bad)).unwrap_err(), why, "{bad:?}");
        }
        assert!(parse(&args(&["--bogus", "--help"])).unwrap().help);

        let ok = parse(&args(&[
            "--port=9001",
            "-H",
            "0.0.0.0",
            "--root=/srv/a:alpha",
            "--root",
            "C:\\repo",
            "--print-socket",
            "--json",
        ]))
        .unwrap();
        assert_eq!((ok.cfg.port, ok.cfg.host.as_str()), (9001, "0.0.0.0"));
        assert_eq!(
            ok.multi_roots,
            vec![
                ("/srv/a".to_string(), Some("alpha".to_string())),
                ("C:\\repo".to_string(), None)
            ]
        );
        assert!(ok.print_socket && ok.json && !ok.help);
    }
}
