//! `lean-ctx provider …` — provider scaffolding, sync, OAuth.

/// Reads a `--data-source <id>` / `--data-source=<id>` flag, defaulting to "jira".
fn data_source_flag(args: &[String]) -> String {
    args.iter()
        .enumerate()
        .find_map(|(i, a)| {
            if let Some(v) = a.strip_prefix("--data-source=") {
                return Some(v.to_string());
            }
            if a == "--data-source" {
                return args.get(i + 1).cloned();
            }
            None
        })
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "jira".to_string())
}

fn slugify_id(raw: &str) -> Option<String> {
    let id: String = raw
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if id.is_empty() { None } else { Some(id) }
}

fn provider_usage() {
    println!(
        "Usage: lean-ctx provider <command>\n\n\
         Commands:\n  \
         init <id> [--force] [--project-root <path>]\n  \
                                            Scaffold a config provider in <project>/.lean-ctx/providers/\n  \
         auth jira [--data-source <id>]     Connect a Jira Cloud site via OAuth 2.0 (3LO)\n  \
         logout jira [--data-source <id>]   Remove stored Jira OAuth credentials\n  \
         list                               List connected Jira OAuth data sources\n  \
         credentials set                    Store NAME=value lines from stdin (tokens never in argv)\n  \
         credentials remove <NAME>...       Remove stored values\n  \
         credentials list [--json]          Names of stored values (never the values)\n  \
         check <github|gitlab|jira|postgres> [--json] [--stdin]\n  \
                                            Sign in once and report the account; --stdin tests\n  \
                                            candidate NAME=value lines instead of stored settings\n\n\
         Stored values fill in for environment variables an agent host does not pass\n\
         to the Engine; a variable that is set always wins.\n\n\
         Jira OAuth requires your own Atlassian app credentials in the environment:\n  \
         JIRA_OAUTH_CLIENT_ID, JIRA_OAUTH_CLIENT_SECRET\n  \
         (optional) JIRA_OAUTH_SCOPES — default: \"read:jira-work read:jira-user offline_access\"\n\n\
         Register a free app at https://developer.atlassian.com/console/myapps/"
    );
}

/// Arguments that are not flags and not the value of a flag that takes one.
fn positionals(args: &[String]) -> Vec<&str> {
    const VALUED: &[&str] = &["--project-root", "--root", "--data-source"];
    let mut out = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
        } else if VALUED.contains(&arg.as_str()) {
            skip = true;
        } else if !arg.starts_with('-') {
            out.push(arg.as_str());
        }
    }
    out
}

/// The folder `provider init` writes into: `<root>/.lean-ctx/providers` for an
/// explicit `--project-root`/`--root` (resolved like every project command,
/// refused when broad or missing), else the current directory as before.
fn providers_dir(
    args: &[String],
) -> Result<(std::path::PathBuf, Option<std::path::PathBuf>), String> {
    use crate::core::providers::scaffold;

    let explicit = args.iter().enumerate().find_map(|(i, a)| {
        a.strip_prefix("--project-root=")
            .or_else(|| a.strip_prefix("--root="))
            .map(str::to_string)
            .or_else(|| {
                (a == "--project-root" || a == "--root")
                    .then(|| args.get(i + 1).cloned().unwrap_or_default())
            })
    });
    let Some(value) = explicit else {
        return Ok((std::path::PathBuf::from(scaffold::PROVIDERS_SUBDIR), None));
    };
    // An empty value must not silently fall back to the current directory.
    if value.trim().is_empty() || value.starts_with("--") {
        return Err("--project-root needs a path".to_string());
    }
    let root = std::path::PathBuf::from(crate::cli::common::detect_project_root(args));
    if !root.is_dir() {
        return Err(format!("{} is not a folder", root.display()));
    }
    if crate::core::pathutil::is_broad_or_unsafe_root(&root) {
        return Err(format!("{} is not a project", root.display()));
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", root.display()))?;
    Ok((root.join(scaffold::PROVIDERS_SUBDIR), Some(root)))
}

/// Writes the scaffold for `args` and returns its path.
fn write_provider_scaffold(args: &[String]) -> Result<std::path::PathBuf, String> {
    use crate::core::providers::scaffold;

    let force = args.iter().any(|a| a == "--force" || a == "-f");
    let Some(raw) = positionals(args).first().copied() else {
        return Err(
            "Usage: lean-ctx provider init <id> [--force] [--project-root <path>]".to_string(),
        );
    };
    // Provider ids share the addon slug shape (`[a-z0-9-]`).
    let Some(id) = slugify_id(raw) else {
        return Err(format!("`{raw}` has no usable id characters ([a-z0-9-])."));
    };

    let (dir, root) = providers_dir(args)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Error creating {}: {e}", dir.display()))?;
    // A symlinked `.lean-ctx` must not carry the write outside the project.
    if let Some(root) = root {
        let real = dir
            .canonicalize()
            .map_err(|e| format!("cannot resolve {}: {e}", dir.display()))?;
        if !real.starts_with(&root) {
            return Err(format!(
                "{} leads outside {}",
                dir.display(),
                root.display()
            ));
        }
    }
    let path = dir.join(format!("{id}.toml"));
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(format!(
            "{} is a symbolic link; not writing through it",
            path.display()
        ));
    }
    if path.exists() && !force {
        return Err(format!(
            "{} already exists. Re-run with --force.",
            path.display()
        ));
    }
    std::fs::write(&path, scaffold::provider_config(&id))
        .map_err(|e| format!("Error writing {}: {e}", path.display()))?;
    Ok(path)
}

