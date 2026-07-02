//! `lean-ctx gateway doctor` (enterprise#49) — go-live preflight.
//!
//! Every check prints one line (`ok` / `warn` / `FAIL`) with a concrete fix
//! command; the process exits non-zero when any FAIL is present. Checks run
//! against the *instance directory* (`--dir`, default `.`): its `.env`,
//! `config.toml` and `gateway-keys.toml` — plus live probes (Postgres
//! connect + `SELECT 1`, proxy/admin port reachability).

use std::fmt;
use std::path::{Path, PathBuf};

/// Severity of a single check result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warn,
    Fail,
}

/// One check line: what was checked, what was found, how to fix it.
#[derive(Debug)]
pub struct CheckResult {
    pub severity: Severity,
    pub name: &'static str,
    pub detail: String,
    pub fix: Option<String>,
}

impl fmt::Display for CheckResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tag = match self.severity {
            Severity::Ok => "\x1b[32m ok \x1b[0m",
            Severity::Warn => "\x1b[33mwarn\x1b[0m",
            Severity::Fail => "\x1b[31mFAIL\x1b[0m",
        };
        write!(f, "[{tag}] {:<18} {}", self.name, self.detail)?;
        if let Some(fix) = &self.fix {
            write!(f, "\n{:24}fix: {fix}", "")?;
        }
        Ok(())
    }
}

fn ok(name: &'static str, detail: impl Into<String>) -> CheckResult {
    CheckResult {
        severity: Severity::Ok,
        name,
        detail: detail.into(),
        fix: None,
    }
}
fn warn(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> CheckResult {
    CheckResult {
        severity: Severity::Warn,
        name,
        detail: detail.into(),
        fix: Some(fix.into()),
    }
}
fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> CheckResult {
    CheckResult {
        severity: Severity::Fail,
        name,
        detail: detail.into(),
        fix: Some(fix.into()),
    }
}

/// The `.env` slice doctor cares about.
#[derive(Debug, Default)]
struct EnvFile {
    proxy_token: Option<String>,
    admin_token: Option<String>,
    database_url: Option<String>,
}

/// Parses `KEY=value` lines (the generated `.env` format; quotes not needed).
fn parse_env_file(path: &Path) -> EnvFile {
    let mut out = EnvFile::default();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return out;
    };
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().to_string();
            match k.trim() {
                "LEAN_CTX_PROXY_TOKEN" => out.proxy_token = Some(v),
                "LEAN_CTX_GATEWAY_ADMIN_TOKEN" => out.admin_token = Some(v),
                "DATABASE_URL" => out.database_url = Some(v),
                _ => {}
            }
        }
    }
    out
}

/// Runs all checks. `proxy_port`/`admin_port` are probed on localhost.
pub async fn run_checks(dir: &Path, proxy_port: u16, admin_port: u16) -> Vec<CheckResult> {
    let mut results = Vec::new();

    // -- instance files ------------------------------------------------------
    let config_path = dir.join("config.toml");
    let config_raw = std::fs::read_to_string(&config_path).ok();
    match &config_raw {
        Some(raw) => match toml::from_str::<toml::Value>(raw) {
            Ok(v) => {
                results.push(ok("config.toml", "present, parses"));
                results.extend(check_config_values(&v));
            }
            Err(e) => results.push(fail(
                "config.toml",
                format!("does not parse: {e}"),
                "fix the TOML syntax (or regenerate with `lean-ctx gateway init`)",
            )),
        },
        None => results.push(warn(
            "config.toml",
            format!("not found in {}", dir.display()),
            "run `lean-ctx gateway init <dir>` or pass --dir <instance dir>",
        )),
    }

    let keys_path = keys_path_for(dir);
    match crate::proxy::gateway_identity::GatewayKeys::load(&keys_path) {
        Ok(keys) if keys.is_empty() => results.push(warn(
            "gateway-keys",
            "no per-person keys — usage will meter as 'anonymous'",
            format!(
                "lean-ctx gateway keys add --person alice@example.com --file {}",
                keys_path.display()
            ),
        )),
        Ok(keys) => results.push(ok("gateway-keys", format!("{} identities", keys.len()))),
        Err(e) => results.push(fail(
            "gateway-keys",
            format!("invalid: {e}"),
            "fix the file — the gateway refuses to start on a malformed key set",
        )),
    }

    // -- secrets -------------------------------------------------------------
    let env = parse_env_file(&dir.join(".env"));
    let proxy_token = env
        .proxy_token
        .or_else(|| std::env::var("LEAN_CTX_PROXY_TOKEN").ok());
    match proxy_token {
        Some(t) if t.len() >= 32 => results.push(ok("proxy token", "set")),
        Some(_) => results.push(warn(
            "proxy token",
            "set but short (<32 chars)",
            "use 32+ random bytes: openssl rand -hex 32",
        )),
        None => results.push(fail(
            "proxy token",
            "LEAN_CTX_PROXY_TOKEN not in .env or environment",
            "add LEAN_CTX_PROXY_TOKEN=$(openssl rand -hex 32) to .env",
        )),
    }
    let admin_token = env
        .admin_token
        .or_else(|| std::env::var(super::serve::ADMIN_TOKEN_ENV).ok());
    match admin_token {
        Some(t) if t.len() >= 32 => results.push(ok("admin token", "set")),
        Some(_) => results.push(warn(
            "admin token",
            "set but short (<32 chars)",
            "use 32+ random bytes: openssl rand -hex 32",
        )),
        None => results.push(warn(
            "admin token",
            format!(
                "{} not set — admin console stays off",
                super::serve::ADMIN_TOKEN_ENV
            ),
            "add LEAN_CTX_GATEWAY_ADMIN_TOKEN=$(openssl rand -hex 32) to .env",
        )),
    }

    // -- Postgres ------------------------------------------------------------
    let database_url = env
        .database_url
        .or_else(|| std::env::var(super::serve::DATABASE_URL_ENV).ok());
    match database_url {
        Some(url) => results.push(check_postgres(&url).await),
        None => results.push(warn(
            "postgres",
            "DATABASE_URL not set — metering/console off (traffic still works)",
            "add DATABASE_URL=postgres://… to .env",
        )),
    }

    // -- provider credentials (from config's registry) -----------------------
    if let Some(raw) = &config_raw
        && let Ok(v) = toml::from_str::<toml::Value>(raw)
    {
        results.extend(check_provider_credentials(
            &v,
            &parse_env_names(&dir.join(".env")),
        ));
    }

    // -- live ports ----------------------------------------------------------
    results.push(probe_http("proxy port", proxy_port, "/health", false).await);
    results.push(probe_http("admin port", admin_port, "/healthz", true).await);

    results
}

