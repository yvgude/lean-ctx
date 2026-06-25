//! Jujutsu (`jj`) output compression.
//!
//! `jj log` renders a two-line-per-commit graph (header with author/date, then
//! the description). We collapse each commit to `change_id commit_id desc` and
//! drop author/timestamp + graph art. `jj status`/`diff` keep the file-change
//! lines and the working-copy/parent summary.

use crate::core::compressor::strip_ansi;

const GRAPH: &str = "@○◉●×◇~│├╮╭╯╰┐└┌┘ \t";

#[must_use]
pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("jj: ok".to_string());
    }
    if cmd.contains("log") {
        return Some(compress_log(trimmed));
    }
    if cmd.contains("status") || cmd.contains(" st") || cmd.contains("diff") {
        return compress_status(trimmed);
    }
    Some(fallback(trimmed))
}

fn compress_log(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some((cid, commit)) = parse_header(lines[i]) {
            let desc = lines
                .get(i + 1)
                .map(|l| strip_graph(l))
                .filter(|d| !d.is_empty() && *d != "(no description set)")
                .unwrap_or("(no description set)");
            out.push(format!("{cid} {commit} {desc}").trim().to_string());
            i += 2;
        } else {
            i += 1;
        }
    }
    if out.is_empty() {
        return fallback(output);
    }
    out.join("\n")
}

fn compress_status(output: &str) -> Option<String> {
    let mut kept: Vec<String> = Vec::new();
    for raw in output.lines() {
        let line = strip_ansi(raw);
        let line = line.trim_end();
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let is_file = matches!(t.chars().next(), Some('M' | 'A' | 'D' | 'R' | 'C'))
            && t.chars().nth(1) == Some(' ');
        // Keep working-copy/parent summary lines but drop the
        // "Working copy changes:" section header.
        let is_summary =
            (t.starts_with("Working copy") || t.starts_with("Parent commit")) && t.contains(": ");
        if is_file || is_summary {
            kept.push(t.to_string());
        }
    }
    if kept.is_empty() {
        return None;
    }
    Some(kept.join("\n"))
}

fn parse_header(line: &str) -> Option<(String, String)> {
    let body = strip_graph(line);
    if !has_date(body) {
        return None;
    }
    let tokens: Vec<&str> = body.split_whitespace().collect();
    let cid = tokens.first()?;
    let commit = tokens.iter().rev().find(|t| is_hex8(t))?;
    Some((cid.to_string(), commit.to_string()))
}

fn strip_graph(line: &str) -> &str {
    line.trim_start_matches(|c| GRAPH.contains(c)).trim_end()
}

fn has_date(s: &str) -> bool {
    s.split_whitespace().any(|t| {
        let p: Vec<&str> = t.split('-').collect();
        p.len() == 3 && p[0].len() == 4 && p.iter().all(|x| x.chars().all(|c| c.is_ascii_digit()))
    })
}

fn is_hex8(t: &str) -> bool {
    t.len() >= 7 && t.len() <= 12 && t.chars().all(|c| c.is_ascii_hexdigit())
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

    const LOG: &str = "@  qpvuntsm user@host.com 2024-01-01 12:00:00 1234abcd\n│  add feature x\n○  zzzzmmmm user@host.com 2024-01-01 11:00:00 main 5678efab\n│  initial commit\n~\n";

    #[test]
    fn log_collapses_commits() {
        let r = compress("jj log", LOG).unwrap();
        assert!(r.contains("qpvuntsm 1234abcd add feature x"), "{r}");
        assert!(r.contains("zzzzmmmm 5678efab initial commit"), "{r}");
        assert!(!r.contains("user@host"), "drops author: {r}");
        assert!(!r.contains("12:00:00"), "drops time: {r}");
    }

    #[test]
    fn status_keeps_file_changes() {
        let st = "Working copy changes:\nM src/main.rs\nA src/new.rs\nWorking copy : qpvuntsm 1234abcd (no description set)\nParent commit: zzzzmmmm 5678efab main | initial";
        let r = compress("jj status", st).unwrap();
        assert!(r.contains("M src/main.rs"), "{r}");
        assert!(r.contains("Parent commit"), "{r}");
        assert!(
            !r.contains("Working copy changes:"),
            "drops header noise: {r}"
        );
    }

    #[test]
    fn empty_is_ok() {
        assert_eq!(compress("jj log", "").unwrap(), "jj: ok");
    }
}
