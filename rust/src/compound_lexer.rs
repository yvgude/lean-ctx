/// Splits a compound shell command into segments separated by `&&`, `||`, `;`.
/// Pipes (`|`) are treated specially: only the left side of a pipe is eligible
/// for rewriting (the right side consumes output format and must stay raw).
///
/// Respects single quotes, double quotes, backtick-quotes, and `$(...)` subshells
/// so that operators inside quoted strings are not treated as separators.
///
/// Returns a `Vec<Segment>` where each entry is either a command segment or an
/// operator token that should be emitted verbatim.

#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    Command(String),
    Operator(String),
}

pub fn split_compound(input: &str) -> Vec<Segment> {
    let input = input.trim();
    if input.is_empty() {
        return vec![];
    }

    if contains_heredoc(input) {
        return vec![Segment::Command(input.to_string())];
    }

    let bytes = input.as_bytes();
    let mut segments: Vec<Segment> = Vec::new();
    let mut current = String::new();
    let mut i = 0;
    let len = bytes.len();

    while i < len {
        let ch = bytes[i] as char;

        match ch {
            '\'' => {
                current.push(ch);
                i += 1;
                while i < len && bytes[i] != b'\'' {
                    current.push(bytes[i] as char);
                    i += 1;
                }
                if i < len {
                    current.push('\'');
                    i += 1;
                }
            }
            '"' => {
                current.push(ch);
                i += 1;
                while i < len && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < len {
                        current.push('\\');
                        current.push(bytes[i + 1] as char);
                        i += 2;
                        continue;
                    }
                    current.push(bytes[i] as char);
                    i += 1;
                }
                if i < len {
                    current.push('"');
                    i += 1;
                }
            }
            '`' => {
                current.push(ch);
                i += 1;
                while i < len && bytes[i] != b'`' {
                    current.push(bytes[i] as char);
                    i += 1;
                }
                if i < len {
                    current.push('`');
                    i += 1;
                }
            }
            '$' if i + 1 < len && bytes[i + 1] == b'(' => {
                let start = i;
                i += 2;
                let mut depth = 1;
                while i < len && depth > 0 {
                    if bytes[i] == b'(' {
                        depth += 1;
                    } else if bytes[i] == b')' {
                        depth -= 1;
                    }
                    i += 1;
                }
                current.push_str(&input[start..i]);
            }
            '\\' if i + 1 < len => {
                current.push('\\');
                current.push(bytes[i + 1] as char);
                i += 2;
            }
            '&' if i + 1 < len && bytes[i + 1] == b'&' => {
                push_command(&mut segments, &current);
                current.clear();
                segments.push(Segment::Operator("&&".to_string()));
                i += 2;
            }
            '|' if i + 1 < len && bytes[i + 1] == b'|' => {
                push_command(&mut segments, &current);
                current.clear();
                segments.push(Segment::Operator("||".to_string()));
                i += 2;
            }
            '|' => {
                // Pipe: left side is a command, right side is NOT rewritten.
                // We emit the left command, the pipe operator, and the entire
                // rest of the input as a single opaque command segment.
                push_command(&mut segments, &current);
                current.clear();
                segments.push(Segment::Operator("|".to_string()));
                let rest = input[i + 1..].trim().to_string();
                if !rest.is_empty() {
                    segments.push(Segment::Command(rest));
                }
                return segments;
            }
            ';' => {
                push_command(&mut segments, &current);
                current.clear();
                segments.push(Segment::Operator(";".to_string()));
                i += 1;
            }
            _ => {
                current.push(ch);
                i += 1;
            }
        }
    }

    push_command(&mut segments, &current);
    segments
}

fn push_command(segments: &mut Vec<Segment>, cmd: &str) {
    let trimmed = cmd.trim();
    if !trimmed.is_empty() {
        segments.push(Segment::Command(trimmed.to_string()));
    }
}

fn contains_heredoc(input: &str) -> bool {
    input.contains("<<") || input.contains("$((")
}

