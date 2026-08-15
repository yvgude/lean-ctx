//! Shareable savings badges for pull requests and social posts.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{
    core::session::{SessionState, SessionStats},
    proxy::value_gate_proxy,
};

const BADGE_SPEED_MULTIPLIER: f64 = 4.2;
const COST_PER_MILLION_TOKENS_USD: f64 = 3.0;
const GITHUB_ACTION_PATH: &str = ".github/workflows/lean-ctx-badge.yml";

/// Operations supported by `lean-ctx badge`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadgeCommand {
    Generate,
    Stats { json: bool },
    GithubAction,
    Install,
}

/// A compact, shareable representation of the session's savings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    pub text: String,
    pub markdown: String,
    pub svg_url: String,
    pub share_url: String,
}

#[derive(Debug, Serialize)]
struct BadgeStats {
    tokens_saved: u64,
    compression_ratio: f64,
    sessions_count: usize,
    total_cost_saved_usd: f64,
    badge_text: String,
    badge_url: String,
    share_url: String,
}

/// Generate a badge for one session. The share link uses one session because
/// this function deliberately has no filesystem dependency.
pub fn generate_badge(stats: &SessionStats) -> Badge {
    generate_badge_for_sessions(stats, 1)
}

fn generate_badge_for_sessions(stats: &SessionStats, sessions_count: usize) -> Badge {
    let saved_percent = savings_percent(stats);
    let saved_percent_display = saved_percent.round() as u64;
    let svg_url =
        format!("https://img.shields.io/badge/lean--ctx-{saved_percent_display}%25_saved-blue.svg");
    let markdown = format!(
        "![lean-ctx](https://img.shields.io/badge/lean--ctx-{saved_percent_display}%25_saved-blue)"
    );
    let text = format!(
        "Written with lean-ctx: {saved_percent_display}% fewer tokens, {BADGE_SPEED_MULTIPLIER:.1}x faster"
    );
    let share_url = format!(
        "https://lean-ctx.dev/share?saved={saved_percent_display}&tokens={}&sessions={}",
        stats.total_tokens_saved,
        sessions_count.max(1)
    );

    Badge {
        text,
        markdown,
        svg_url,
        share_url,
    }
}

/// Render the workflow installed by `lean-ctx badge install`.
pub fn generate_github_action() -> String {
    r#"name: lean-ctx badge

on:
  pull_request:
    types: [opened, synchronize, reopened]

permissions:
  contents: read
  pull-requests: write

jobs:
  badge:
    name: Post lean-ctx savings badge
    runs-on: ubuntu-latest
    steps:
      - name: Check out PR branch
        uses: actions/checkout@v4
        with:
          ref: ${{ github.event.pull_request.head.sha }}

      - name: Install lean-ctx
        run: curl -fsSL https://lean-ctx.dev/install.sh | sh

      - name: Read session savings
        id: stats
        run: echo "json=$(lean-ctx badge stats --json | jq -c .)" >> "$GITHUB_OUTPUT"

      - name: Post savings badge
        uses: actions/github-script@v7
        env:
          STATS: ${{ steps.stats.outputs.json }}
        with:
          script: |
            const stats = JSON.parse(process.env.STATS);
            const body = `### ${stats.badge_text}\n\n![lean-ctx](${stats.badge_url})\n\n[Share your savings](${stats.share_url})`;
            await github.rest.issues.createComment({
              owner: context.repo.owner,
              repo: context.repo.repo,
              issue_number: context.issue.number,
              body,
            });
"#
    .to_string()
}

/// Return recent metrics as machine-readable badge statistics.
pub fn cmd_badge_stats() -> Result<String, String> {
    let stats = current_session_stats();
    let sessions_count = SessionState::list_sessions().len().max(1);
    let badge = generate_badge_for_sessions(&stats, sessions_count);
    let proxy_metrics = value_gate_proxy::session_metrics();
    let tokens_saved = if proxy_metrics.total_original_tokens > 0 {
        proxy_metrics.total_tokens_pruned
    } else {
        stats.total_tokens_saved
    };
    let cost_saved_usd = if proxy_metrics.cost_micros_estimate > 0 {
        proxy_metrics.cost_micros_estimate as f64 / 1_000_000.0
    } else {
        tokens_saved as f64 * COST_PER_MILLION_TOKENS_USD / 1_000_000.0
    };
    let result = BadgeStats {
        tokens_saved,
        compression_ratio: savings_percent(&stats),
        sessions_count,
        total_cost_saved_usd: (cost_saved_usd * 100_000.0).round() / 100_000.0,
        badge_text: badge.text,
        badge_url: badge.svg_url,
        share_url: badge.share_url,
    };

    serde_json::to_string(&result).map_err(|error| error.to_string())
}

