use std::io::Write;
use std::path::PathBuf;

use chrono::{Local, Utc};

fn journal_path() -> PathBuf {
    crate::core::paths::state_dir()
        .unwrap_or_else(|_| PathBuf::from(".lean-ctx"))
        .join("journal.md")
}

fn is_enabled() -> bool {
    if let Ok(v) = std::env::var("LEAN_CTX_JOURNAL") {
        return !matches!(v.trim(), "0" | "false" | "off");
    }
    super::config::Config::load().journal_enabled
}

/// Append a human-readable entry to the activity journal.
pub fn log(category: &str, message: &str) {
    if !is_enabled() {
        return;
    }
    let path = journal_path();
    let timestamp = Local::now().format("%Y-%m-%d %H:%M");

    let entry = format!("- **{timestamp}** [{category}] {message}\n");

    let needs_header = !path.exists();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);

    if let Ok(mut f) = file {
        if needs_header {
            let date = Utc::now().format("%Y-%m-%d");
            let _ = writeln!(f, "# lean-ctx Activity Journal\n\n## {date}\n");
        }
        let _ = f.write_all(entry.as_bytes());
    }
}

/// Insert a day separator if the last entry was on a different date.
pub fn maybe_day_separator() {
    if !is_enabled() {
        return;
    }
    let path = journal_path();
    if !path.exists() {
        return;
    }

    let today = Local::now().format("%Y-%m-%d").to_string();
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let header = format!("## {today}");
    if !content.contains(&header) {
        let file = std::fs::OpenOptions::new().append(true).open(&path);
        if let Ok(mut f) = file {
            let _ = writeln!(f, "\n{header}\n");
        }
    }
}

/// Log a tool call to the journal.
pub fn log_tool_call(tool_name: &str, summary: &str) {
    if matches!(
        tool_name,
        "ctx_session" | "ctx_knowledge" | "ctx_context" | "ctx_radar"
    ) {
        return;
    }
    log("tool", &format!("`{tool_name}` — {summary}"));
}

/// Return the journal content for display.
#[must_use]
pub fn read_journal(tail_lines: usize) -> String {
    let path = journal_path();
    if !path.exists() {
        return "No journal entries yet.".to_string();
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    if tail_lines == 0 {
        return content;
    }
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(tail_lines);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_log_creates_file() {
        // `journal.md` is STATE (GH #408); isolated_data_dir collapses all four
        // category dirs onto one temp dir so the write/read pair stays valid.
        let iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::set_var("LEAN_CTX_JOURNAL", "1");

        log("test", "hello world");

        let path = iso.path().join("journal.md");
        assert!(path.exists(), "journal.md should be created");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[test] hello world"));
        assert!(content.contains("# lean-ctx Activity Journal"));

        crate::test_env::remove_var("LEAN_CTX_JOURNAL");
    }

    #[test]
    fn read_journal_tail() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::set_var("LEAN_CTX_JOURNAL", "1");

        for i in 0..5 {
            log("test", &format!("entry {i}"));
        }

        let tail = read_journal(2);
        assert!(tail.contains("entry 4"), "should contain last entry");
        assert!(
            !tail.contains("Activity Journal"),
            "should not contain header"
        );

        crate::test_env::remove_var("LEAN_CTX_JOURNAL");
    }
}
