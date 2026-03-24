use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct StatsStore {
    pub total_commands: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub first_use: Option<String>,
    pub last_use: Option<String>,
    pub commands: HashMap<String, CommandStats>,
    pub daily: Vec<DayStats>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct CommandStats {
    pub count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct DayStats {
    pub date: String,
    pub commands: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

fn stats_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".lean-ctx"))
}

fn stats_path() -> Option<PathBuf> {
    stats_dir().map(|d| d.join("stats.json"))
}

pub fn load() -> StatsStore {
    let path = match stats_path() {
        Some(p) => p,
        None => return StatsStore::default(),
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => StatsStore::default(),
    }
}

pub fn save(store: &StatsStore) {
    let dir = match stats_dir() {
        Some(d) => d,
        None => return,
    };

    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
    }

    let path = dir.join("stats.json");
    if let Ok(json) = serde_json::to_string_pretty(store) {
        let _ = std::fs::write(path, json);
    }
}

pub fn record(command: &str, input_tokens: usize, output_tokens: usize) {
    let mut store = load();
    let now = chrono::Local::now();
    let today = now.format("%Y-%m-%d").to_string();
    let timestamp = now.to_rfc3339();

    store.total_commands += 1;
    store.total_input_tokens += input_tokens as u64;
    store.total_output_tokens += output_tokens as u64;

    if store.first_use.is_none() {
        store.first_use = Some(timestamp.clone());
    }
    store.last_use = Some(timestamp);

    let cmd_key = normalize_command(command);
    let entry = store.commands.entry(cmd_key).or_default();
    entry.count += 1;
    entry.input_tokens += input_tokens as u64;
    entry.output_tokens += output_tokens as u64;

    if let Some(day) = store.daily.last_mut() {
        if day.date == today {
            day.commands += 1;
            day.input_tokens += input_tokens as u64;
            day.output_tokens += output_tokens as u64;
        } else {
            store.daily.push(DayStats {
                date: today,
                commands: 1,
                input_tokens: input_tokens as u64,
                output_tokens: output_tokens as u64,
            });
        }
    } else {
        store.daily.push(DayStats {
            date: today,
            commands: 1,
            input_tokens: input_tokens as u64,
            output_tokens: output_tokens as u64,
        });
    }

    if store.daily.len() > 90 {
        store.daily.drain(..store.daily.len() - 90);
    }

    save(&store);
}

fn normalize_command(command: &str) -> String {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return command.to_string();
    }

    let base = std::path::Path::new(parts[0])
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(parts[0]);

    match base {
        "git" => {
            if parts.len() > 1 {
                format!("git {}", parts[1])
            } else {
                "git".to_string()
            }
        }
        "cargo" => {
            if parts.len() > 1 {
                format!("cargo {}", parts[1])
            } else {
                "cargo".to_string()
            }
        }
        "npm" | "yarn" | "pnpm" => {
            if parts.len() > 1 {
                format!("{} {}", base, parts[1])
            } else {
                base.to_string()
            }
        }
        "docker" => {
            if parts.len() > 1 {
                format!("docker {}", parts[1])
            } else {
                "docker".to_string()
            }
        }
        _ => base.to_string(),
    }
}

pub struct GainSummary {
    pub total_saved: u64,
    pub total_calls: u64,
}

pub fn load_stats() -> GainSummary {
    let store = load();
    let saved = store.total_input_tokens.saturating_sub(store.total_output_tokens);
    GainSummary {
        total_saved: saved,
        total_calls: store.total_commands,
    }
}

pub fn format_gain() -> String {
    let store = load();
    let mut out = Vec::new();

    if store.total_commands == 0 {
        return "No commands recorded yet. Use lean-ctx -c \"command\" to start tracking.".to_string();
    }

    let saved = store.total_input_tokens.saturating_sub(store.total_output_tokens);
    let pct = if store.total_input_tokens > 0 {
        saved as f64 / store.total_input_tokens as f64 * 100.0
    } else {
        0.0
    };

    out.push(format!("lean-ctx Token Savings"));
    out.push("═".repeat(50));
    out.push(String::new());
    out.push(format!("Total commands:  {}", format_num(store.total_commands)));
    out.push(format!("Input tokens:    {}", format_big(store.total_input_tokens)));
    out.push(format!("Output tokens:   {}", format_big(store.total_output_tokens)));
    out.push(format!("Tokens saved:    {} ({:.1}%)", format_big(saved), pct));

    if let (Some(first), Some(last)) = (&store.first_use, &store.last_use) {
        let first_short = first.get(..10).unwrap_or(first);
        let last_short = last.get(..10).unwrap_or(last);
        out.push(String::new());
        out.push(format!("Tracking since:  {first_short}"));
        out.push(format!("Last used:       {last_short}"));
    }

    if !store.commands.is_empty() {
        out.push(String::new());
        out.push("By Command:".to_string());
        out.push("─".repeat(50));
        out.push(format!(
            "{:<20} {:>6}  {:>9}  {:>5}",
            "Command", "Count", "Saved", "Avg%"
        ));

        let mut sorted: Vec<_> = store.commands.iter().collect();
        sorted.sort_by(|a, b| {
            let saved_a = a.1.input_tokens.saturating_sub(a.1.output_tokens);
            let saved_b = b.1.input_tokens.saturating_sub(b.1.output_tokens);
            saved_b.cmp(&saved_a)
        });

        for (cmd, stats) in sorted.iter().take(15) {
            let cmd_saved = stats.input_tokens.saturating_sub(stats.output_tokens);
            let cmd_pct = if stats.input_tokens > 0 {
                cmd_saved as f64 / stats.input_tokens as f64 * 100.0
            } else {
                0.0
            };
            out.push(format!(
                "{:<20} {:>6}  {:>9}  {:>4.1}%",
                truncate_cmd(cmd, 20),
                stats.count,
                format_big(cmd_saved),
                cmd_pct
            ));
        }
    }

    if store.daily.len() >= 2 {
        out.push(String::new());
        out.push("Recent Days:".to_string());
        out.push("─".repeat(50));
        out.push(format!(
            "{:<12} {:>6}  {:>9}  {:>5}",
            "Date", "Cmds", "Saved", "Avg%"
        ));

        let recent: Vec<_> = store.daily.iter().rev().take(7).collect();
        for day in recent.iter().rev() {
            let day_saved = day.input_tokens.saturating_sub(day.output_tokens);
            let day_pct = if day.input_tokens > 0 {
                day_saved as f64 / day.input_tokens as f64 * 100.0
            } else {
                0.0
            };
            out.push(format!(
                "{:<12} {:>6}  {:>9}  {:>4.1}%",
                &day.date,
                day.commands,
                format_big(day_saved),
                day_pct
            ));
        }
    }

    out.push(String::new());
    out.push("═".repeat(50));

    out.join("\n")
}

