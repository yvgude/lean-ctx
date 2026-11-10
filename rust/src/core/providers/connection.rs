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
    /// Why the check failed, machine-readable (`None` when connected).
    pub reason: Option<FailureReason>,
    pub latency_ms: u64,
}

/// Why a provider check or call failed. Stable snake_case strings; readers
/// show an unknown value as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    /// A required setting or credential is missing.
    NotConfigured,
    /// The source refused the credentials (HTTP 401/403, a database login).
    Auth,
    /// The source could not be reached (DNS, connection refused, TLS).
    Unreachable,
    Timeout,
    RateLimited,
    NotFound,
    /// The source answered with a server error (HTTP 5xx).
    Unavailable,
    /// This source cannot be checked or called here.
    Unsupported,
    /// The request itself was malformed (bad arguments or input).
    InvalidInput,
    Error,
}

impl FailureReason {
    /// The reason of an HTTP status.
    pub fn of_status(status: u16) -> Self {
        match status {
            401 | 403 => Self::Auth,
            404 => Self::NotFound,
            408 => Self::Timeout,
            429 => Self::RateLimited,
            500..=599 => Self::Unavailable,
            _ => Self::Error,
        }
    }

    /// The reason a client's error text names, for sources whose clients
    /// only report text (Jira, `psql`).
    pub fn of_message(message: &str) -> Self {
        let m = message.to_ascii_lowercase();
        let has = |needles: &[&str]| needles.iter().any(|n| m.contains(n));
        if has(&[
            "not set",
            "not configured",
            "missing",
            "no such file",
            "not found in path",
        ]) {
            Self::NotConfigured
        } else if has(&[
            "http 401",
            "http 403",
            "unauthorized",
            "forbidden",
            "authentication failed",
            "refused the token",
        ]) {
            Self::Auth
        } else if has(&["timeout", "timed out", "no answer within"]) {
            Self::Timeout
        } else if has(&["http 429", "rate limit"]) {
            Self::RateLimited
        } else if has(&["http 404"]) {
            Self::NotFound
        } else if has(&["http 5"]) {
            Self::Unavailable
        } else if has(&[
            "could not connect",
            "connection refused",
            "dns",
            "resolve",
            "unreachable",
            "tls",
            "connect",
        ]) {
            Self::Unreachable
        } else {
            Self::Error
        }
    }
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
                FailureReason::Timeout,
                format!("no answer within {} s", timeout.as_secs()),
                started,
            )
        }),
        Err(e) => failed(
            provider,
            FailureReason::Error,
            format!("cannot start the check: {e}"),
            started,
        ),
    };
    redact(result, &secret_values())
}

