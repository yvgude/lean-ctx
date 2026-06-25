//! Flyway (database migrations) output compression.
//!
//! `flyway migrate` prints an edition banner, the JDBC URL and a validation
//! line before the actual work. We keep the applied-migration summary (count +
//! resulting version), the per-version names being migrated, the up-to-date
//! signal and any error, dropping the banner/URL/validation noise.

use crate::core::compressor::strip_ansi;

#[must_use]
pub fn compress(_cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("flyway: ok".to_string());
    }

    let mut applied: Option<String> = None;
    let mut migrating: Vec<String> = Vec::new();
    let mut up_to_date = false;
    let mut errors: Vec<String> = Vec::new();

    for raw in trimmed.lines() {
        let stripped = strip_ansi(raw);
        let line = stripped.trim();
        if line.is_empty() {
            continue;
        }

        if line.contains("No migration necessary") || line.contains("is up to date") {
            up_to_date = true;
        } else if let Some(v) = extract_quoted_after(line, "to version ") {
            migrating.push(format_version(&v));
        } else if line.starts_with("Successfully applied") {
            applied = Some(summarize_applied(line));
        } else if is_error(line) {
            errors.push(line.to_string());
        }
    }

    if applied.is_none() && migrating.is_empty() && !up_to_date && errors.is_empty() {
        return Some(fallback(trimmed));
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(a) = applied {
        parts.push(format!("flyway: {a}"));
    } else if up_to_date {
        parts.push("flyway: up to date".to_string());
    } else if !errors.is_empty() {
        parts.push("flyway: FAILED".to_string());
    } else {
        parts.push("flyway: migrating".to_string());
    }
    for m in &migrating {
        parts.push(format!("  {m}"));
    }
    for e in errors.iter().take(5) {
        parts.push(format!("  {e}"));
    }
    Some(parts.join("\n"))
}

/// Return the text inside the first `"..."` that appears after `marker`.
fn extract_quoted_after(line: &str, marker: &str) -> Option<String> {
    let after = line.split_once(marker)?.1;
    let after = after.split_once('"')?.1;
    let inner = after.split_once('"')?.0;
    Some(inner.to_string())
}

/// `5 - add orders` -> `v5 add orders`.
fn format_version(v: &str) -> String {
    match v.split_once(" - ") {
        Some((ver, name)) => format!("v{} {}", ver.trim(), name.trim()),
        None => format!("v{}", v.trim()),
    }
}

/// `Successfully applied 1 migration to schema "x", now at version v5 (..)` ->
/// `applied 1 migration -> v5`.
fn summarize_applied(line: &str) -> String {
    let count = line
        .strip_prefix("Successfully applied ")
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or("?");
    let noun = if count == "1" {
        "migration"
    } else {
        "migrations"
    };
    let version = extract_after(line, "now at version ")
        .map(|v| {
            v.split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches([',', '.'])
                .to_string()
        })
        .filter(|v| !v.is_empty());
    match version {
        Some(v) => format!("applied {count} {noun} -> {v}"),
        None => format!("applied {count} {noun}"),
    }
}

fn extract_after(line: &str, marker: &str) -> Option<String> {
    line.split_once(marker).map(|(_, r)| r.to_string())
}

fn is_error(line: &str) -> bool {
    line.starts_with("ERROR")
        || line.contains("Migration") && (line.contains("failed") || line.contains("FAILED"))
        || line.contains("SQL State")
        || line.contains("FlywayException")
}

fn fallback(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let n = lines.len().min(8);
    let mut s = lines[..n].join("\n");
    if lines.len() > n {
        s.push_str(&format!("\n... (+{} lines)", lines.len() - n));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIGRATE: &str = "Flyway Community Edition 9.22.0 by Redgate\nDatabase: jdbc:postgresql://localhost:5432/mydb (PostgreSQL 15.2)\nSuccessfully validated 5 migrations (execution time 00:00.123s)\nCurrent version of schema \"public\": 4\nMigrating schema \"public\" to version \"5 - add orders\"\nSuccessfully applied 1 migration to schema \"public\", now at version v5 (execution time 00:00.456s)\n";

    #[test]
    fn keeps_applied_summary_and_version() {
        let r = compress("flyway migrate", MIGRATE).unwrap();
        assert!(r.contains("applied 1 migration -> v5"), "{r}");
        assert!(r.contains("v5 add orders"), "{r}");
        assert!(!r.contains("Redgate"), "drops banner: {r}");
        assert!(!r.contains("jdbc:"), "drops jdbc url: {r}");
    }

    #[test]
    fn detects_up_to_date() {
        let out = "Flyway Community Edition 9.22.0 by Redgate\nSchema \"public\" is up to date. No migration necessary.\n";
        let r = compress("flyway migrate", out).unwrap();
        assert_eq!(r, "flyway: up to date");
    }

    #[test]
    fn shorter_than_input() {
        let r = compress("flyway migrate", MIGRATE).unwrap();
        assert!(r.len() < MIGRATE.len());
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("flyway migrate", "").unwrap(), "flyway: ok");
    }
}
