macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn pnpm_added_re() -> &'static regex::Regex {
    static_regex!(r"(\d+) packages? (?:are )?(?:installed|added|updated)")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    if command.contains("install") || command.contains("add") || command.contains("i ") {
        return Some(compress_install(output));
    }
    if command.contains("list") || command.contains("ls") {
        return Some(compress_list(output));
    }
    if command.contains("outdated") {
        return Some(compress_outdated(output));
    }
    if command.contains("run") || command.contains("exec") {
        return Some(compress_run(output));
    }
    if command.contains("test") {
        return Some(compress_test(output));
    }
    if command.contains("why") {
        return Some(compact_output(output, 10));
    }
    if command.contains("store") {
        return Some(compress_store(output));
    }
    None
}

fn compress_install(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut pkg_count = 0u32;
    if let Some(caps) = pnpm_added_re().captures(trimmed) {
        pkg_count = caps[1].parse().unwrap_or(0);
    }

    let progress_free: Vec<&str> = trimmed
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with("Progress:")
                && !t.starts_with("Already up to date")
                && !t.contains("Downloading")
                && !t.contains("fetched from")
        })
        .collect();

    if pkg_count > 0 {
        return format!("ok ({pkg_count} packages installed)");
    }
    if progress_free.len() <= 3 {
        return progress_free.join("\n");
    }
    format!(
        "ok\n{}",
        progress_free[progress_free.len() - 3..].join("\n")
    )
}

fn compress_list(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 5 {
        return output.to_string();
    }

    let _deps: Vec<&str> = lines
        .iter()
        .filter(|l| l.contains("dependencies:") || l.starts_with(' '))
        .copied()
        .collect();

    let top: Vec<String> = lines
        .iter()
        .filter(|l| {
            let trimmed = l.trim();
            !trimmed.is_empty()
                && (trimmed.starts_with('+')
                    || trimmed.starts_with("└")
                    || trimmed.starts_with("├"))
        })
        .map(|l| {
            l.replace("├──", "")
                .replace("└──", "")
                .replace("├─", "")
                .replace("└─", "")
                .trim()
                .to_string()
        })
        .collect();

    if !top.is_empty() {
        return format!("{} packages:\n{}", top.len(), top.join("\n"));
    }
    compact_output(output, 15)
}

fn compress_outdated(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 1 {
        return "all up-to-date".to_string();
    }

    let mut packages = Vec::new();
    for line in &lines[1..] {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            packages.push(format!("{}: {} → {}", parts[0], parts[1], parts[2]));
        }
    }

    if packages.is_empty() {
        return "all up-to-date".to_string();
    }
    format!("{} outdated:\n{}", packages.len(), packages.join("\n"))
}

fn compress_run(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('>')
        })
        .collect();

    if lines.len() <= 5 {
        return lines.join("\n");
    }
    let tail = &lines[lines.len() - 3..];
    format!("...({} lines)\n{}", lines.len(), tail.join("\n"))
}

fn compress_test(output: &str) -> String {
    compress_run(output)
}

fn compress_store(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }
    compact_output(trimmed, 5)
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