fn failed(
    provider: &str,
    reason: FailureReason,
    error: String,
    started: Instant,
) -> ConnectionCheck {
    ConnectionCheck {
        provider: provider.to_string(),
        connected: false,
        account: None,
        scope: None,
        error: Some(error),
        reason: Some(reason),
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
        other => Err((
            FailureReason::Unsupported,
            format!(
                "{other} cannot be checked here; use one of {}",
                CHECKABLE.join(", ")
            ),
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
            reason: None,
            latency_ms,
        },
        Err((reason, error)) => ConnectionCheck {
            provider: provider.to_string(),
            connected: false,
            account: None,
            scope: None,
            error: Some(error),
            reason: Some(reason),
            latency_ms,
        },
    }
}

/// A failed check: its reason and its (unredacted) text.
type Failure = (FailureReason, String);

fn not_configured(error: String) -> Failure {
    (FailureReason::NotConfigured, error)
}

fn by_message(error: String) -> Failure {
    (FailureReason::of_message(&error), error)
}

/// The body of an HTTP answer, or the failure its outcome names.
fn http_body(
    outcome: super::hardened_http::HttpOutcome,
    context: impl FnOnce(String) -> String,
) -> Result<String, Failure> {
    use super::hardened_http::HttpOutcome as O;
    let reason = match &outcome {
        O::Success { .. } => None,
        O::Timeout { .. } => Some(FailureReason::Timeout),
        O::NetworkError { .. } => Some(FailureReason::Unreachable),
        O::HttpError { status, .. } => Some(FailureReason::of_status(*status)),
    };
    outcome
        .into_body()
        .map_err(|e| (reason.unwrap_or(FailureReason::Error), context(e)))
}

fn field(body: &str, keys: &[&str]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    keys.iter()
        .find_map(|key| value.get(*key).and_then(serde_json::Value::as_str))
        .map(str::to_string)
}

fn github() -> Result<(String, Option<String>), Failure> {
    let config = GitHubConfig::from_env().map_err(not_configured)?;
    let token = format!("Bearer {}", config.token);
    let body = http_body(
        super::hardened_http::provider_get_with_headers(
            "github",
            &format!("{}/user", config.api_base),
            &[
                ("Authorization", &token),
                ("Accept", "application/vnd.github+json"),
                ("X-GitHub-Api-Version", "2022-11-28"),
            ],
        ),
        |e| format!("GitHub refused the token: {e}"),
    )?;
    let login = field(&body, &["login"]).unwrap_or_else(|| "signed in".to_string());
    Ok((login, config.repo_slug()))
}

fn gitlab() -> Result<(String, Option<String>), Failure> {
    let config = GitLabConfig::from_env().map_err(not_configured)?;
    let body = http_body(
        super::hardened_http::provider_get_with_headers(
            "gitlab",
            &config.api_url("/user"),
            &[("PRIVATE-TOKEN", &config.token)],
        ),
        |e| format!("GitLab at {} refused the token: {e}", config.host),
    )?;
    let user = field(&body, &["username", "name"]).unwrap_or_else(|| "signed in".to_string());
    Ok((user, config.project_path.clone()))
}

fn jira() -> Result<(String, Option<String>), Failure> {
    let config = JiraConfig::from_env().map_err(not_configured)?;
    let user = config.whoami().map_err(by_message)?;
    Ok((user, config.project.clone()))
}

/// The check's `psql` call; the connection travels only in `PG*` variables.
fn postgres_command() -> Result<std::process::Command, Failure> {
    if !super::credentials::is_set("DATABASE_URL") && !super::credentials::is_set("PGDATABASE") {
        return Err(not_configured("DATABASE_URL not set".to_string()));
    }
    let mut command = super::postgres::psql_command().map_err(not_configured)?;
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

fn postgres() -> Result<(String, Option<String>), Failure> {
    let mut command = postgres_command()?;
    let out = super::postgres::run_psql(&mut command, std::time::Duration::from_secs(10))
        .map_err(by_message)?;
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
        assert_eq!(result.reason, Some(FailureReason::Unsupported));
        assert!(result.error.unwrap().contains("github"));
    }

    #[test]
    fn failures_carry_a_machine_readable_reason() {
        assert_eq!(FailureReason::of_status(401), FailureReason::Auth);
        assert_eq!(FailureReason::of_status(429), FailureReason::RateLimited);
        assert_eq!(FailureReason::of_status(503), FailureReason::Unavailable);
        for (text, reason) in [
            ("JIRA_URL not set", FailureReason::NotConfigured),
            ("HTTP 401: Unauthorized", FailureReason::Auth),
            (
                "psql: FATAL: password authentication failed",
                FailureReason::Auth,
            ),
            (
                "psql: could not connect to server: Connection refused",
                FailureReason::Unreachable,
            ),
            ("request timed out", FailureReason::Timeout),
            ("something odd", FailureReason::Error),
        ] {
            assert_eq!(FailureReason::of_message(text), reason, "{text}");
        }
        let line = serde_json::to_string(&FailureReason::NotConfigured).unwrap();
        assert_eq!(line, "\"not_configured\"");
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
                reason: Some(FailureReason::Auth),
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
