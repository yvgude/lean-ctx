macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn uv_installed_line_re() -> &'static regex::Regex {
    static_regex!(r"^\s*\+\s+(\S+)")
}
fn uv_resolved_re() -> &'static regex::Regex {
    static_regex!(r"(?i)^(Resolved|Prepared|Installed|Audited)\s+")
}
fn poetry_installing_re() -> &'static regex::Regex {
    static_regex!(r"(?i)^\s*-\s+Installing\s+(\S+)\s+\(([^)]+)\)")
}
fn poetry_updating_re() -> &'static regex::Regex {
    static_regex!(r"(?i)^\s*-\s+Updating\s+(\S+)\s+\(([^)]+)\)")
}
fn pip_style_success_re() -> &'static regex::Regex {
    static_regex!(r"(?i)Successfully installed\s+(.+)")
}
fn percent_bar_re() -> &'static regex::Regex {
    static_regex!(r"\d+%\|")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    let cl = command.trim().to_ascii_lowercase();
    if cl.starts_with("poetry ") {
        let sub = cl.split_whitespace().nth(1).unwrap_or("");
        return match sub {
            "install" | "add" => Some(compress_poetry(output, false)),
            "update" => Some(compress_poetry(output, true)),
            _ => None,
        };
    }
    let parts: Vec<&str> = cl.split_whitespace().collect();
    if parts.len() >= 2 && parts[0] == "uv" && parts[1] == "sync" {
        return Some(compress_uv(output));
    }
    if parts.len() >= 3 && parts[0] == "uv" && parts[1] == "pip" && parts[2] == "install" {
        return Some(compress_uv(output));
    }
    if cl.starts_with("conda ") || cl.starts_with("mamba ") {
        let sub = parts.get(1).copied().unwrap_or("");
        return match sub {
            "install" | "create" | "update" | "remove" => Some(compress_conda(output)),
            "list" => Some(compress_conda_list(output)),
            "info" => Some(compress_conda_info(output)),
            _ => None,
        };
    }
    if cl.starts_with("pipx ") {
        return Some(compress_pipx(output));
    }
    None
}

fn is_download_noise(line: &str) -> bool {
    let t = line.trim();
    let tl = t.to_ascii_lowercase();
    if tl.contains("downloading ")
        || tl.starts_with("downloading [")
        || tl.contains("kiB/s")
        || tl.contains("kib/s")
        || tl.contains("mib/s")
        || tl.contains('%') && (tl.contains("eta") || tl.contains('|') || tl.contains("of "))
    {
        return true;
    }
    if tl.starts_with("progress ") && tl.contains('/') {
        return true;
    }
    if percent_bar_re().is_match(t) {
        return true;
    }
    false
}

fn compress_poetry(output: &str, prefer_update: bool) -> String {
    let mut packages = Vec::new();
    let mut errors = Vec::new();

    for line in output.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() || is_download_noise(t) {
            continue;
        }
        let trim = t.trim();
        let tl = trim.to_ascii_lowercase();

        if prefer_update {
            if let Some(caps) = poetry_updating_re().captures(trim) {
                packages.push(format!("{} {}", &caps[1], &caps[2]));
                continue;
            }
        }
        if let Some(caps) = poetry_installing_re().captures(trim) {
            packages.push(format!("{} {}", &caps[1], &caps[2]));
            continue;
        }
        if !prefer_update {
            if let Some(caps) = poetry_updating_re().captures(trim) {
                packages.push(format!("{} {}", &caps[1], &caps[2]));
                continue;
            }
        }

        if tl.contains("error")
            && (tl.contains("because") || tl.contains("could not") || tl.contains("failed"))
        {
            errors.push(trim.to_string());
        }
        if tl.starts_with("solverproblemerror") || tl.contains("version solving failed") {
            errors.push(trim.to_string());
        }
    }

    let mut parts = Vec::new();
    if !packages.is_empty() {
        parts.push(format!("{} package(s):", packages.len()));
        parts.extend(packages.into_iter().map(|p| format!("  {p}")));
    }
    if !errors.is_empty() {
        parts.push(format!("{} error line(s):", errors.len()));
        parts.extend(errors.into_iter().take(15).map(|e| format!("  {e}")));
    }

    if parts.is_empty() {
        fallback_compact(output)
    } else {
        parts.join("\n")
    }
}