/// `provider init <id>` — scaffold a config-provider TOML in the project-local
/// `.lean-ctx/providers/` directory the discovery layer auto-loads (P4 DX).
fn provider_init(args: &[String]) {
    let path = match write_provider_scaffold(args) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let id = path
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    println!("✓ Wrote {} (provider `{id}`).", path.display());
    println!("\nNext:");
    println!("  1. Edit base_url, [auth] and [resources] for your API.");
    println!("  2. Export the token env var referenced under [auth].");
    println!(
        "  3. It is auto-discovered — query it via ctx_provider / ctx_search action=semantic."
    );
}

/// `provider credentials set|remove|list` — the Engine's store for source
/// tokens and settings. Values arrive on stdin and are never printed.
fn provider_credentials(args: &[String]) {
    use crate::core::providers::credentials;

    let json = args.iter().any(|a| a == "--json");
    let words = positionals(args);
    match words.first().copied().unwrap_or("list") {
        "set" => match read_stdin_bounded().and_then(|input| store_pairs(&input)) {
            Ok(names) => println!("\x1b[32m✓\x1b[0m Stored {}.", names.join(", ")),
            Err(e) => {
                eprintln!("\x1b[31m✗\x1b[0m {e}");
                std::process::exit(1);
            }
        },
        "remove" | "rm" => {
            let names: Vec<String> = words[1..].iter().map(|s| (*s).to_string()).collect();
            if names.is_empty() {
                eprintln!("Usage: lean-ctx provider credentials remove <NAME>...");
                std::process::exit(1);
            }
            match credentials::remove(&names) {
                Ok(count) => println!("Removed {count} stored value(s)."),
                Err(e) => {
                    eprintln!("\x1b[31m✗\x1b[0m {e}");
                    std::process::exit(1);
                }
            }
        }
        "list" | "ls" => println!("{}", credentials_list_text(json)),
        _ => {
            eprintln!("Usage: lean-ctx provider credentials <set|remove <NAME>...|list [--json]>");
            std::process::exit(1);
        }
    }
}

/// `NAME=value` lines from stdin, refused above
/// [`credentials::MAX_INPUT_BYTES`](crate::core::providers::credentials::MAX_INPUT_BYTES).
fn read_stdin_bounded() -> Result<String, String> {
    use crate::core::providers::credentials::MAX_INPUT_BYTES;
    use std::io::Read;

    let mut input = String::new();
    std::io::stdin()
        .take(MAX_INPUT_BYTES as u64 + 1)
        .read_to_string(&mut input)
        .map_err(|_| "stdin is not NAME=value text".to_string())?;
    if input.len() > MAX_INPUT_BYTES {
        return Err(format!("stdin holds more than {MAX_INPUT_BYTES} bytes"));
    }
    Ok(input)
}

fn store_pairs(input: &str) -> Result<Vec<String>, String> {
    use crate::core::providers::credentials;

    let pairs = credentials::parse_pairs(input)?;
    if pairs.is_empty() {
        return Err("no NAME=value line on stdin".to_string());
    }
    credentials::set(&pairs)
}