fn format_big(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

fn format_num(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{},{:03}", n / 1_000, n % 1_000)
    } else {
        format!("{n}")
    }
}

fn truncate_cmd(cmd: &str, max: usize) -> String {
    if cmd.len() <= max {
        cmd.to_string()
    } else {
        format!("{}…", &cmd[..max - 1])
    }
}

pub fn format_gain_graph() -> String {
    let store = load();
    if store.daily.is_empty() {
        return "No daily data yet. Use lean-ctx for a few days to see the graph.".to_string();
    }

    let days: Vec<_> = store.daily.iter().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();

    let max_saved = days
        .iter()
        .map(|d| d.input_tokens.saturating_sub(d.output_tokens))
        .max()
        .unwrap_or(1)
        .max(1);

    let bar_width = 40;
    let mut out = Vec::new();
    out.push("lean-ctx Token Savings (last 30 days)".to_string());
    out.push("=".repeat(60));
    out.push(String::new());

    for day in &days {
        let saved = day.input_tokens.saturating_sub(day.output_tokens);
        let bar_len = (saved as f64 / max_saved as f64 * bar_width as f64) as usize;
        let bar: String = "#".repeat(bar_len);
        let date_short = day.date.get(5..).unwrap_or(&day.date);
        out.push(format!("{date_short} |{bar:<width$}| {}", format_big(saved), width = bar_width));
    }

    let total_saved: u64 = days.iter().map(|d| d.input_tokens.saturating_sub(d.output_tokens)).sum();
    let total_cmds: u64 = days.iter().map(|d| d.commands).sum();
    out.push(String::new());
    out.push(format!("Period: {} saved across {} commands", format_big(total_saved), format_num(total_cmds)));
    out.push("=".repeat(60));

    out.join("\n")
}

pub fn format_gain_daily() -> String {
    let store = load();
    if store.daily.is_empty() {
        return "No daily data yet.".to_string();
    }

    let mut out = Vec::new();
    out.push("lean-ctx Daily Breakdown".to_string());
    out.push("=".repeat(60));
    out.push(format!(
        "{:<12} {:>6}  {:>9}  {:>9}  {:>5}",
        "Date", "Cmds", "Input", "Saved", "Pct"
    ));
    out.push("-".repeat(60));

    for day in store.daily.iter().rev().take(30).collect::<Vec<_>>().into_iter().rev() {
        let saved = day.input_tokens.saturating_sub(day.output_tokens);
        let pct = if day.input_tokens > 0 {
            saved as f64 / day.input_tokens as f64 * 100.0
        } else {
            0.0
        };
        out.push(format!(
            "{:<12} {:>6}  {:>9}  {:>9}  {:>4.1}%",
            &day.date,
            day.commands,
            format_big(day.input_tokens),
            format_big(saved),
            pct
        ));
    }

    let total_input: u64 = store.daily.iter().map(|d| d.input_tokens).sum();
    let total_saved: u64 = store.daily.iter().map(|d| d.input_tokens.saturating_sub(d.output_tokens)).sum();
    let total_pct = if total_input > 0 {
        total_saved as f64 / total_input as f64 * 100.0
    } else {
        0.0
    };
    out.push("-".repeat(60));
    out.push(format!(
        "{:<12} {:>6}  {:>9}  {:>9}  {:>4.1}%",
        "TOTAL",
        format_num(store.total_commands),
        format_big(total_input),
        format_big(total_saved),
        total_pct
    ));
    out.push("=".repeat(60));

    out.join("\n")
}

pub fn format_gain_json() -> String {
    let store = load();
    serde_json::to_string_pretty(&store).unwrap_or_else(|_| "{}".to_string())
}
