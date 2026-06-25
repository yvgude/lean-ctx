//! mise (dev tool version manager) output compression.
//!
//! `mise ls`/`list` prints `tool  version  source-path` rows — we keep
//! tool + version and drop the config source path. `mise install` keeps the
//! installed/failed lines and drops download progress.

use crate::core::compressor::strip_ansi;

#[must_use]
pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("mise: ok".to_string());
    }
    if cmd.contains(" ls") || cmd.contains(" list") {
        return Some(compress_ls(trimmed));
    }
    if cmd.contains("install") || cmd.contains("use") || cmd.contains("upgrade") {
        return Some(compress_install(trimmed));
    }
    Some(fallback(trimmed))
}

fn compress_ls(output: &str) -> String {
    let mut rows: Vec<String> = Vec::new();
    for raw in output.lines() {
        let line = strip_ansi(raw);
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 2 {
            rows.push(format!("{} {}", cols[0], cols[1]));
        }
    }
    if rows.is_empty() {
        return fallback(output);
    }
    format!("mise: {} tool(s)\n{}", rows.len(), rows.join("\n"))
}

fn compress_install(output: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    for raw in output.lines() {
        let line = strip_ansi(raw);
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let l = t.to_ascii_lowercase();
        if l.contains("installed")
            || l.contains("installing")
            || l.contains("failed")
            || l.contains("error")
            || t.contains('✓')
        {
            kept.push(t.to_string());
        }
    }
    if kept.is_empty() {
        return "mise: ok".to_string();
    }
    kept.join("\n")
}

fn fallback(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let n = lines.len().min(10);
    let mut s = lines[..n].join("\n");
    if lines.len() > n {
        s.push_str(&format!("\n... (+{} lines)", lines.len() - n));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ls_drops_source_path() {
        let out = "node    20.10.0  ~/.config/mise/config.toml\npython  3.12.0   ~/.tool-versions\nrust    1.75.0   ~/.config/mise/config.toml";
        let r = compress("mise ls", out).unwrap();
        assert!(r.contains("3 tool(s)"), "{r}");
        assert!(r.contains("node 20.10.0"), "{r}");
        assert!(!r.contains("config.toml"), "drops source path: {r}");
    }

    #[test]
    fn install_keeps_status() {
        let out = "mise downloading node@20.10.0\nmise extracting node@20.10.0\nmise node@20.10.0 ✓ installed";
        let r = compress("mise install node", out).unwrap();
        assert!(r.contains("installed"), "{r}");
        assert!(!r.contains("extracting"), "drops progress: {r}");
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("mise ls", "").unwrap(), "mise: ok");
    }
}