/// `credentials list`: names only, never values.
fn credentials_list_text(json: bool) -> String {
    let names = crate::core::providers::credentials::stored_names();
    if json {
        serde_json::json!({ "stored": names }).to_string()
    } else if names.is_empty() {
        "No stored source values.".to_string()
    } else {
        format!("Stored source values: {}", names.join(", "))
    }
}

/// Runs `provider check <id> [--stdin]`. With `--stdin`, `stdin` holds the
/// candidate `NAME=value` lines; they are the only values the check sees and
/// are removed again before this returns.
fn run_check(
    args: &[String],
    stdin: Option<&str>,
) -> Result<crate::core::providers::connection::ConnectionCheck, String> {
    use crate::core::providers::{connection, credentials};

    let Some(id) = positionals(args).first().copied() else {
        return Err(format!(
            "Usage: lean-ctx provider check <{}> [--json] [--stdin]",
            connection::CHECKABLE.join("|")
        ));
    };
    let Some(input) = stdin else {
        return Ok(connection::check(id, connection::CHECK_TIMEOUT));
    };
    credentials::set_candidates(id, &credentials::parse_pairs(input)?)?;
    let result = connection::check(id, connection::CHECK_TIMEOUT);
    credentials::clear_candidates();
    Ok(result)
}

/// `provider check <id>` — one authenticated request with the current
/// settings, or with candidate values from stdin (`--stdin`).
fn provider_check(args: &[String]) {
    use crate::core::providers::connection::ConnectionCheck;

    let json = args.iter().any(|a| a == "--json");
    let stdin = if args.iter().any(|a| a == "--stdin") {
        match read_stdin_bounded() {
            Ok(input) => Some(input),
            Err(e) => {
                eprintln!("\x1b[31m✗\x1b[0m {e}");
                std::process::exit(1);
            }
        }
    } else {
        None
    };
    let result = match run_check(args, stdin.as_deref()) {
        Ok(result) => result,
        // Input errors name settings, never values; JSON callers get one line.
        Err(e) if json => ConnectionCheck {
            provider: positionals(args)
                .first()
                .copied()
                .unwrap_or_default()
                .to_string(),
            connected: false,
            account: None,
            scope: None,
            error: Some(e),
            latency_ms: 0,
        },
        Err(e) => {
            eprintln!("\x1b[31m✗\x1b[0m {e}");
            std::process::exit(1);
        }
    };
    if json {
        println!("{}", serde_json::to_string(&result).unwrap_or_default());
    } else if result.connected {
        println!(
            "\x1b[32m✓\x1b[0m {} connected as {}{} ({} ms)",
            result.provider,
            result.account.as_deref().unwrap_or("?"),
            result
                .scope
                .as_deref()
                .map(|s| format!(" · {s}"))
                .unwrap_or_default(),
            result.latency_ms
        );
    } else {
        println!(
            "\x1b[31m✗\x1b[0m {} not connected: {}",
            result.provider,
            result.error.as_deref().unwrap_or("unknown error")
        );
    }
    // With --json the answer is the report; a failed check is not a failed command.
    if !result.connected && !json {
        std::process::exit(2);
    }
}

