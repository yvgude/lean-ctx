// SPDX-License-Identifier: Apache-2.0
//! PostgreSQL provider — database schema introspection via `psql`.
//!
//! Extracts table/column definitions from `information_schema` to make
//! database structure available as context. Uses `psql` CLI to avoid
//! adding a native PG driver dependency.
//!
//! Configuration via environment variables:
//!   - `DATABASE_URL`: Full connection string (e.g., "postgres://user:pass@host/db")
//!   - Or individual: `PGHOST`, `PGPORT`, `PGDATABASE`, `PGUSER`, `PGPASSWORD`

use crate::core::providers::{ContextProvider, ProviderItem, ProviderParams, ProviderResult};

const MAX_TABLES: usize = 100;
const MAX_STDOUT_BYTES: usize = 1_048_576;
const MAX_STDERR_BYTES: usize = 16_384;
const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub struct PostgresProvider {
    available: bool,
}

impl Default for PostgresProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PostgresProvider {
    pub fn new() -> Self {
        let available = super::credentials::var("DATABASE_URL").is_ok()
            || super::credentials::var("PGDATABASE").is_ok();
        Self { available }
    }
}

impl ContextProvider for PostgresProvider {
    fn id(&self) -> &'static str {
        "postgres"
    }

    fn display_name(&self) -> &'static str {
        "PostgreSQL"
    }

    fn supported_actions(&self) -> &[&str] {
        &["schemas", "tables"]
    }

    fn execute(&self, action: &str, params: &ProviderParams) -> Result<ProviderResult, String> {
        if !self.available {
            return Err("PostgreSQL not configured (need DATABASE_URL or PGDATABASE)".into());
        }
        match action {
            "schemas" | "tables" => list_tables(params),
            _ => Err(format!("Unsupported action: {action}")),
        }
    }

    fn cache_ttl_secs(&self) -> u64 {
        300
    }

    fn requires_auth(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.available
    }
}

/// Validates a PostgreSQL identifier before it is interpolated into SQL.
///
/// The schema name comes from provider params (agent-controlled), and the query
/// is executed via `psql -c`, so parameterized queries are not available.
/// A strict identifier whitelist (`[A-Za-z_][A-Za-z0-9_$]*`, max 63 bytes — the
/// PostgreSQL `NAMEDATALEN` limit) makes injection impossible.
fn validate_pg_identifier(name: &str) -> Result<(), String> {
    let valid_start = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    let valid_rest = name
        .chars()
        .skip(1)
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if name.is_empty() || name.len() > 63 || !valid_start || !valid_rest {
        return Err(format!(
            "Invalid PostgreSQL schema identifier: {name:?} (allowed: [A-Za-z_][A-Za-z0-9_$]*, max 63 chars)"
        ));
    }
    Ok(())
}

fn list_tables(params: &ProviderParams) -> Result<ProviderResult, String> {
    let schema = params.state.as_deref().unwrap_or("public");
    validate_pg_identifier(schema)?;
    let limit = params.limit.unwrap_or(50);
    if !(1..=MAX_TABLES).contains(&limit) {
        return Err(format!(
            "PostgreSQL table limit must be between 1 and {MAX_TABLES}"
        ));
    }
    let column_limit = limit * 20; // bounded before multiplication

    let query = format!(
        "SELECT table_name, column_name, data_type, is_nullable \
         FROM information_schema.columns \
         WHERE table_schema = '{schema}' \
         ORDER BY table_name, ordinal_position \
         LIMIT {column_limit};",
    );

    let mut cmd = psql_command()?;
    cmd.args([
        "-X",
        "-w",
        "-v",
        "ON_ERROR_STOP=1",
        "-t",
        "-A",
        "-F",
        "|",
        "-c",
        &query,
    ])
    .env("PGCLIENTENCODING", "UTF8")
    .stdin(std::process::Stdio::null());
    let stdout = run_psql(&mut cmd, QUERY_TIMEOUT)?;
    parse_tables(&stdout, schema, limit, column_limit)
}

/// A `psql` command connected through `DATABASE_URL` (environment or store).
/// The address is handed over only as `PG*` variables of the child, never as
/// an argument: argv is visible to every user in the process list.
pub(in crate::core) fn psql_command() -> Result<std::process::Command, String> {
    match super::credentials::var("DATABASE_URL") {
        Ok(url) => psql_command_for(&url),
        Err(_) => Ok(std::process::Command::new("psql")),
    }
}

/// A `psql` command connected to `url` through `PG*` variables only.
pub(in crate::core) fn psql_command_for(url: &str) -> Result<std::process::Command, String> {
    let mut command = std::process::Command::new("psql");
    command.envs(connection_env(url)?);
    Ok(command)
}