/// Dispatch `lean-ctx badge` CLI arguments.
pub(crate) fn cmd_badge(args: &[String]) {
    match parse_command(args) {
        BadgeCommand::Generate => {
            let stats = current_session_stats();
            let sessions_count = SessionState::list_sessions().len().max(1);
            let badge = generate_badge_for_sessions(&stats, sessions_count);
            println!("{}\n{}\n{}", badge.text, badge.markdown, badge.share_url);
        }
        BadgeCommand::Stats { json: true } => match cmd_badge_stats() {
            Ok(stats) => println!("{stats}"),
            Err(error) => eprintln!("badge stats: {error}"),
        },
        BadgeCommand::Stats { json: false } => match cmd_badge_stats() {
            Ok(stats) => println!("{stats}"),
            Err(error) => eprintln!("badge stats: {error}"),
        },
        BadgeCommand::GithubAction => print!("{}", generate_github_action()),
        BadgeCommand::Install => match install_github_action(Path::new(".")) {
            Ok(path) => println!("Installed {}", path.display()),
            Err(error) => eprintln!("badge install: {error}"),
        },
    }
}

fn parse_command(args: &[String]) -> BadgeCommand {
    match args.first().map(String::as_str) {
        Some("stats") => BadgeCommand::Stats {
            json: args.iter().any(|arg| arg == "--json"),
        },
        Some("github-action") | Some("github_action") => BadgeCommand::GithubAction,
        Some("install") => BadgeCommand::Install,
        Some("generate") | None => BadgeCommand::Generate,
        Some(other) => {
            eprintln!(
                "unknown badge subcommand: {other}; use generate, stats, github-action, or install"
            );
            BadgeCommand::Generate
        }
    }
}

fn current_session_stats() -> SessionStats {
    let proxy_metrics = value_gate_proxy::session_metrics();
    if proxy_metrics.total_original_tokens > 0 {
        return SessionStats {
            total_tool_calls: u32::try_from(proxy_metrics.request_count).unwrap_or(u32::MAX),
            total_tokens_saved: proxy_metrics.total_tokens_pruned,
            total_tokens_input: proxy_metrics.total_original_tokens,
            ..SessionStats::default()
        };
    }

    SessionState::load_latest()
        .map(|session| session.stats)
        .unwrap_or_default()
}

fn install_github_action(project_root: &Path) -> Result<PathBuf, String> {
    let path = project_root.join(GITHUB_ACTION_PATH);
    let parent = path
        .parent()
        .ok_or_else(|| "badge workflow has no parent directory".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    std::fs::write(&path, generate_github_action()).map_err(|error| error.to_string())?;
    Ok(path)
}

fn savings_percent(stats: &SessionStats) -> f64 {
    if stats.total_tokens_input == 0 {
        return 0.0;
    }

    (stats.total_tokens_saved as f64 * 100.0 / stats.total_tokens_input as f64).clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known_stats() -> SessionStats {
        SessionStats {
            total_tokens_saved: 7_300,
            total_tokens_input: 10_000,
            ..SessionStats::default()
        }
    }

    #[test]
    fn generates_badge_from_known_stats() {
        let badge = generate_badge(&known_stats());

        assert_eq!(
            badge.text,
            "Written with lean-ctx: 73% fewer tokens, 4.2x faster"
        );
        assert_eq!(
            badge.markdown,
            "![lean-ctx](https://img.shields.io/badge/lean--ctx-73%25_saved-blue)"
        );
        assert_eq!(
            badge.share_url,
            "https://lean-ctx.dev/share?saved=73&tokens=7300&sessions=1"
        );
    }

    #[test]
    fn github_action_yaml_contains_required_workflow_parts() {
        let yaml = generate_github_action();

        assert!(yaml.starts_with("name: lean-ctx badge\n"));
        assert!(yaml.contains("on:\n  pull_request:"));
        assert!(yaml.contains("jobs:\n  badge:"));
        assert!(yaml.contains("pull_request:"));
        assert!(yaml.contains("lean-ctx badge stats --json"));
        assert!(yaml.contains("issues.createComment"));
    }

    #[test]
    fn stats_output_contains_required_fields() {
        let json = cmd_badge_stats().expect("serializes current badge stats");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        for field in [
            "tokens_saved",
            "compression_ratio",
            "sessions_count",
            "total_cost_saved_usd",
            "badge_text",
        ] {
            assert!(value.get(field).is_some(), "missing {field}");
        }
    }
}
