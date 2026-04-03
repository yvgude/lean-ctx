//! `lean-ctx report-issue` — collects diagnostics and creates a GitHub issue.

use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPO: &str = "yvgude/lean-ctx";
const BOLD: &str = "\x1b[1m";
const RST: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";

pub fn run(args: &[String]) {
    let title = extract_flag(args, "--title");
    let description = extract_flag(args, "--description");
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let include_tee = args.iter().any(|a| a == "--include-tee");

    println!("{BOLD}lean-ctx report-issue{RST}\n");

    let title = title.unwrap_or_else(|| prompt_input("Issue title"));
    if title.trim().is_empty() {
        eprintln!("Title is required. Aborting.");
        return;
    }
    let description = description.unwrap_or_else(|| prompt_input("Describe the problem"));

    println!("\n{DIM}Collecting diagnostics...{RST}");
    let body = build_report_body(&title, &description, include_tee);

    println!("\n{BOLD}=== Preview ==={RST}\n");
    let preview: String = body.chars().take(2000).collect();
    println!("{preview}");
    if body.len() > 2000 {
        println!("{DIM}... ({} more characters){RST}", body.len() - 2000);
    }

    if dry_run {
        println!("\n{YELLOW}--dry-run: not submitting.{RST}");
        if let Some(dir) = lean_ctx_dir() {
            let path = dir.join("last-report.md");
            let _ = std::fs::write(&path, &body);
            println!("Report saved to {}", path.display());
        }
        return;
    }

    println!("\n{BOLD}Submit this as a GitHub issue to {REPO}?{RST} [y/N]");
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    if !answer.trim().eq_ignore_ascii_case("y") {
        println!("Aborted.");
        if let Some(dir) = lean_ctx_dir() {
            let path = dir.join("last-report.md");
            let _ = std::fs::write(&path, &body);
            println!("Report saved to {}", path.display());
        }
        return;
    }

    if try_gh_cli(&title, &body) {
        return;
    }
    try_ureq_api(&title, &body);
}

fn build_report_body(_title: &str, description: &str, include_tee: bool) -> String {
    let mut sections = Vec::new();

    sections.push(format!("## Description\n\n{description}"));
    sections.push(section_environment());
    sections.push(section_configuration());
    sections.push(section_mcp_status());
    sections.push(section_tool_calls());
    sections.push(section_session());
    sections.push(section_performance());
    sections.push(section_slow_commands());
    sections.push(section_tee_logs(include_tee));
    sections.push(section_project_context());

    let body = sections.join("\n\n---\n\n");
    anonymize_report(&body)
}

// ── Section Builders ──────────────────────────────────────────────────────

fn section_environment() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "unknown".into());
    let ide = detect_ide();

    format!(
        "## Environment\n\n\
         | Field | Value |\n|---|---|\n\
         | lean-ctx | {VERSION} |\n\
         | OS | {os} {arch} |\n\
         | Shell | {shell} |\n\
         | IDE | {ide} |"
    )
}

fn section_configuration() -> String {
    let mut out = String::from("## Configuration\n\n```toml\n");
    if let Some(dir) = lean_ctx_dir() {
        let config_path = dir.join("config.toml");
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            let clean = mask_secrets(&content);
            out.push_str(&clean);
        } else {
            out.push_str("# config.toml not found — using defaults");
        }
    }
    out.push_str("\n```");
    out
}

fn section_mcp_status() -> String {
    let mut lines = vec!["## MCP Integration Status\n".to_string()];

    let binary_ok = which_lean_ctx().is_some();
    lines.push(format!(
        "- Binary on PATH: {}",
        if binary_ok { "yes" } else { "no" }
    ));

    let hooks = check_shell_hooks();
    lines.push(format!("- Shell hooks: {hooks}"));

    let ides = check_mcp_configs();
    lines.push(format!("- MCP configured for: {ides}"));

    lines.join("\n")
}