pub(crate) fn cmd_provider(rest: &[String]) {
    use crate::core::providers::jira_oauth;

    let sub = rest.first().map_or("help", std::string::String::as_str);
    match sub {
        "init" | "new" => provider_init(&rest[1..]),
        "credentials" | "creds" => provider_credentials(&rest[1..]),
        "check" | "test" => provider_check(&rest[1..]),
        "auth" | "login" | "connect" => {
            let target = rest.get(1).map_or("", std::string::String::as_str);
            if !target.eq_ignore_ascii_case("jira") {
                eprintln!("Only 'jira' is supported for OAuth today.\n");
                provider_usage();
                std::process::exit(1);
            }
            let args: &[String] = if rest.len() > 2 { &rest[2..] } else { &[] };
            let data_source = data_source_flag(args);
            match jira_oauth::run_auth_flow(&data_source) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("\x1b[31m✗\x1b[0m Jira OAuth failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "logout" | "disconnect" => {
            let target = rest.get(1).map_or("", std::string::String::as_str);
            if !target.eq_ignore_ascii_case("jira") {
                provider_usage();
                std::process::exit(1);
            }
            let args: &[String] = if rest.len() > 2 { &rest[2..] } else { &[] };
            let data_source = data_source_flag(args);
            match jira_oauth::remove_credential(&data_source) {
                Ok(true) => {
                    println!(
                        "\x1b[32m✓\x1b[0m Removed Jira OAuth credentials for '{data_source}'."
                    );
                }
                Ok(false) => {
                    println!("No stored Jira OAuth credentials for '{data_source}'.");
                }
                Err(e) => {
                    eprintln!("\x1b[31m✗\x1b[0m {e}");
                    std::process::exit(1);
                }
            }
        }
        "list" | "ls" | "status" => {
            let conns = jira_oauth::list_connections();
            if conns.is_empty() {
                println!("No Jira OAuth data sources connected. Run: lean-ctx provider auth jira");
            } else {
                println!("Connected Jira OAuth data sources:");
                for c in conns {
                    println!("  • {c}");
                }
            }
        }
        _ => provider_usage(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::providers::credentials;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /// Runs `body` with a private data directory (the credential store).
    fn with_data_dir<T>(body: impl FnOnce() -> T) -> T {
        let _lock = crate::core::data_dir::test_env_lock();
        let data = tempfile::tempdir().unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.path());
        let result = body();
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        result
    }

    #[test]
    fn check_with_stdin_never_reports_the_candidate_secret() {
        const SECRET: &str = concat!("gh", "p_LeakCanary0123456789abcdefABCDEF");
        with_data_dir(|| {
            // The token also sits in the API address, so a client error that
            // quotes the URL would carry it into the report.
            let stdin =
                format!("GITHUB_TOKEN={SECRET}\nGITHUB_API_URL=http://127.0.0.1:9/{SECRET}\n");
            let result = run_check(&args(&["github", "--json", "--stdin"]), Some(&stdin)).unwrap();
            let line = serde_json::to_string(&result).unwrap();
            assert!(!result.connected, "{line}");
            assert!(result.error.is_some(), "{line}");
            assert!(!line.contains(SECRET), "{line}");
            // Candidates live only for the check.
            assert!(!credentials::candidates_active());

            // A value in a name position is refused without being echoed.
            let error = run_check(
                &args(&["github", "--stdin"]),
                Some(&format!("{SECRET}=x\n")),
            )
            .unwrap_err();
            assert!(!error.contains(SECRET), "{error}");
        });
    }

    #[test]
    fn credentials_list_prints_names_never_values() {
        with_data_dir(|| {
            store_pairs("GITLAB_TOKEN=glpat-StoredValue123\nGITLAB_HOST=git.example\n").unwrap();
            let json: serde_json::Value =
                serde_json::from_str(&credentials_list_text(true)).unwrap();
            assert_eq!(
                json,
                serde_json::json!({"stored": ["GITLAB_HOST", "GITLAB_TOKEN"]})
            );
            for text in [credentials_list_text(true), credentials_list_text(false)] {
                assert!(!text.contains("glpat-StoredValue123") && !text.contains("git.example"));
            }
            // Unknown or malformed names are refused and nothing is stored.
            assert!(store_pairs("HOME=/tmp\n").is_err());
            assert!(store_pairs("gitlab_token=x\n").is_err());
            assert_eq!(
                credentials::stored_names(),
                vec!["GITLAB_HOST", "GITLAB_TOKEN"]
            );
        });
    }

    #[test]
    fn init_with_project_root_writes_there_not_in_cwd() {
        let root = tempfile::tempdir().unwrap();
        let root_arg = root.path().to_string_lossy().to_string();
        let id = "wp-project-root-probe";
        for form in [
            args(&["--project-root", &root_arg, id]),
            args(&[id, &format!("--project-root={root_arg}"), "--force"]),
        ] {
            let path = write_provider_scaffold(&form).unwrap();
            assert_eq!(
                path,
                root.path()
                    .canonicalize()
                    .unwrap()
                    .join(".lean-ctx/providers")
                    .join(format!("{id}.toml"))
            );
            assert!(path.is_file());
        }
        let cwd_file = std::env::current_dir()
            .unwrap()
            .join(".lean-ctx/providers")
            .join(format!("{id}.toml"));
        assert!(!cwd_file.exists());
        // A missing project folder is refused, not created.
        let missing = root.path().join("absent");
        let missing = missing.to_string_lossy().to_string();
        assert!(write_provider_scaffold(&args(&[id, "--project-root", &missing])).is_err());
        assert!(!root.path().join("absent").exists());
    }
}
