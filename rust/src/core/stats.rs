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
    if let Ok(json) = serde_json::to_string(store) {
        let tmp = dir.join(".stats.json.tmp");
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
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

const RST: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const CYAN: &str = "\x1b[36m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const WHITE: &str = "\x1b[97m";
const GRAY: &str = "\x1b[90m";
fn line(ch: char, n: usize) -> String {
    std::iter::repeat(ch).take(n).collect()
}

fn pct_color(pct: f64) -> &'static str {
    if pct >= 90.0 { "\x1b[32m" }
    else if pct >= 70.0 { "\x1b[36m" }
    else if pct >= 50.0 { "\x1b[33m" }
    else if pct >= 30.0 { "\x1b[35m" }
    else { "\x1b[37m" }
}

fn bar_block(ratio: f64, width: usize) -> String {
    let blocks = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let full = (ratio * width as f64).max(0.0);
    let whole = full as usize;
    let frac = ((full - whole as f64) * 8.0) as usize;
    let mut s = "█".repeat(whole);
    if whole < width && frac > 0 {
        s.push_str(blocks[frac]);
    }
    if s.is_empty() && ratio > 0.0 {
        s.push_str("▏");
    }
    s
}

fn sparkline(values: &[u64]) -> String {
    let ticks = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = *values.iter().max().unwrap_or(&1) as f64;
    if max == 0.0 {
        return " ".repeat(values.len());
    }
    values
        .iter()
        .map(|v| {
            let idx = ((*v as f64 / max) * 7.0).round() as usize;
            ticks[idx.min(7)]
        })
        .collect()
}

fn usd_estimate(tokens: u64) -> String {
    let cost = tokens as f64 * 2.50 / 1_000_000.0;
    if cost >= 1.0 { format!("${cost:.2}") }
    else if cost >= 0.01 { format!("${cost:.2}") }
    else { format!("${cost:.3}") }
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

pub fn format_gain() -> String {
    let store = load();
    let mut o = Vec::new();

    if store.total_commands == 0 {
        return format!("{DIM}No commands recorded yet.{RST} Use {CYAN}lean-ctx -c \"command\"{RST} to start tracking.");
    }

    let saved = store.total_input_tokens.saturating_sub(store.total_output_tokens);
    let pct = if store.total_input_tokens > 0 {
        saved as f64 / store.total_input_tokens as f64 * 100.0
    } else {
        0.0
    };
    let usd = usd_estimate(saved);
    let days_active = store.daily.len();

    o.push(String::new());
    let ln56 = line('─', 56);
    o.push(format!("  {BOLD}{WHITE}◆ lean-ctx{RST}  {DIM}Token Savings Dashboard{RST}"));
    o.push(format!("  {DIM}{ln56}{RST}"));
    o.push(String::new());

    o.push(format!(
        "  {BOLD}{GREEN} {:<12}{RST}  {BOLD}{CYAN} {:<12}{RST}  {BOLD}{YELLOW} {:<10}{RST}  {BOLD}{MAGENTA} {:<10}{RST}",
        format_big(saved), format!("{pct:.1}%"), format_num(store.total_commands), usd
    ));
    o.push(format!(
        "  {DIM} tokens saved   compression    commands       USD saved{RST}"
    ));
    o.push(String::new());

    if let (Some(first), Some(_last)) = (&store.first_use, &store.last_use) {
        let first_short = first.get(..10).unwrap_or(first);
        let daily_savings: Vec<u64> = store.daily.iter()
            .map(|d| d.input_tokens.saturating_sub(d.output_tokens))
            .collect();
        let spark = sparkline(&daily_savings);
        o.push(format!(
            "  {DIM}Since {first_short} ({days_active} day{plural}){RST}  {GREEN}{spark}{RST}",
            plural = if days_active != 1 { "s" } else { "" }
        ));
        o.push(String::new());
    }

    if !store.commands.is_empty() {
        o.push(format!("  {BOLD}{WHITE}Top Commands{RST}"));
        o.push(format!("  {DIM}{ln56}{RST}"));

        let mut sorted: Vec<_> = store.commands.iter().collect();
        sorted.sort_by(|a, b| {
            let sa = a.1.input_tokens.saturating_sub(a.1.output_tokens);
            let sb = b.1.input_tokens.saturating_sub(b.1.output_tokens);
            sb.cmp(&sa)
        });

        let max_cmd_saved = sorted.first()
            .map(|(_, s)| s.input_tokens.saturating_sub(s.output_tokens))
            .unwrap_or(1)
            .max(1);

        for (cmd, stats) in sorted.iter().take(12) {
            let cmd_saved = stats.input_tokens.saturating_sub(stats.output_tokens);
            let cmd_pct = if stats.input_tokens > 0 {
                cmd_saved as f64 / stats.input_tokens as f64 * 100.0
            } else {
                0.0
            };
            let ratio = cmd_saved as f64 / max_cmd_saved as f64;
            let bar = bar_block(ratio, 20);
            let pc = pct_color(cmd_pct);
            o.push(format!(
                "  {GRAY}{:<16}{RST} {:>5}x  {pc}{bar:<20}{RST} {BOLD}{pc}{:>6}{RST}  {DIM}{}{RST}",
                truncate_cmd(cmd, 16),
                stats.count,
                format_big(cmd_saved),
                format!("{cmd_pct:.0}%"),
            ));
        }

        if sorted.len() > 12 {
            o.push(format!("  {DIM}  ... +{} more commands{RST}", sorted.len() - 12));
        }
    }

    if store.daily.len() >= 2 {
        o.push(String::new());
        o.push(format!("  {BOLD}{WHITE}Recent Days{RST}"));
        o.push(format!("  {DIM}{ln56}{RST}"));

        let recent: Vec<_> = store.daily.iter().rev().take(7).collect();
        for day in recent.iter().rev() {
            let day_saved = day.input_tokens.saturating_sub(day.output_tokens);
            let day_pct = if day.input_tokens > 0 {
                day_saved as f64 / day.input_tokens as f64 * 100.0
            } else {
                0.0
            };
            let pc = pct_color(day_pct);
            let date_short = day.date.get(5..).unwrap_or(&day.date);
            o.push(format!(
                "  {GRAY}{date_short}{RST}  {:>5} cmds  {pc}{BOLD}{:>8}{RST} saved  {pc}{day_pct:>5.1}%{RST}",
                day.commands,
                format_big(day_saved),
            ));
        }
    }

    o.push(String::new());
    o.push(format!("  {DIM}{ln56}{RST}"));
    o.push(format!("  {DIM}lean-ctx v2.1.0  |  leanctx.com  |  lean-ctx dashboard{RST}"));
    o.push(String::new());

    o.join("\n")
}

pub fn format_gain_graph() -> String {
    let store = load();
    if store.daily.is_empty() {
        return format!("{DIM}No daily data yet.{RST} Use lean-ctx for a few days to see the graph.");
    }

    let days: Vec<_> = store.daily.iter().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();

    let savings: Vec<u64> = days.iter()
        .map(|d| d.input_tokens.saturating_sub(d.output_tokens))
        .collect();

    let max_saved = *savings.iter().max().unwrap_or(&1);
    let max_saved = max_saved.max(1);

    let bar_width = 36;
    let mut o = Vec::new();

    o.push(String::new());
    let ln58 = line('─', 58);
    o.push(format!("  {BOLD}{WHITE}◆ lean-ctx{RST}  {DIM}Token Savings Graph (last 30 days){RST}"));
    o.push(format!("  {DIM}{ln58}{RST}"));
    o.push(format!("  {DIM}{:>58}{RST}", format!("peak: {}", format_big(max_saved))));
    o.push(String::new());

    for (i, day) in days.iter().enumerate() {
        let saved = savings[i];
        let ratio = saved as f64 / max_saved as f64;
        let bar = bar_block(ratio, bar_width);

        let pct = if day.input_tokens > 0 {
            saved as f64 / day.input_tokens as f64 * 100.0
        } else {
            0.0
        };
        let pc = pct_color(pct);
        let date_short = day.date.get(5..).unwrap_or(&day.date);

        o.push(format!(
            "  {GRAY}{date_short}{RST} {DIM}│{RST} {pc}{bar:<width$}{RST} {BOLD}{:>6}{RST} {DIM}{pct:.0}%{RST}",
            format_big(saved),
            width = bar_width,
        ));
    }

    let total_saved: u64 = savings.iter().sum();
    let total_cmds: u64 = days.iter().map(|d| d.commands).sum();
    let spark = sparkline(&savings);

    o.push(String::new());
    o.push(format!("  {DIM}{ln58}{RST}"));
    o.push(format!(
        "  {GREEN}{spark}{RST}  {BOLD}{WHITE}{}{RST} saved across {BOLD}{}{RST} commands",
        format_big(total_saved),
        format_num(total_cmds),
    ));
    o.push(String::new());

    o.join("\n")
}

pub fn format_gain_daily() -> String {
    let store = load();
    if store.daily.is_empty() {
        return format!("{DIM}No daily data yet.{RST}");
    }

    let mut o = Vec::new();
    let w = 64;

    o.push(String::new());
    let lnw = line('─', w);
    o.push(format!("  {BOLD}{WHITE}◆ lean-ctx{RST}  {DIM}Daily Breakdown{RST}"));
    o.push(format!("  {DIM}┌{lnw}┐{RST}"));
    o.push(format!(
        "  {DIM}│{RST} {BOLD}{WHITE}{:<12} {:>6}  {:>10}  {:>10}  {:>7}  {:>6}{RST} {DIM}│{RST}",
        "Date", "Cmds", "Input", "Saved", "Rate", "USD"
    ));
    o.push(format!("  {DIM}├{lnw}┤{RST}"));

    let days: Vec<_> = store.daily.iter().rev().take(30).collect::<Vec<_>>().into_iter().rev().cloned().collect();

    for day in &days {
        let saved = day.input_tokens.saturating_sub(day.output_tokens);
        let pct = if day.input_tokens > 0 {
            saved as f64 / day.input_tokens as f64 * 100.0
        } else {
            0.0
        };
        let pc = pct_color(pct);
        let usd = usd_estimate(saved);
        o.push(format!(
            "  {DIM}│{RST} {GRAY}{:<12}{RST} {:>6}  {:>10}  {pc}{BOLD}{:>10}{RST}  {pc}{:>6.1}%{RST}  {DIM}{:>6}{RST} {DIM}│{RST}",
            &day.date,
            day.commands,
            format_big(day.input_tokens),
            format_big(saved),
            pct,
            usd,
        ));
    }

    let total_input: u64 = store.daily.iter().map(|d| d.input_tokens).sum();
    let total_saved: u64 = store.daily.iter().map(|d| d.input_tokens.saturating_sub(d.output_tokens)).sum();
    let total_pct = if total_input > 0 {
        total_saved as f64 / total_input as f64 * 100.0
    } else {
        0.0
    };
    let total_usd = usd_estimate(total_saved);

    o.push(format!("  {DIM}├{lnw}┤{RST}"));
    o.push(format!(
        "  {DIM}│{RST} {BOLD}{WHITE}{:<12}{RST} {:>6}  {:>10}  {GREEN}{BOLD}{:>10}{RST}  {GREEN}{BOLD}{:>6.1}%{RST}  {BOLD}{:>6}{RST} {DIM}│{RST}",
        "TOTAL",
        format_num(store.total_commands),
        format_big(total_input),
        format_big(total_saved),
        total_pct,
        total_usd,
    ));
    o.push(format!("  {DIM}└{lnw}┘{RST}"));

    let daily_savings: Vec<u64> = days.iter().map(|d| d.input_tokens.saturating_sub(d.output_tokens)).collect();
    let spark = sparkline(&daily_savings);
    o.push(format!("  {DIM}Trend:{RST} {GREEN}{spark}{RST}"));
    o.push(String::new());

    o.join("\n")
}

pub fn format_gain_json() -> String {
    let store = load();
    serde_json::to_string_pretty(&store).unwrap_or_else(|_| "{}".to_string())
}
