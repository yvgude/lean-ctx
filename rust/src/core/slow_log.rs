use std::path::PathBuf;

const LOG_FILENAME: &str = "slow-commands.log";
const MAX_LOG_ENTRIES: usize = 500;

fn slow_log_path() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join(LOG_FILENAME))
}

pub fn record(command: &str, duration_ms: u128, exit_code: i32) {
    let Some(path) = slow_log_path() else { return };

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let entry = format!("{ts}\t{duration_ms}ms\texit:{exit_code}\t{command}\n");

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let lines: Vec<&str> = existing.lines().collect();

    let kept = if lines.len() >= MAX_LOG_ENTRIES {
        &lines[lines.len() - MAX_LOG_ENTRIES + 1..]
    } else {
        &lines[..]
    };

    let mut content = kept.join("\n");
    if !content.is_empty() {
        content.push('\n');
    }
    content.push_str(&entry);

    let _ = std::fs::write(&path, content);
}

pub fn list() -> String {
    let Some(path) = slow_log_path() else {
        return "Cannot determine data directory.".to_string();
    };

    match std::fs::read_to_string(&path) {
        Ok(content) if !content.trim().is_empty() => {
            let lines: Vec<&str> = content.lines().collect();
            let header = format!(
                "Slow command log ({} entries)  [{}]\n{}\n",
                lines.len(),
                path.display(),
                "─".repeat(72)
            );
            let table: String = lines
                .iter()
                .map(|l| {
                    let parts: Vec<&str> = l.splitn(4, '\t').collect();
                    if parts.len() == 4 {
                        format!(
                            "{:<20}  {:>8}  {:>8}  {}",
                            parts[0], parts[1], parts[2], parts[3]
                        )
                    } else {
                        l.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("{header}{table}\n")
        }
        Ok(_) => "No slow commands recorded yet.".to_string(),
        Err(_) => format!("No slow log found at {}", path.display()),
    }
}

pub fn clear() -> String {
    let Some(path) = slow_log_path() else {
        return "Cannot determine data directory.".to_string();
    };

    if !path.exists() {
        return "No slow log to clear.".to_string();
    }

    match std::fs::remove_file(&path) {
        Ok(()) => format!("Cleared {}", path.display()),
        Err(e) => format!("Error clearing log: {e}"),
    }
}
