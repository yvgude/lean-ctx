macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn pr_line_re() -> &'static regex::Regex {
    static_regex!(r"#(\d+)\s+(.+?)\s{2,}(\S+)\s+(\S+)")
}
fn issue_line_re() -> &'static regex::Regex {
    static_regex!(r"#(\d+)\s+(.+?)\s{2,}")
}
fn pr_created_re() -> &'static regex::Regex {
    static_regex!(r"https://github\.com/\S+/pull/(\d+)")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    if command.contains(" api ") || command.starts_with("gh api") {
        return None;
    }
    if command.contains("pr") {
        if command.contains("diff") {
            return None;
        }
        if command.contains("list") {
            return Some(compress_pr_list(output));
        }
        if command.contains("view") {
            return Some(compress_pr_view(output));
        }
        if command.contains("create") {
            return Some(compress_pr_create(output));
        }
        if command.contains("merge") {
            return Some(compress_simple_action(output, "merged"));
        }
        if command.contains("close") {
            return Some(compress_simple_action(output, "closed"));
        }
        if command.contains("checkout") || command.contains("co") {
            return Some(compress_simple_action(output, "checked out"));
        }
    }
    if command.contains("issue") {
        if command.contains("list") {
            return Some(compress_issue_list(output));
        }
        if command.contains("view") {
            return Some(compress_issue_view(output));
        }
        if command.contains("create") {
            return Some(compress_simple_action(output, "created"));
        }
    }
    if command.contains("run") {
        if command.contains("list") {
            return Some(compress_run_list(output));
        }
        if command.contains("view") {
            return Some(compress_run_view(output));
        }
    }
    if command.contains("repo") {
        return Some(compress_repo(output));
    }
    if command.contains("release") {
        return Some(compress_release(output));
    }

    None
}

fn compress_pr_list(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed.contains("no pull requests") {
        return "no PRs".to_string();
    }

    let mut prs = Vec::new();
    for line in trimmed.lines() {
        if let Some(caps) = pr_line_re().captures(line) {
            let num = &caps[1];
            let title = caps[2].trim();
            let branch = &caps[3];
            prs.push(format!("#{num} {title} ({branch})"));
        } else {
            let l = line.trim();
            if !l.is_empty() && l.starts_with('#') {
                prs.push(l.to_string());
            }
        }
    }

    if prs.is_empty() {
        return compact_output(trimmed, 10);
    }
    format!("{} PRs:\n{}", prs.len(), prs.join("\n"))
}

fn compress_pr_view(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 5 {
        return output.to_string();
    }

    let mut title = String::new();
    let mut state = String::new();
    let mut labels = Vec::new();
    let mut checks = Vec::new();

    for line in &lines {
        let l = line.trim();
        if l.starts_with("title:") || (title.is_empty() && l.starts_with('#')) {
            title = l.replace("title:", "").replace('#', "").trim().to_string();
        }
        if l.starts_with("state:") {
            state = l.replace("state:", "").trim().to_string();
        }
        if l.starts_with("labels:") {
            labels = l
                .replace("labels:", "")
                .split(',')
                .map(|s| s.trim().to_string())
                .collect();
        }
        if l.contains("✓") || l.contains("✗") || l.contains("pass") || l.contains("fail") {
            checks.push(l.to_string());
        }
    }

    let mut parts = Vec::new();
    if !title.is_empty() {
        parts.push(title);
    }
    if !state.is_empty() {
        parts.push(format!("state: {state}"));
    }
    if !labels.is_empty() {
        parts.push(format!("labels: {}", labels.join(", ")));
    }
    if !checks.is_empty() && checks.len() <= 5 {
        parts.push(format!("checks: {}", checks.join("; ")));
    }

    if parts.is_empty() {
        return compact_output(output, 10);
    }
    parts.join("\n")
}

fn compress_pr_create(output: &str) -> String {
    if let Some(caps) = pr_created_re().captures(output) {
        return format!("#{} created", &caps[1]);
    }
    let trimmed = output.trim();
    if trimmed.contains("http") {
        for line in trimmed.lines() {
            if line.contains("http") {
                return format!("created: {}", line.trim());
            }
        }
    }
    compact_output(trimmed, 3)
}