fn section_tool_calls() -> String {
    let mut out = String::from("## Recent Tool Calls\n\n```\n");
    if let Some(dir) = lean_ctx_dir() {
        let log_path = dir.join("tool-calls.log");
        if let Ok(content) = std::fs::read_to_string(&log_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(20);
            for line in &lines[start..] {
                out.push_str(line);
                out.push('\n');
            }
        } else {
            out.push_str("# No tool call log found\n");
        }
    }
    out.push_str("```");
    out
}

fn section_session() -> String {
    let mut out = String::from("## Session State\n\n");
    if let Some(dir) = lean_ctx_dir() {
        let latest = dir.join("sessions").join("latest.json");
        if let Ok(content) = std::fs::read_to_string(&latest) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(task) = val.get("task") {
                    out.push_str(&format!(
                        "- Task: {}\n",
                        task.get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("-")
                    ));
                }
                if let Some(stats) = val.get("stats") {
                    out.push_str(&format!("- Stats: {}\n", stats));
                }
                if let Some(files) = val.get("files_touched").and_then(|f| f.as_object()) {
                    out.push_str(&format!("- Files touched: {}\n", files.len()));
                }
            }
        } else {
            out.push_str("No active session found.\n");
        }
    }
    out
}

fn section_performance() -> String {
    let mut out = String::from("## Performance Metrics\n\n");
    if let Some(dir) = lean_ctx_dir() {
        let mcp_live = dir.join("mcp-live.json");
        if let Ok(content) = std::fs::read_to_string(&mcp_live) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                let fields = [
                    "cep_score",
                    "cache_utilization",
                    "compression_rate",
                    "tokens_saved",
                    "tokens_original",
                    "tool_calls",
                ];
                out.push_str("| Metric | Value |\n|---|---|\n");
                for field in fields {
                    if let Some(v) = val.get(field) {
                        out.push_str(&format!("| {field} | {v} |\n"));
                    }
                }
            }
        }

        let stats_path = dir.join("stats.json");
        if let Ok(content) = std::fs::read_to_string(&stats_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(cmds) = val.get("commands").and_then(|c| c.as_object()) {
                    let mut top: Vec<_> = cmds
                        .iter()
                        .filter_map(|(k, v)| {
                            v.get("count").and_then(|c| c.as_u64()).map(|c| (k, c))
                        })
                        .collect();
                    top.sort_by(|a, b| b.1.cmp(&a.1));
                    top.truncate(5);
                    out.push_str("\n**Top 5 tools:**\n");
                    for (name, count) in top {
                        out.push_str(&format!("- {name}: {count} calls\n"));
                    }
                }
            }
        }
    }
    out
}

fn section_slow_commands() -> String {
    let mut out = String::from("## Slow Commands\n\n```\n");
    if let Some(dir) = lean_ctx_dir() {
        let log_path = dir.join("slow-commands.log");
        if let Ok(content) = std::fs::read_to_string(&log_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(10);
            for line in &lines[start..] {
                out.push_str(line);
                out.push('\n');
            }
        } else {
            out.push_str("# No slow commands logged\n");
        }
    }
    out.push_str("```");
    out
}

fn section_tee_logs(include_content: bool) -> String {
    let mut out = String::from("## Tee Logs (last 24h)\n\n");
    if let Some(dir) = lean_ctx_dir() {
        let tee_dir = dir.join("tee");
        if tee_dir.is_dir() {
            let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 3600);
            let mut entries: Vec<_> = std::fs::read_dir(&tee_dir)
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .is_some_and(|t| t > cutoff)
                })
                .collect();
            entries.sort_by_key(|e| {
                std::cmp::Reverse(
                    e.metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                )
            });

            if entries.is_empty() {
                out.push_str("No tee logs in the last 24h.\n");
            } else {
                for entry in entries.iter().take(10) {
                    let name = entry.file_name();
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    out.push_str(&format!("- `{}` ({size} bytes)\n", name.to_string_lossy()));
                }
                if include_content {
                    if let Some(latest) = entries.first() {
                        if let Ok(content) = std::fs::read_to_string(latest.path()) {
                            let truncated: String = content.chars().take(3000).collect();
                            out.push_str(&format!(
                                "\n**Latest tee content (`{}`):**\n```\n{truncated}\n```",
                                latest.file_name().to_string_lossy()
                            ));
                        }
                    }
                }
            }
        } else {
            out.push_str("No tee directory found.\n");
        }
    }
    out
}

