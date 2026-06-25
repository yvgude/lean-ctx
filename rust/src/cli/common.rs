pub(crate) fn print_savings(original: usize, sent: usize) {
    let footer = crate::core::protocol::format_savings(original, sent);
    if !footer.is_empty() {
        println!("{footer}");
    }
}

/// Strip savings footers from daemon output when the CLI client has footer suppressed.
#[cfg(unix)]
pub(crate) fn filter_daemon_output(text: &str) -> String {
    if crate::core::protocol::savings_footer_visible() {
        return text.to_string();
    }
    text.lines()
        .filter(|l| {
            let t = l.trim();
            !(t.starts_with('[')
                && t.contains("tok")
                && t.ends_with(']')
                && (t.contains("tok saved") || t.contains("lean-ctx:") || t.contains("vs native")))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[must_use]
pub fn load_shell_history_pub() -> Vec<String> {
    load_shell_history()
}

pub(crate) fn load_shell_history() -> Vec<String> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    let history_file = if shell.contains("zsh") {
        home.join(".zsh_history")
    } else if shell.contains("fish") {
        home.join(".local/share/fish/fish_history")
    } else if cfg!(windows) && shell.is_empty() {
        home.join("AppData")
            .join("Roaming")
            .join("Microsoft")
            .join("Windows")
            .join("PowerShell")
            .join("PSReadLine")
            .join("ConsoleHost_history.txt")
    } else {
        home.join(".bash_history")
    };

    // Shell history files (especially zsh's metafied format) frequently contain
    // non-UTF-8 bytes; `read_to_string` would reject the whole file. Read raw and
    // decode lossily so a single bad byte never hides 900 lines of real history.
    match std::fs::read(&history_file) {
        Ok(bytes) => String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|l| {
                let trimmed = l.trim();
                if trimmed.starts_with(':') {
                    trimmed
                        .split(';')
                        .nth(1)
                        .map(std::string::ToString::to_string)
                } else {
                    Some(trimmed.to_string())
                }
            })
            .filter(|l| !l.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

pub(crate) fn daemon_fallback_hint() {
    use std::sync::Once;
    static HINT: Once = Once::new();
    HINT.call_once(|| {
        if crate::core::protocol::meta_visible() {
            eprintln!("\x1b[2;33mhint: daemon not running — stats tracked locally (lean-ctx serve -d for full tracking)\x1b[0m");
        }
    });
}

pub(crate) fn format_tokens_cli(tokens: u64) -> String {
    if tokens >= 1_000_000_000_000 {
        format!("{:.2}T", tokens as f64 / 1_000_000_000_000.0)
    } else if tokens >= 1_000_000_000 {
        // Heavy users cross 1B; keep growing visibly instead of "1310.0M".
        format!("{:.2}B", tokens as f64 / 1_000_000_000.0)
    } else if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}

pub(crate) fn cli_track_read(
    path: &str,
    mode: &str,
    original_tokens: usize,
    output_tokens: usize,
    output: &str,
    duration: std::time::Duration,
) {
    crate::core::tool_lifecycle::record_file_read(
        path,
        mode,
        original_tokens,
        output_tokens,
        false,
        duration,
        output,
    );
}

pub(crate) fn cli_track_read_cached(
    path: &str,
    mode: &str,
    original_tokens: usize,
    output_tokens: usize,
    output: &str,
    duration: std::time::Duration,
) {
    crate::core::tool_lifecycle::record_file_read(
        path,
        mode,
        original_tokens,
        output_tokens,
        true,
        duration,
        output,
    );
}

pub(crate) fn cli_track_search(
    modeled_baseline: usize,
    observed_tokens: usize,
    output_tokens: usize,
    pattern: &str,
    path: &str,
    output: &str,
    duration: std::time::Duration,
) {
    crate::core::tool_lifecycle::record_search(
        modeled_baseline,
        observed_tokens,
        output_tokens,
        pattern,
        path,
        duration,
        output,
    );
}

pub(crate) fn cli_track_tree(original_tokens: usize, output_tokens: usize) {
    crate::core::tool_lifecycle::record_tree(original_tokens, output_tokens);
}

pub(crate) fn detect_project_root(args: &[String]) -> String {
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--root=")
            && !v.trim().is_empty()
        {
            return promote_to_git_root(v);
        }
        if let Some(v) = a.strip_prefix("--project-root=")
            && !v.trim().is_empty()
        {
            return promote_to_git_root(v);
        }
        if (a == "--root" || a == "--project-root")
            && let Some(v) = it.peek()
            && !v.starts_with("--")
            && !v.trim().is_empty()
        {
            return promote_to_git_root(v);
        }
    }
    let cwd = std::env::current_dir()
        .ok()
        .map_or_else(|| ".".to_string(), |p| p.to_string_lossy().to_string());
    promote_to_git_root(&cwd)
}

fn promote_to_git_root(path: &str) -> String {
    let mut p = std::path::Path::new(path);
    loop {
        if p.join(".git").exists() {
            return p.to_string_lossy().to_string();
        }
        match p.parent() {
            Some(parent) => p = parent,
            None => return path.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_tokens_cli;

    #[test]
    fn format_tokens_cli_scales_through_billions() {
        assert_eq!(format_tokens_cli(742), "742");
        assert_eq!(format_tokens_cli(2_500), "2.5K");
        assert_eq!(format_tokens_cli(3_400_000), "3.4M");
        // Must read as billions once a heavy user crosses 1B, not "1310.0M".
        assert_eq!(format_tokens_cli(1_310_000_000), "1.31B");
        assert_eq!(format_tokens_cli(1_500_000_000_000), "1.50T");
    }
}
