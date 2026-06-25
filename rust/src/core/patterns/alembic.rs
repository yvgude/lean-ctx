//! Alembic (`SQLAlchemy` migrations) output compression.
//!
//! `alembic upgrade`/`downgrade` log each step as
//! `INFO  [alembic.runtime.migration] Running upgrade <from> -> <to>, <msg>`
//! plus boilerplate (`Context impl ...`, `Will assume transactional DDL`).
//! We keep one compact line per applied revision and any error, dropping the
//! logger prefix and boilerplate.

use crate::core::compressor::strip_ansi;

#[must_use]
pub fn compress(_cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("alembic: ok".to_string());
    }

    let mut upgrades: Vec<String> = Vec::new();
    let mut downgrades: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for raw in trimmed.lines() {
        let stripped = strip_ansi(raw);
        let line = stripped.trim();
        if line.is_empty() {
            continue;
        }
        let body = strip_log_prefix(line);

        if let Some(rest) = body.strip_prefix("Running upgrade ") {
            upgrades.push(parse_target(rest));
        } else if let Some(rest) = body.strip_prefix("Running downgrade ") {
            downgrades.push(parse_target(rest));
        } else if is_error(line) {
            errors.push(body.to_string());
        }
    }

    if upgrades.is_empty() && downgrades.is_empty() && errors.is_empty() {
        return Some(fallback(trimmed));
    }

    let mut parts: Vec<String> = Vec::new();
    let mut header = String::from("alembic:");
    if !upgrades.is_empty() {
        header.push_str(&format!(" {} upgrade(s)", upgrades.len()));
    }
    if !downgrades.is_empty() {
        header.push_str(&format!(" {} downgrade(s)", downgrades.len()));
    }
    if upgrades.is_empty() && downgrades.is_empty() {
        header.push_str(" FAILED");
    }
    parts.push(header);
    for u in &upgrades {
        parts.push(format!("  ↑ {u}"));
    }
    for d in &downgrades {
        parts.push(format!("  ↓ {d}"));
    }
    for e in errors.iter().take(5) {
        parts.push(format!("  {e}"));
    }
    Some(parts.join("\n"))
}

/// Drop a leading `LEVEL  [alembic...] ` python-logging prefix.
fn strip_log_prefix(line: &str) -> &str {
    let is_log = line.starts_with("INFO")
        || line.starts_with("WARNING")
        || line.starts_with("ERROR")
        || line.starts_with("DEBUG");
    if is_log
        && line.contains("[alembic")
        && let Some(idx) = line.find("] ")
    {
        return line[idx + 2..].trim_start();
    }
    line
}

/// Parse `<from> -> <to>, <msg>` into `<to> <msg>` (msg optional).
fn parse_target(rest: &str) -> String {
    let after = rest.split("-> ").nth(1).unwrap_or(rest).trim();
    match after.split_once(", ") {
        Some((rev, msg)) => format!("{} {}", rev.trim(), msg.trim()),
        None => after.to_string(),
    }
}

fn is_error(line: &str) -> bool {
    line.starts_with("ERROR")
        || line.starts_with("FAILED")
        || line.contains("Error:")
        || line.contains("alembic.util.exc")
        || line.contains("Can't locate revision")
        || line.contains("Target database is not up to date")
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

    const UPGRADE: &str = "INFO  [alembic.runtime.migration] Context impl PostgresqlImpl.\nINFO  [alembic.runtime.migration] Will assume transactional DDL.\nINFO  [alembic.runtime.migration] Running upgrade  -> a1b2c3, create users table\nINFO  [alembic.runtime.migration] Running upgrade a1b2c3 -> d4e5f6, add email index\n";

    #[test]
    fn keeps_revisions_drops_boilerplate() {
        let r = compress("alembic upgrade head", UPGRADE).unwrap();
        assert!(r.contains("2 upgrade(s)"), "counts upgrades: {r}");
        assert!(r.contains("a1b2c3 create users table"), "{r}");
        assert!(r.contains("d4e5f6 add email index"), "{r}");
        assert!(!r.contains("transactional DDL"), "drops boilerplate: {r}");
        assert!(!r.contains("Context impl"), "drops boilerplate: {r}");
    }

    #[test]
    fn shorter_than_input() {
        let r = compress("alembic upgrade head", UPGRADE).unwrap();
        assert!(r.len() < UPGRADE.len());
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("alembic upgrade head", "").unwrap(), "alembic: ok");
    }

    #[test]
    fn surfaces_errors() {
        let out = "INFO  [alembic.runtime.migration] Context impl PostgresqlImpl.\nFAILED: Can't locate revision identified by 'deadbeef'\n";
        let r = compress("alembic upgrade head", out).unwrap();
        assert!(r.contains("FAILED"), "{r}");
        assert!(r.contains("deadbeef"), "{r}");
    }
}