fn section_project_context() -> String {
    let mut out = String::from("## Project Context\n\n");
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".into());
    out.push_str(&format!("- Working directory: {cwd}\n"));

    if let Ok(entries) = std::fs::read_dir(".") {
        let count = entries.filter_map(|e| e.ok()).count();
        out.push_str(&format!("- Files in root: {count}\n"));
    }
    out
}

// ── Anonymization ─────────────────────────────────────────────────────────

fn anonymize_report(text: &str) -> String {
    let home = dirs::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut result = text.to_string();
    if !home.is_empty() {
        result = result.replace(&home, "~");
    }

    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    if user.len() > 2 {
        result = result.replace(&user, "<user>");
    }

    result
}

fn mask_secrets(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if line.contains("token")
            || line.contains("key")
            || line.contains("secret")
            || line.contains("password")
            || line.contains("api_key")
        {
            if let Some(eq) = line.find('=') {
                out.push_str(&line[..=eq]);
                out.push_str(" \"[REDACTED]\"");
            } else {
                out.push_str(line);
            }
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

// ── GitHub Submission ─────────────────────────────────────────────────────

fn find_gh_binary() -> Option<std::path::PathBuf> {
    let candidates = [
        "/opt/homebrew/bin/gh",
        "/usr/local/bin/gh",
        "/usr/bin/gh",
        "/home/linuxbrew/.linuxbrew/bin/gh",
    ];
    for c in &candidates {
        let p = std::path::Path::new(c);
        if p.exists() {
            return Some(p.to_path_buf());
        }
    }
    if let Ok(output) = std::process::Command::new("which")
        .arg("gh")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(std::path::PathBuf::from(path));
            }
        }
    }
    None
}

fn try_gh_cli(title: &str, body: &str) -> bool {
    let gh = match find_gh_binary() {
        Some(p) => p,
        None => return false,
    };

    let tmp = std::env::temp_dir().join("lean-ctx-report.md");
    if std::fs::write(&tmp, body).is_err() {
        return false;
    }

    let result = std::process::Command::new(&gh)
        .args([
            "issue", "create",
            "--repo", REPO,
            "--title", title,
            "--body-file", &tmp.to_string_lossy(),
            "--label", "bug,auto-report",
        ])
        .output();

    if let Ok(ref output) = result {
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("not found") && stderr.contains("label") {
                let _ = std::fs::remove_file(&tmp);
                let fallback = std::process::Command::new(&gh)
                    .args([
                        "issue", "create",
                        "--repo", REPO,
                        "--title", title,
                        "--body-file", &tmp.to_string_lossy(),
                    ])
                    .output();
                let _ = std::fs::remove_file(&tmp);
                if let Ok(fb_out) = fallback {
                    if fb_out.status.success() {
                        let url = String::from_utf8_lossy(&fb_out.stdout);
                        println!("\n{GREEN}Issue created:{RST} {}", url.trim());
                        return true;
                    }
                }
                return false;
            }
        }
    }

    let _ = std::fs::remove_file(&tmp);

    match result {
        Ok(output) if output.status.success() => {
            let url = String::from_utf8_lossy(&output.stdout);
            println!("\n{GREEN}Issue created:{RST} {}", url.trim());
            true
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("not logged") || stderr.contains("auth login") {
                eprintln!("{YELLOW}gh CLI found but not authenticated. Run: gh auth login{RST}");
            } else {
                eprintln!("{YELLOW}gh issue create failed: {}{RST}", stderr.trim());
            }
            false
        }
        Err(e) => {
            eprintln!("{YELLOW}Failed to run gh: {e}{RST}");
            false
        }
    }
}

