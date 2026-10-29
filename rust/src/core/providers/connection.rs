// SPDX-License-Identifier: Apache-2.0

//! A real connection check for one built-in source: the same configuration a
//! provider builds, one authenticated read-only request, and who the source
//! says the Engine is. Used by `lean-ctx provider check <id>`.

use std::time::{Duration, Instant};

use super::config::GitLabConfig;
use super::github::GitHubConfig;
use super::jira::JiraConfig;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConnectionCheck {
    pub provider: String,
    pub connected: bool,
    /// The account the source reports for these credentials.
    pub account: Option<String>,
    /// What the source will be read for (repository, project, database).
    pub scope: Option<String>,
    pub error: Option<String>,
    pub latency_ms: u64,
}

pub const CHECKABLE: &[&str] = &["github", "gitlab", "jira", "postgres"];

/// Upper bound for one check, below the Cockpit's 40 s command deadline.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// Checks `provider` within `timeout`. Every secret the check could see
/// (tokens, the database address and its password) is masked in `account`,
/// `scope` and `error`, whatever the source or its client echoed back.
pub fn check(provider: &str, timeout: Duration) -> ConnectionCheck {
    let started = Instant::now();
    let (sender, receiver) = std::sync::mpsc::channel();
    let id = provider.to_string();
    let spawned = std::thread::Builder::new()
        .name("provider-check".into())
        .spawn(move || {
            let _ = sender.send(check_now(&id));
        });
    let result = match spawned {
        Ok(_) => receiver.recv_timeout(timeout).unwrap_or_else(|_| {
            failed(
                provider,
                format!("no answer within {} s", timeout.as_secs()),
                started,
            )
        }),
        Err(e) => failed(provider, format!("cannot start the check: {e}"), started),
    };
    redact(result, &secret_values())
}

fn failed(provider: &str, error: String, started: Instant) -> ConnectionCheck {
    ConnectionCheck {
        provider: provider.to_string(),
        connected: false,
        account: None,
        scope: None,
        error: Some(error),
        latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

/// The secret values a check resolves, longest first so a token that
/// contains another secret is masked whole.
fn secret_values() -> Vec<String> {
    let mut values = Vec::new();
    for name in super::credentials::SECRET_NAMES {
        if let Ok(value) = super::credentials::var(name) {
            let value = value.trim().to_string();
            if let Some(password) = url_password(&value) {
                values.push(password);
            }
            values.push(value);
        }
    }
    // Very short values would mask ordinary words; real secrets are longer.
    values.retain(|value| value.len() >= 4);
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    values
}

/// The password of `scheme://user:password@host/…`, if any.
fn url_password(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split('/').next()?;
    let (userinfo, _) = authority.rsplit_once('@')?;
    let (_, password) = userinfo.split_once(':')?;
    (!password.is_empty()).then(|| password.to_string())
}

fn mask(text: &str, secrets: &[String]) -> String {
    secrets.iter().fold(text.to_string(), |text, secret| {
        text.replace(secret.as_str(), "[redacted]")
    })
}

fn redact(mut result: ConnectionCheck, secrets: &[String]) -> ConnectionCheck {
    result.account = result.account.map(|value| mask(&value, secrets));
    result.scope = result.scope.map(|value| mask(&value, secrets));
    // Errors may quote a response body: also apply the generic secret rules.
    result.error = result
        .error
        .map(|value| crate::core::redaction::redact_text(&mask(&value, secrets)));
    result
}

fn check_now(provider: &str) -> ConnectionCheck {
    let started = Instant::now();
    let outcome = match provider {
        "github" => github(),
        "gitlab" => gitlab(),
        "jira" => jira(),
        "postgres" => postgres(),
        other => Err(format!(
            "{other} cannot be checked here; use one of {}",
            CHECKABLE.join(", ")
        )),
    };
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match outcome {
        Ok((account, scope)) => ConnectionCheck {
            provider: provider.to_string(),
            connected: true,
            account: Some(account),
            scope,
            error: None,
            latency_ms,
        },
        Err(error) => ConnectionCheck {
            provider: provider.to_string(),
            connected: false,
            account: None,
            scope: None,
            error: Some(error),
            latency_ms,
        },
    }
}

fn field(body: &str, keys: &[&str]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_str))
        .map(str::to_string)
}

fn github() -> Result<(String, Option<String>), String> {
    let config = GitHubConfig::from_env()?;
    let token = format!("Bearer {}", config.token);
    let body = super::hardened_http::provider_get_with_headers(
        "github",
        &format!("{}/user", config.api_base),
        &[
            ("Authorization", &token),
            ("Accept", "application/vnd.github+json"),
            ("X-GitHub-Api-Version", "2022-11-28"),
        ],
    )
    .into_body()
    .map_err(|e| format!("GitHub refused the token: {e}"))?;
    let login = field(&body, &["login"]).unwrap_or_else(|| "signed in".to_string());
    Ok((login, config.repo_slug()))
}

