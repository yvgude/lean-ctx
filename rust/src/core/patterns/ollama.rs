//! Ollama CLI output compression.
//!
//! Handles `ollama list`/`ps` (drop the low-value ID hash column, prefix a
//! model count) and `ollama pull`/`push` (strip download progress bars, keep
//! the final status + layer count). Content commands (`run`, `chat`, `serve`,
//! `show`) are left untouched — their output is the model's answer, not noise.

use crate::core::compressor::strip_ansi;

pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ollama: ok".to_string());
    }

    if cmd.contains(" list") || cmd.contains(" ls") {
        return Some(compress_table(trimmed, "model(s)"));
    }
    if cmd.contains(" ps") {
        return Some(compress_table(trimmed, "running"));
    }
    if cmd.contains(" pull") || cmd.contains(" push") {
        return Some(compress_transfer(trimmed));
    }

    // run/chat/serve/show emit model content — never compress.
    None
}

/// Drop the `ID` column (a low-value 12-char hash) and the header row, prefix
/// a count.
fn compress_table(output: &str, noun: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 2 {
        return output.to_string();
    }
    let header = split_cols(lines[0]);
    let drop = header.iter().position(|c| c.eq_ignore_ascii_case("ID"));

    let mut rows: Vec<String> = Vec::new();
    for line in &lines[1..] {
        let mut cols = split_cols(line);
        if let Some(i) = drop
            && i < cols.len()
        {
            cols.remove(i);
        }
        rows.push(cols.join("  "));
    }
    format!("ollama: {} {}\n{}", rows.len(), noun, rows.join("\n"))
}

/// Strip per-layer progress bars from `pull`/`push`, keep the final status.
fn compress_transfer(output: &str) -> String {
    let mut layers = 0usize;
    let mut success = false;
    let mut errors: Vec<String> = Vec::new();

    for raw in output.lines() {
        let line = strip_ansi(raw);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if (line.starts_with("pulling") || line.starts_with("pushing")) && line.contains('%') {
            layers += 1;
        } else if line == "success" {
            success = true;
        } else if line.contains("Error") || line.contains("error") {
            errors.push(line.to_string());
        }
    }

    if !errors.is_empty() {
        return format!("ollama: FAILED\n  {}", errors.join("\n  "));
    }
    if success {
        return format!("ollama: success ({layers} layers)");
    }
    format!("ollama: {layers} layers")
}

/// Split a table row on runs of 2+ spaces (columns may contain single spaces,
/// e.g. "2.0 GB" or "3 days ago").
fn split_cols(line: &str) -> Vec<String> {
    let mut cols = Vec::new();
    let mut cur = String::new();
    let mut spaces = 0;
    for ch in line.trim().chars() {
        if ch == ' ' {
            spaces += 1;
            continue;
        }
        if spaces >= 2 && !cur.is_empty() {
            cols.push(std::mem::take(&mut cur));
        } else if spaces == 1 && !cur.is_empty() {
            cur.push(' ');
        }
        spaces = 0;
        cur.push(ch);
    }
    if !cur.is_empty() {
        cols.push(cur);
    }
    cols
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "NAME                    ID              SIZE      MODIFIED\nllama3.2:latest         a80c4f17acd5    2.0 GB    3 days ago\nqwen2.5-coder:7b        2b0496514337    4.7 GB    2 weeks ago\n";

    #[test]
    fn list_drops_id_keeps_name_size() {
        let r = compress("ollama list", LIST).unwrap();
        assert!(r.contains("2 model(s)"), "{r}");
        assert!(r.contains("llama3.2:latest"), "{r}");
        assert!(r.contains("2.0 GB"), "{r}");
        assert!(r.contains("3 days ago"), "keeps multi-word column: {r}");
        assert!(!r.contains("a80c4f17acd5"), "drops ID hash: {r}");
    }

    #[test]
    fn pull_collapses_progress() {
        let out = "pulling manifest\npulling aabbccdd... 100% ▕████████▏ 2.0 GB\npulling 1234efgh... 100% ▕██▏ 1.2 KB\nverifying sha256 digest\nwriting manifest\nsuccess\n";
        let r = compress("ollama pull llama3.2", out).unwrap();
        assert_eq!(r, "ollama: success (2 layers)");
    }

    #[test]
    fn run_is_not_compressed() {
        assert!(
            compress(
                "ollama run llama3.2 'hi'",
                "The capital of France is Paris."
            )
            .is_none()
        );
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("ollama list", "").unwrap(), "ollama: ok");
    }
}