fn try_ureq_api(title: &str, body: &str) {
    println!("\n{YELLOW}gh CLI not available. Using GitHub API directly.{RST}");
    println!("Enter a GitHub Personal Access Token (needs 'repo' scope):");
    println!("{DIM}Create one at: https://github.com/settings/tokens/new{RST}");

    let mut token = String::new();
    let _ = std::io::stdin().read_line(&mut token);
    let token = token.trim();

    if token.is_empty() {
        eprintln!("No token provided. Saving report locally.");
        save_report_locally(body);
        return;
    }

    let url = format!("https://api.github.com/repos/{REPO}/issues");
    let payload = serde_json::json!({
        "title": title,
        "body": body,
        "labels": ["bug", "auto-report"]
    });

    let payload_bytes = serde_json::to_vec(&payload).unwrap_or_default();
    match ureq::post(&url)
        .header("Authorization", &format!("Bearer {token}"))
        .header("Accept", "application/vnd.github.v3+json")
        .header("Content-Type", "application/json")
        .header("User-Agent", &format!("lean-ctx/{VERSION}"))
        .send(payload_bytes.as_slice())
    {
        Ok(resp) => {
            let resp_body = resp.into_body().read_to_string().unwrap_or_default();
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&resp_body) {
                if let Some(html_url) = val.get("html_url").and_then(|u| u.as_str()) {
                    println!("\n{GREEN}Issue created:{RST} {html_url}");
                    return;
                }
            }
            println!("{GREEN}Issue created successfully.{RST}");
        }
        Err(e) => {
            eprintln!("GitHub API error: {e}");
            save_report_locally(body);
        }
    }
}

fn save_report_locally(body: &str) {
    if let Some(dir) = lean_ctx_dir() {
        let path = dir.join("last-report.md");
        let _ = std::fs::write(&path, body);
        println!("Report saved to {}", path.display());
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn lean_ctx_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".lean-ctx"))
}

fn which_lean_ctx() -> Option<PathBuf> {
    let cmd = if cfg!(windows) { "where" } else { "which" };
    std::process::Command::new(cmd)
        .arg("lean-ctx")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().to_string()))
}

fn check_shell_hooks() -> String {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return "unknown".into(),
    };

    let mut found = Vec::new();
    let shells = [
        (".zshrc", "zsh"),
        (".bashrc", "bash"),
        (".config/fish/config.fish", "fish"),
    ];
    for (file, name) in shells {
        let path = home.join(file);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if content.contains("lean-ctx") {
                found.push(name);
            }
        }
    }

    if found.is_empty() {
        "none detected".into()
    } else {
        found.join(", ")
    }
}

fn check_mcp_configs() -> String {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return "unknown".into(),
    };

    let mut found = Vec::new();
    let configs: &[(&str, &str)] = &[
        (".cursor/mcp.json", "Cursor"),
        (".claude.json", "Claude Code"),
        (".codeium/windsurf/mcp_config.json", "Windsurf"),
    ];

    for (path, name) in configs {
        let full = home.join(path);
        if let Ok(content) = std::fs::read_to_string(&full) {
            if content.contains("lean-ctx") {
                found.push(*name);
            }
        }
    }

    if found.is_empty() {
        "none".into()
    } else {
        found.join(", ")
    }
}

fn detect_ide() -> String {
    if std::env::var("CURSOR_SESSION").is_ok() || std::env::var("CURSOR_TRACE_DIR").is_ok() {
        return "Cursor".into();
    }
    if std::env::var("VSCODE_PID").is_ok() {
        return "VS Code".into();
    }
    "unknown".into()
}

fn extract_flag(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn prompt_input(label: &str) -> String {
    eprint!("{BOLD}{label}:{RST} ");
    let mut input = String::new();
    let _ = std::io::stdin().read_line(&mut input);
    input.trim().to_string()
}