/// `postgres[ql]://[user[:password]@][host[:port][,…]][/dbname][?sslmode=…]`
/// as libpq environment variables. Errors never quote the address.
fn connection_env(url: &str) -> Result<Vec<(&'static str, String)>, String> {
    let decode = |part: &str| {
        urlencoding::decode(part)
            .map(std::borrow::Cow::into_owned)
            .map_err(|_| "DATABASE_URL has an invalid percent-encoding".to_string())
    };
    let rest = url
        .trim()
        .strip_prefix("postgresql://")
        .or_else(|| url.trim().strip_prefix("postgres://"))
        .ok_or_else(|| "DATABASE_URL must start with postgres:// or postgresql://".to_string())?;
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (authority, dbname) = rest.split_once('/').unwrap_or((rest, ""));
    let (userinfo, hosts) = authority
        .rsplit_once('@')
        .map_or((None, authority), |(u, h)| (Some(u), h));

    let mut env = Vec::new();
    if let Some(userinfo) = userinfo {
        let (user, password) = userinfo
            .split_once(':')
            .map_or((userinfo, None), |(u, p)| (u, Some(p)));
        if !user.is_empty() {
            env.push(("PGUSER", decode(user)?));
        }
        if let Some(password) = password.filter(|p| !p.is_empty()) {
            env.push(("PGPASSWORD", decode(password)?));
        }
    }
    if !hosts.is_empty() {
        let (mut names, mut ports) = (Vec::new(), Vec::new());
        for host in hosts.split(',') {
            // `[::1]:5432` keeps its brackets out of PGHOST.
            let (name, port) = if let Some(v6) = host.strip_prefix('[') {
                let (name, tail) = v6
                    .split_once(']')
                    .ok_or_else(|| "DATABASE_URL has an unclosed [host]".to_string())?;
                (name, tail.strip_prefix(':').unwrap_or(""))
            } else {
                host.rsplit_once(':').unwrap_or((host, ""))
            };
            names.push(decode(name)?);
            ports.push(port.to_string());
        }
        env.push(("PGHOST", names.join(",")));
        if ports.iter().any(|p| !p.is_empty()) {
            env.push(("PGPORT", ports.join(",")));
        }
    }
    if !dbname.is_empty() {
        env.push(("PGDATABASE", decode(dbname)?));
    }
    for (key, value) in query.split('&').filter_map(|pair| pair.split_once('=')) {
        if key == "sslmode" {
            env.push(("PGSSLMODE", decode(value)?));
        }
    }
    Ok(env)
}

/// Shared bounded execution for catalog introspection and the pgvector backend.
pub(in crate::core) fn run_psql(
    command: &mut std::process::Command,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let captured = crate::core::process_capture::run_with_output_limits(
        command,
        Some(timeout),
        MAX_STDOUT_BYTES,
        MAX_STDERR_BYTES,
    )
    .map_err(|_| "PostgreSQL command failed or exceeded its output bound".to_owned())?;
    if captured.timed_out {
        return Err("PostgreSQL command exceeded its deadline".to_owned());
    }
    if captured.cancelled || !captured.output.status.success() {
        // Database diagnostics can echo connection credentials, SQL or row data.
        return Err("PostgreSQL command failed; database diagnostics withheld".to_owned());
    }
    String::from_utf8(captured.output.stdout)
        .map_err(|_| "PostgreSQL returned invalid UTF-8".to_owned())
}