fn gitlab() -> Result<(String, Option<String>), String> {
    let config = GitLabConfig::from_env()?;
    let body = super::hardened_http::provider_get_with_headers(
        "gitlab",
        &config.api_url("/user"),
        &[("PRIVATE-TOKEN", &config.token)],
    )
    .into_body()
    .map_err(|e| format!("GitLab at {} refused the token: {e}", config.host))?;
    let user = field(&body, &["username", "name"]).unwrap_or_else(|| "signed in".to_string());
    Ok((user, config.project_path.clone()))
}

fn jira() -> Result<(String, Option<String>), String> {
    let config = JiraConfig::from_env()?;
    let user = config.whoami()?;
    Ok((user, config.project.clone()))
}

/// The check's `psql` call; the connection travels only in `PG*` variables.
fn postgres_command() -> Result<std::process::Command, String> {
    if !super::credentials::is_set("DATABASE_URL") && !super::credentials::is_set("PGDATABASE") {
        return Err("DATABASE_URL not set".to_string());
    }
    let mut command = super::postgres::psql_command()?;
    command
        .args([
            "-X",
            "-w",
            "-t",
            "-A",
            "-c",
            "SELECT current_user || '|' || current_database();",
        ])
        .env("PGCLIENTENCODING", "UTF8")
        .stdin(std::process::Stdio::null());
    Ok(command)
}

fn postgres() -> Result<(String, Option<String>), String> {
    let mut command = postgres_command()?;
    let out = super::postgres::run_psql(&mut command, std::time::Duration::from_secs(10))?;
    let line = out.lines().next().unwrap_or("").trim();
    let (user, database) = line.split_once('|').unwrap_or((line, ""));
    Ok((
        user.to_string(),
        (!database.is_empty()).then(|| database.to_string()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_sources_are_refused_without_a_request() {
        let result = check("weather", CHECK_TIMEOUT);
        assert!(!result.connected);
        assert!(result.error.unwrap().contains("github"));
    }

    #[test]
    fn database_password_is_masked_in_every_reported_field() {
        use super::super::credentials;
        let _lock = crate::core::data_dir::test_env_lock();
        let url = concat!("postgres://reader:", "Pw-Secret9", "@db.example/app");
        credentials::set_candidates("postgres", &[("DATABASE_URL".into(), url.into())]).unwrap();
        let secrets = secret_values();
        credentials::clear_candidates();
        let result = redact(
            ConnectionCheck {
                provider: "postgres".into(),
                connected: false,
                account: Some("reader:Pw-Secret9".into()),
                scope: Some("app".into()),
                error: Some(format!("cannot reach {url}: password Pw-Secret9 refused")),
                latency_ms: 1,
            },
            &secrets,
        );
        let line = serde_json::to_string(&result).unwrap();
        assert!(!line.contains("Pw-Secret9"), "{line}");
        assert_eq!(result.scope.as_deref(), Some("app"));
    }

    #[test]
    fn psql_argv_never_carries_the_database_address() {
        use super::super::credentials;
        let _lock = crate::core::data_dir::test_env_lock();
        let url = concat!(
            "postgresql://reader:",
            "Pw-Secret9%40x",
            "@db.example:6543/app?sslmode=require"
        );
        credentials::set_candidates("postgres", &[("DATABASE_URL".into(), url.into())]).unwrap();
        let command = postgres_command();
        credentials::clear_candidates();
        let command = command.unwrap();
        for arg in command.get_args() {
            let arg = arg.to_string_lossy();
            assert!(
                !arg.contains("Pw-Secret9") && !arg.contains("db.example"),
                "{arg}"
            );
        }
        let env: std::collections::BTreeMap<String, String> = command
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        for (key, value) in [
            ("PGPASSWORD", "Pw-Secret9@x"),
            ("PGUSER", "reader"),
            ("PGHOST", "db.example"),
            ("PGPORT", "6543"),
            ("PGDATABASE", "app"),
            ("PGSSLMODE", "require"),
        ] {
            assert_eq!(env.get(key).map(String::as_str), Some(value), "{key}");
        }
    }

    #[test]
    fn account_fields_are_read_from_the_response() {
        assert_eq!(
            field(r#"{"login":"octo","name":"Octo"}"#, &["login"]).as_deref(),
            Some("octo")
        );
        assert_eq!(
            field(r#"{"name":"Ada"}"#, &["username", "name"]).as_deref(),
            Some("Ada")
        );
        assert_eq!(field("not json", &["login"]), None);
    }
}
