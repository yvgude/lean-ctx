pub fn resolve_portable_binary() -> String {
    let which_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = std::process::Command::new(which_cmd)
        .arg("lean-ctx")
        .stderr(std::process::Stdio::null())
        .output()
    {
        if output.status.success() {
            let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !raw.is_empty() {
                let path = pick_best_binary_line(&raw);
                return sanitize_exe_path(path);
            }
        }
    }
    let path = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "lean-ctx".to_string());
    sanitize_exe_path(path)
}

/// On Windows, `where lean-ctx` returns multiple lines (e.g. `lean-ctx` and
/// `lean-ctx.cmd`). Pick the `.cmd`/`.exe` variant if available, otherwise
/// the first line.
fn pick_best_binary_line(raw: &str) -> String {
    let lines: Vec<&str> = raw
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() <= 1 {
        return lines.first().unwrap_or(&"lean-ctx").to_string();
    }
    if cfg!(windows) {
        if let Some(cmd) = lines
            .iter()
            .find(|l| l.ends_with(".cmd") || l.ends_with(".exe"))
        {
            return cmd.to_string();
        }
    }
    lines[0].to_string()
}

fn sanitize_exe_path(path: String) -> String {
    path.trim_end_matches(" (deleted)").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_returns_as_is() {
        assert_eq!(
            pick_best_binary_line("/usr/bin/lean-ctx"),
            "/usr/bin/lean-ctx"
        );
    }

    #[test]
    fn multiline_returns_first_line() {
        let raw = "/usr/bin/lean-ctx\n/usr/local/bin/lean-ctx";
        let result = pick_best_binary_line(raw);
        assert_eq!(result, "/usr/bin/lean-ctx");
    }

    #[test]
    fn empty_returns_fallback() {
        assert_eq!(pick_best_binary_line(""), "lean-ctx");
    }

    #[test]
    fn sanitize_removes_deleted_suffix() {
        assert_eq!(
            sanitize_exe_path("/usr/bin/lean-ctx (deleted)".to_string()),
            "/usr/bin/lean-ctx"
        );
    }

    #[test]
    fn whitespace_lines_are_filtered() {
        let raw = "  /usr/bin/lean-ctx  \n  \n  /usr/local/bin/lean-ctx  ";
        assert_eq!(pick_best_binary_line(raw), "/usr/bin/lean-ctx");
    }
}