fn parse_tables(
    stdout: &str,
    schema: &str,
    limit: usize,
    column_limit: usize,
) -> Result<ProviderResult, String> {
    let mut tables: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut column_count = 0;
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() == 4
            && !parts[0].trim().is_empty()
            && !parts[1].trim().is_empty()
            && !parts[2].trim().is_empty()
            && matches!(parts[3].trim(), "YES" | "NO")
        {
            column_count += 1;
            let table = parts[0].trim();
            let col = parts[1].trim();
            let dtype = parts[2].trim();
            let nullable = parts.get(3).map_or("", |s| s.trim());

            let null_marker = if nullable == "YES" { "?" } else { "" };
            tables
                .entry(table.to_string())
                .or_default()
                .push(format!("  {col}: {dtype}{null_marker}"));
        } else {
            return Err("PostgreSQL returned an invalid schema record".to_owned());
        }
    }

    let items: Vec<ProviderItem> = tables
        .iter()
        .take(limit)
        .map(|(table, columns)| {
            let body = format!("{schema}.{table}\n{}", columns.join("\n"));
            ProviderItem {
                id: table.clone(),
                title: format!("{schema}.{table}"),
                state: Some("active".into()),
                author: None,
                created_at: None,
                updated_at: None,
                url: None,
                labels: vec![schema.to_string()],
                body: Some(body),
                ..Default::default()
            }
        })
        .collect();

    let truncated = column_count >= column_limit || tables.len() > limit;
    Ok(ProviderResult {
        provider: "postgres".into(),
        resource_type: "schemas".into(),
        items,
        total_count: (!truncated).then_some(tables.len()),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_provider_unavailable_without_env() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("DATABASE_URL");
        crate::test_env::remove_var("PGDATABASE");

        let provider = PostgresProvider::new();
        assert!(!provider.is_available());
        assert_eq!(provider.id(), "postgres");
        assert!(provider.requires_auth());
    }

    #[test]
    fn postgres_provider_supported_actions() {
        let provider = PostgresProvider::new();
        assert!(provider.supported_actions().contains(&"schemas"));
        assert!(provider.supported_actions().contains(&"tables"));
    }

    // P0-5 (#417): schema names are agent-controlled and interpolated into SQL —
    // only strict identifiers may pass.
    #[test]
    fn valid_pg_identifiers_pass() {
        for ok in ["public", "my_schema", "_internal", "Schema1", "a$b"] {
            assert!(validate_pg_identifier(ok).is_ok(), "{ok} should be valid");
        }
    }

    #[test]
    fn sql_injection_payloads_are_rejected() {
        for evil in [
            "public' UNION SELECT usename, passwd, '', '' FROM pg_shadow --",
            "public'; DROP TABLE users; --",
            "a\"b",
            "a b",
            "a;b",
            "schema\n--",
            "",
            "1starts_with_digit",
        ] {
            assert!(
                validate_pg_identifier(evil).is_err(),
                "{evil:?} must be rejected"
            );
        }
    }

    #[test]
    fn overlong_identifier_is_rejected() {
        let too_long = "a".repeat(64);
        assert!(validate_pg_identifier(&too_long).is_err());
        let max_ok = "a".repeat(63);
        assert!(validate_pg_identifier(&max_ok).is_ok());
    }

    #[test]
    fn injection_via_params_state_fails_closed() {
        let params = ProviderParams {
            state: Some("public' OR '1'='1".into()),
            ..Default::default()
        };
        let err = list_tables(&params).unwrap_err();
        assert!(err.contains("Invalid PostgreSQL schema identifier"));
    }

    #[test]
    fn table_limits_reject_overflow_and_empty_requests_before_dispatch() {
        for limit in [0, MAX_TABLES + 1, usize::MAX] {
            let params = ProviderParams {
                limit: Some(limit),
                ..Default::default()
            };
            assert!(list_tables(&params).unwrap_err().contains("table limit"));
        }
    }

    #[test]
    fn schema_column_cap_is_reported_as_incomplete_not_a_total() {
        let result = parse_tables("orders|id|integer|NO\n", "public", 1, 1).unwrap();
        assert!(result.truncated);
        assert_eq!(result.total_count, None);
        let complete = parse_tables("orders|id|integer|NO\n", "public", 1, 20).unwrap();
        assert!(!complete.truncated);
        assert_eq!(complete.total_count, Some(1));
        assert_eq!(
            complete.items[0].body.as_deref(),
            Some("public.orders\n  id: integer")
        );
        for invalid in [
            "orders|partial\n",
            "orders|id||NO\n",
            "orders|id|  |NO\n",
            "orders|id|integer|\n",
            "orders|id|integer|UNKNOWN\n",
        ] {
            assert!(parse_tables(invalid, "public", 1, 20).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn process_failure_does_not_disclose_database_diagnostics() {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "printf 'private-diagnostic-marker' >&2; exit 1"]);
        let error = run_psql(&mut command, QUERY_TIMEOUT).unwrap_err();
        assert!(error.contains("diagnostics withheld"));
        assert!(!error.contains("private-diagnostic-marker"));
    }

    #[cfg(unix)]
    #[test]
    fn process_capture_rejects_oversized_stdout_and_stderr() {
        for script in [
            "/bin/dd if=/dev/zero bs=1048577 count=1 2>/dev/null",
            "/bin/dd if=/dev/zero bs=16385 count=1 1>&2 2>/dev/null",
        ] {
            let mut command = std::process::Command::new("/bin/sh");
            command.args(["-c", script]);
            assert!(
                run_psql(&mut command, QUERY_TIMEOUT)
                    .unwrap_err()
                    .contains("output bound")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn process_capture_has_a_deadline_and_rejects_invalid_utf8() {
        let mut command = std::process::Command::new("/bin/sleep");
        command.arg("20");
        let started = std::time::Instant::now();
        let error = run_psql(&mut command, std::time::Duration::from_millis(20)).unwrap_err();
        assert!(error.contains("deadline"));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "printf '\\377'"]);
        assert!(
            run_psql(&mut command, QUERY_TIMEOUT)
                .unwrap_err()
                .contains("invalid UTF-8")
        );
    }
}