/// Rewrites a compound command by applying a rewrite function to each command segment.
/// Operators and pipe-right-hand segments are preserved unchanged.
/// `rewrite_fn` receives a command string and returns `Some(rewritten)` if it should
/// be rewritten, or `None` to keep the original.
pub fn rewrite_compound<F>(input: &str, rewrite_fn: F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    let segments = split_compound(input);
    if segments.len() <= 1 {
        return None;
    }

    let mut any_rewritten = false;
    let mut result = String::new();
    let mut after_pipe = false;

    for seg in &segments {
        match seg {
            Segment::Operator(op) => {
                if op == "|" {
                    after_pipe = true;
                }
                if !result.is_empty() && !result.ends_with(' ') {
                    result.push(' ');
                }
                result.push_str(op);
                result.push(' ');
            }
            Segment::Command(cmd) => {
                if after_pipe {
                    result.push_str(cmd);
                } else if let Some(rewritten) = rewrite_fn(cmd) {
                    any_rewritten = true;
                    result.push_str(&rewritten);
                } else {
                    result.push_str(cmd);
                }
            }
        }
    }

    if any_rewritten {
        Some(result.trim().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_command() {
        let segs = split_compound("git status");
        assert_eq!(segs, vec![Segment::Command("git status".into())]);
    }

    #[test]
    fn and_chain() {
        let segs = split_compound("cd src && git status && echo done");
        assert_eq!(
            segs,
            vec![
                Segment::Command("cd src".into()),
                Segment::Operator("&&".into()),
                Segment::Command("git status".into()),
                Segment::Operator("&&".into()),
                Segment::Command("echo done".into()),
            ]
        );
    }

    #[test]
    fn pipe_stops_at_right() {
        let segs = split_compound("git log --oneline | grep fix");
        assert_eq!(
            segs,
            vec![
                Segment::Command("git log --oneline".into()),
                Segment::Operator("|".into()),
                Segment::Command("grep fix".into()),
            ]
        );
    }

    #[test]
    fn pipe_in_chain() {
        let segs = split_compound("cd src && git log | head -5");
        assert_eq!(
            segs,
            vec![
                Segment::Command("cd src".into()),
                Segment::Operator("&&".into()),
                Segment::Command("git log".into()),
                Segment::Operator("|".into()),
                Segment::Command("head -5".into()),
            ]
        );
    }

    #[test]
    fn semicolons() {
        let segs = split_compound("git add .; git commit -m 'fix'");
        assert_eq!(
            segs,
            vec![
                Segment::Command("git add .".into()),
                Segment::Operator(";".into()),
                Segment::Command("git commit -m 'fix'".into()),
            ]
        );
    }

    #[test]
    fn or_chain() {
        let segs = split_compound("git pull || echo failed");
        assert_eq!(
            segs,
            vec![
                Segment::Command("git pull".into()),
                Segment::Operator("||".into()),
                Segment::Command("echo failed".into()),
            ]
        );
    }

    #[test]
    fn quoted_ampersand_not_split() {
        let segs = split_compound("echo 'foo && bar'");
        assert_eq!(segs, vec![Segment::Command("echo 'foo && bar'".into())]);
    }

    #[test]
    fn double_quoted_pipe_not_split() {
        let segs = split_compound(r#"echo "hello | world""#);
        assert_eq!(
            segs,
            vec![Segment::Command(r#"echo "hello | world""#.into())]
        );
    }

    #[test]
    fn heredoc_kept_whole() {
        let segs = split_compound("cat <<EOF\nhello\nEOF && echo done");
        assert_eq!(
            segs,
            vec![Segment::Command(
                "cat <<EOF\nhello\nEOF && echo done".into()
            )]
        );
    }

    #[test]
    fn subshell_not_split() {
        let segs = split_compound("echo $(git status && echo ok)");
        assert_eq!(
            segs,
            vec![Segment::Command("echo $(git status && echo ok)".into())]
        );
    }

    #[test]
    fn rewrite_compound_and_chain() {
        let result = rewrite_compound("cd src && git status && echo done", |cmd| {
            if cmd.starts_with("git ") {
                Some(format!("rtk {cmd}"))
            } else {
                None
            }
        });
        assert_eq!(result, Some("cd src && rtk git status && echo done".into()));
    }

    #[test]
    fn rewrite_compound_pipe_preserves_right() {
        let result = rewrite_compound("git log | head -5", |cmd| {
            if cmd.starts_with("git ") {
                Some(format!("rtk {cmd}"))
            } else {
                None
            }
        });
        assert_eq!(result, Some("rtk git log | head -5".into()));
    }

    #[test]
    fn rewrite_compound_no_match_returns_none() {
        let result = rewrite_compound("cd src && echo done", |_| None);
        assert_eq!(result, None);
    }

    #[test]
    fn rewrite_single_command_returns_none() {
        let result = rewrite_compound("git status", |cmd| {
            if cmd.starts_with("git ") {
                Some(format!("rtk {cmd}"))
            } else {
                None
            }
        });
        assert_eq!(result, None);
    }
}