fn compress_issue_list(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed.contains("no issues") {
        return "no issues".to_string();
    }

    let mut issues = Vec::new();
    for line in trimmed.lines() {
        if let Some(caps) = issue_line_re().captures(line) {
            let num = &caps[1];
            let title = caps[2].trim();
            issues.push(format!("#{num} {title}"));
        } else {
            let l = line.trim();
            if !l.is_empty() && l.starts_with('#') {
                issues.push(l.to_string());
            }
        }
    }

    if issues.is_empty() {
        return compact_output(trimmed, 10);
    }
    format!("{} issues:\n{}", issues.len(), issues.join("\n"))
}

fn compress_issue_view(output: &str) -> String {
    // #884: an issue's body is the payload; the metadata fields above it are
    // boilerplate. Head-only truncation (`compact_output`) dropped the body
    // wholesale on any issue longer than the field block, forcing a second
    // `--json body` call. Keep head AND tail so the body survives.
    compact_head_tail(output, 40, 40)
}

fn compress_run_list(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "no runs".to_string();
    }

    let mut runs = Vec::new();
    for line in trimmed.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with("STATUS") || l.starts_with("--") {
            continue;
        }
        if l.contains("completed")
            || l.contains("in_progress")
            || l.contains("queued")
            || l.contains("failure")
            || l.contains("success")
        {
            runs.push(l.to_string());
        }
    }

    if runs.is_empty() {
        return compact_output(trimmed, 10);
    }
    format!("{} runs:\n{}", runs.len(), runs.join("\n"))
}

fn compress_run_view(output: &str) -> String {
    // #884: a run's failure summary lives at the tail — head-only truncation hid
    // exactly the part the agent came for. Keep head AND tail.
    compact_head_tail(output, 40, 40)
}

fn compress_repo(output: &str) -> String {
    compact_output(output, 10)
}

fn compress_release(output: &str) -> String {
    compact_output(output, 10)
}

fn compress_simple_action(output: &str, action: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return format!("ok ({action})");
    }
    for line in trimmed.lines() {
        if line.contains("http") || line.contains('#') {
            return format!("{action}: {}", line.trim());
        }
    }
    action.to_string()
}

fn compact_output(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= max {
        return lines.join("\n");
    }
    format!(
        "{}\n... ({} more lines)",
        lines[..max].join("\n"),
        lines.len() - max
    )
}

/// Head+tail compaction (#884): keep the first `head` and last `tail` non-empty
/// lines, eliding only the middle. Unlike [`compact_output`] (head-only), this
/// never drops the tail — used for single-record `view` output whose payload (an
/// issue/PR body, a run's failure summary) follows the metadata header.
fn compact_head_tail(text: &str, head: usize, tail: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= head + tail {
        return lines.join("\n");
    }
    let omitted = lines.len() - head - tail;
    format!(
        "{}\n... ({omitted} more lines)\n{}",
        lines[..head].join("\n"),
        lines[lines.len() - tail..].join("\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_view_keeps_body_not_just_metadata() {
        // gh issue view: a metadata field block then the body. Head-only
        // truncation (`compact_output(_, 15)`) dropped the body entirely (#884).
        let mut out = String::new();
        for f in [
            "title:\tSomething broke",
            "state:\tOPEN",
            "author:\tandig",
            "labels:",
            "comments:\t0",
            "assignees:",
            "projects:",
            "milestone:",
            "number:\t875",
            "--",
        ] {
            out.push_str(f);
            out.push('\n');
        }
        for i in 0..40 {
            out.push_str(&format!("body line {i} - the actual payload\n"));
        }
        let compressed = compress_issue_view(&out);
        assert!(
            compressed.contains("body line 39 - the actual payload"),
            "the end of the body must survive compaction: {compressed}"
        );
        assert!(
            compressed.contains("title:\tSomething broke"),
            "metadata head must still be present: {compressed}"
        );
    }

    #[test]
    fn head_tail_returns_small_output_verbatim() {
        assert_eq!(compact_head_tail("a\nb\nc\n", 40, 40), "a\nb\nc");
    }

    #[test]
    fn head_tail_elides_only_the_middle() {
        let lines: String = (0..200).map(|i| format!("L{i}\n")).collect();
        let out = compact_head_tail(&lines, 5, 5);
        assert!(out.contains("L0") && out.contains("L4"));
        assert!(out.contains("L195") && out.contains("L199"));
        assert!(out.contains("... (190 more lines)"));
        assert!(!out.contains("L100"));
    }
}
