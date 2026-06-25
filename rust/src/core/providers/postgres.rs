//! PostgreSQL provider — database schema introspection via `psql`.
//!
//! Extracts table/column definitions from `information_schema` to make
//! database structure available as context. Uses `psql` CLI to avoid
//! adding a native PG driver dependency.
//!
//! Configuration via environment variables:
//!   - `DATABASE_URL`: Full connection string (e.g., "<postgres://user:pass@host/db>")
//!   - Or individual: `PGHOST`, `PGPORT`, `PGDATABASE`, `PGUSER`, `PGPASSWORD`

use crate::core::providers::{ContextProvider, ProviderItem, ProviderParams, ProviderResult};

pub struct PostgresProvider {
    available: bool,
}

impl Default for PostgresProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PostgresProvider {
    #[must_use]
    pub fn new() -> Self {
        let available =
            std::env::var("DATABASE_URL").is_ok() || std::env::var("PGDATABASE").is_ok();
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

    let query = format!(
        "SELECT table_name, column_name, data_type, is_nullable \
         FROM information_schema.columns \
         WHERE table_schema = '{schema}' \
         ORDER BY table_name, ordinal_position \
         LIMIT {limit_cols};",
        limit_cols = limit * 20, // ~20 columns per table avg
    );

    let mut cmd = std::process::Command::new("psql");

    if let Ok(url) = std::env::var("DATABASE_URL") {
        cmd.arg(&url);
    }

    let output = cmd
        .args(["-t", "-A", "-F", "|", "-c", &query])
        .output()
        .map_err(|e| format!("Failed to run psql: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("psql error: {stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut tables: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() >= 3 {
            let table = parts[0].trim();
            let col = parts[1].trim();
            let dtype = parts[2].trim();
            let nullable = parts.get(3).map_or("", |s| s.trim());

            let null_marker = if nullable == "YES" { "?" } else { "" };
            tables
                .entry(table.to_string())
                .or_default()
                .push(format!("  {col}: {dtype}{null_marker}"));
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

    Ok(ProviderResult {
        provider: "postgres".into(),
        resource_type: "schemas".into(),
        items,
        total_count: Some(tables.len()),
        truncated: tables.len() > limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_provider_unavailable_without_env() {
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
}