fn compress_uv(output: &str) -> String {
    let mut summary = Vec::new();
    let mut installed = Vec::new();
    let mut errors = Vec::new();

    for line in output.lines() {
        let t = line.trim_end();
        if t.trim().is_empty() || is_download_noise(t) {
            continue;
        }
        let trim = t.trim();
        let tl = trim.to_ascii_lowercase();

        if uv_resolved_re().is_match(trim) {
            summary.push(trim.to_string());
            continue;
        }
        if let Some(caps) = uv_installed_line_re().captures(trim) {
            installed.push(caps[1].to_string());
            continue;
        }
        if let Some(caps) = pip_style_success_re().captures(trim) {
            let pkgs: Vec<&str> = caps[1].split_whitespace().collect();
            summary.push(format!("Successfully installed {} packages", pkgs.len()));
            for p in pkgs.into_iter().take(30) {
                installed.push(p.to_string());
            }
            continue;
        }

        if tl.contains("error:")
            || tl.starts_with("error:")
            || tl.contains("failed to")
            || tl.contains("resolution failed")
        {
            errors.push(trim.to_string());
        }
    }

    let mut parts = Vec::new();
    parts.extend(summary);
    if !installed.is_empty() {
        parts.push(format!("+ {} package(s):", installed.len()));
        for p in installed.into_iter().take(40) {
            parts.push(format!("  {p}"));
        }
    }
    if !errors.is_empty() {
        parts.push(format!("{} error line(s):", errors.len()));
        parts.extend(errors.into_iter().take(15).map(|e| format!("  {e}")));
    }

    if parts.is_empty() {
        fallback_compact(output)
    } else {
        parts.join("\n")
    }
}

fn compress_conda(output: &str) -> String {
    let mut packages = Vec::new();
    let mut errors = Vec::new();
    let mut action = String::new();

    for line in output.lines() {
        let t = line.trim();
        if t.is_empty() || is_download_noise(t) {
            continue;
        }
        let tl = t.to_ascii_lowercase();

        if tl.starts_with("the following packages will be")
            || tl.starts_with("the following new packages")
        {
            action = t.to_string();
            continue;
        }
        if t.starts_with("  ") && t.contains("::") {
            packages.push(t.trim().to_string());
            continue;
        }
        if t.starts_with("  ") && !t.starts_with("   ") && packages.is_empty() {
            let name = t.split_whitespace().next().unwrap_or(t);
            packages.push(name.to_string());
            continue;
        }
        if tl.contains("error")
            || tl.contains("conflictingerror")
            || tl.contains("unsatisfiableerror")
        {
            errors.push(t.to_string());
        }
    }

    let mut parts = Vec::new();
    if !action.is_empty() {
        parts.push(action);
    }
    if !packages.is_empty() {
        parts.push(format!("{} package(s)", packages.len()));
        for p in packages.iter().take(20) {
            parts.push(format!("  {p}"));
        }
        if packages.len() > 20 {
            parts.push(format!("  ... +{} more", packages.len() - 20));
        }
    }
    if !errors.is_empty() {
        parts.push(format!("{} error(s):", errors.len()));
        parts.extend(errors.into_iter().take(10).map(|e| format!("  {e}")));
    }

    if parts.is_empty() {
        fallback_compact(output)
    } else {
        parts.join("\n")
    }
}

fn compress_conda_list(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return "no packages".to_string();
    }
    if lines.len() <= 10 {
        return lines.join("\n");
    }
    format!(
        "{} packages installed\n{}\n... +{} more",
        lines.len(),
        lines[..10].join("\n"),
        lines.len() - 10
    )
}

fn compress_conda_info(output: &str) -> String {
    let important = [
        "active environment",
        "conda version",
        "platform",
        "python version",
    ];
    let mut info = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        for key in &important {
            if trimmed.to_lowercase().starts_with(key) {
                info.push(trimmed.to_string());
                break;
            }
        }
    }
    if info.is_empty() {
        fallback_compact(output)
    } else {
        info.join("\n")
    }
}

fn compress_pipx(output: &str) -> String {
    let mut parts = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        if t.is_empty() || is_download_noise(t) {
            continue;
        }
        let tl = t.to_ascii_lowercase();
        if tl.contains("installed package")
            || tl.contains("done!")
            || tl.contains("these apps are now")
        {
            parts.push(t.to_string());
        }
    }
    if parts.is_empty() {
        fallback_compact(output)
    } else {
        parts.join("\n")
    }
}

fn fallback_compact(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty() && !is_download_noise(l))
        .collect();
    if lines.is_empty() {
        return "ok".to_string();
    }
    let max = 12usize;
    if lines.len() <= max {
        return lines.join("\n");
    }
    format!(
        "{}\n... ({} more lines)",
        lines[..max].join("\n"),
        lines.len() - max
    )
}