/// Static config sanity (bind/token posture).
fn check_config_values(v: &toml::Value) -> Vec<CheckResult> {
    let mut out = Vec::new();
    let bind = v.get("proxy_bind_host").and_then(|b| b.as_str());
    let require_token = v
        .get("proxy_require_token")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    match (bind, require_token) {
        (Some(b), true) if b != "127.0.0.1" => {
            out.push(ok("bind posture", format!("{b} with required tokens")));
        }
        (Some(b), false) if b != "127.0.0.1" => out.push(fail(
            "bind posture",
            format!("binds {b} WITHOUT proxy_require_token"),
            "set proxy_require_token = true in config.toml",
        )),
        _ => out.push(ok("bind posture", "loopback (solo mode)")),
    }
    if v.get("proxy")
        .and_then(|p| p.get("baseline"))
        .and_then(|b| b.get("reference_model"))
        .and_then(|m| m.as_str())
        .is_none()
    {
        out.push(warn(
            "baseline",
            "no [proxy.baseline] reference_model — avoided-cost stays 0",
            "set reference_model = \"claude-opus-4.5\" (or your contract reference)",
        ));
    } else {
        out.push(ok("baseline", "reference_model configured"));
    }
    out
}

/// Names (not values) defined in `.env` — for provider `api_key_env` checks.
fn parse_env_names(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|raw| {
            raw.lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Each registry provider that injects a credential needs its env var — in the
/// process env (solo) or declared in `.env` (compose passes it through).
fn check_provider_credentials(v: &toml::Value, env_file_names: &[String]) -> Vec<CheckResult> {
    let mut out = Vec::new();
    let providers = v
        .get("proxy")
        .and_then(|p| p.get("providers"))
        .and_then(|p| p.as_array());
    for entry in providers.unwrap_or(&Vec::new()) {
        let id = entry.get("id").and_then(|i| i.as_str()).unwrap_or("?");
        let enabled = entry
            .get("enabled")
            .and_then(toml::Value::as_bool)
            .unwrap_or(true);
        if !enabled {
            continue;
        }
        let Some(env_name) = entry.get("api_key_env").and_then(|e| e.as_str()) else {
            continue;
        };
        let present = std::env::var(env_name).is_ok_and(|x| !x.trim().is_empty())
            || env_file_names.iter().any(|n| n == env_name);
        if present {
            out.push(ok("provider key", format!("{id}: {env_name} available")));
        } else {
            out.push(fail(
                "provider key",
                format!("{id}: {env_name} missing"),
                format!("add {env_name}=<key> to .env (and pass it through in docker-compose.yml)"),
            ));
        }
    }
    out
}

async fn check_postgres(url: &str) -> CheckResult {
    // Container-internal hostnames (e.g. `postgres`) are normal in compose
    // setups; from the host we resolve them to localhost for the probe.
    let probe_url = url.replace("@postgres:", "@127.0.0.1:");
    match super::store::pool_from_database_url(&probe_url) {
        Ok(pool) => {
            let probe = async {
                let client = pool.get().await?;
                client.query_one("SELECT 1", &[]).await?;
                anyhow::Ok(())
            };
            match tokio::time::timeout(std::time::Duration::from_secs(4), probe).await {
                Ok(Ok(())) => ok("postgres", "connected (SELECT 1 ok)"),
                Ok(Err(e)) => warn(
                    "postgres",
                    format!("unreachable: {e:#}"),
                    "start it (docker compose up -d postgres) — traffic is fail-open meanwhile",
                ),
                Err(_) => warn(
                    "postgres",
                    "connect timeout (4s)",
                    "check host/port/firewall — traffic is fail-open meanwhile",
                ),
            }
        }
        Err(e) => fail(
            "postgres",
            format!("DATABASE_URL invalid: {e:#}"),
            "fix the connection string in .env",
        ),
    }
}

async fn probe_http(name: &'static str, port: u16, path: &str, optional: bool) -> CheckResult {
    let url = format!("http://127.0.0.1:{port}{path}");
    let probe = tokio::task::spawn_blocking(move || {
        ureq::get(&url)
            .config()
            .timeout_global(Some(std::time::Duration::from_secs(2)))
            .build()
            .call()
            .is_ok()
    });
    match probe.await {
        Ok(true) => ok(name, format!("listening on {port}")),
        _ if optional => warn(
            name,
            format!("nothing on {port} (gateway not running?)"),
            "start it: docker compose up -d  (or lean-ctx gateway serve)",
        ),
        _ => warn(
            name,
            format!("nothing on {port} (gateway not running?)"),
            "start it: docker compose up -d  (or lean-ctx gateway serve)",
        ),
    }
}

fn keys_path_for(dir: &Path) -> PathBuf {
    let local = dir.join("gateway-keys.toml");
    if local.exists() {
        local
    } else {
        crate::proxy::gateway_identity::GatewayKeys::default_path()
    }
}

/// True when any check failed (exit code driver).
#[must_use]
pub fn has_failures(results: &[CheckResult]) -> bool {
    results.iter().any(|r| r.severity == Severity::Fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_posture_flags_open_bind_without_tokens() {
        let open: toml::Value =
            toml::from_str("proxy_bind_host = \"0.0.0.0\"\nproxy_require_token = false").unwrap();
        let results = check_config_values(&open);
        assert!(
            results
                .iter()
                .any(|r| r.name == "bind posture" && r.severity == Severity::Fail),
            "open bind without token must FAIL"
        );

        let hardened: toml::Value =
            toml::from_str("proxy_bind_host = \"0.0.0.0\"\nproxy_require_token = true").unwrap();
        let results = check_config_values(&hardened);
        assert!(
            results
                .iter()
                .any(|r| r.name == "bind posture" && r.severity == Severity::Ok)
        );
    }

    #[test]
    fn provider_credential_check_consults_env_file_names() {
        let cfg: toml::Value = toml::from_str(
            r#"
            [[proxy.providers]]
            id = "foundry"
            shape = "openai"
            base_url = "https://x.services.ai.azure.com/models"
            api_key_env = "FOUNDRY_API_KEY_DOCTOR_TEST"

            [[proxy.providers]]
            id = "disabled-one"
            shape = "openai"
            base_url = "https://y.example.com"
            api_key_env = "NEVER_CHECKED"
            enabled = false
            "#,
        )
        .unwrap();

        let missing = check_provider_credentials(&cfg, &[]);
        assert_eq!(missing.len(), 1, "disabled providers are skipped");
        assert_eq!(missing[0].severity, Severity::Fail);

        let present = check_provider_credentials(&cfg, &["FOUNDRY_API_KEY_DOCTOR_TEST".into()]);
        assert_eq!(present[0].severity, Severity::Ok);
    }

    #[test]
    fn env_file_parsing_extracts_doctor_relevant_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".env");
        std::fs::write(
            &path,
            "# comment\nLEAN_CTX_PROXY_TOKEN=abc\nDATABASE_URL=postgres://u:p@h:5432/db\nOTHER=x\n",
        )
        .unwrap();
        let env = parse_env_file(&path);
        assert_eq!(env.proxy_token.as_deref(), Some("abc"));
        assert_eq!(
            env.database_url.as_deref(),
            Some("postgres://u:p@h:5432/db")
        );
        assert_eq!(env.admin_token, None);
        let names = parse_env_names(&path);
        assert!(names.contains(&"OTHER".to_string()));
    }

    #[test]
    fn failure_detection_drives_exit_code() {
        assert!(!has_failures(&[ok("x", "fine")]));
        assert!(has_failures(&[ok("x", "fine"), fail("y", "bad", "fix")]));
        assert!(!has_failures(&[warn("z", "meh", "later")]));
    }
}
