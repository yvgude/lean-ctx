use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct StatsStore {
    pub total_commands: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub first_use: Option<String>,
    pub last_use: Option<String>,
    pub commands: HashMap<String, CommandStats>,
    pub daily: Vec<DayStats>,
    #[serde(default)]
    pub cep: CepStats,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct CepStats {
    pub sessions: u64,
    pub total_cache_hits: u64,
    pub total_cache_reads: u64,
    pub total_tokens_original: u64,
    pub total_tokens_compressed: u64,
    pub modes: HashMap<String, u64>,
    pub scores: Vec<CepSessionSnapshot>,
    #[serde(default)]
    pub last_session_pid: Option<u32>,
    #[serde(default)]
    pub last_session_original: Option<u64>,
    #[serde(default)]
    pub last_session_compressed: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CepSessionSnapshot {
    pub timestamp: String,
    pub score: u32,
    pub cache_hit_rate: u32,
    pub mode_diversity: u32,
    pub compression_rate: u32,
    pub tool_calls: u64,
    pub tokens_saved: u64,
    pub complexity: String,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
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

fn load_from_disk() -> StatsStore {
    let path = match stats_path() {
        Some(p) => p,
        None => return StatsStore::default(),
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => StatsStore::default(),
    }
}

fn save_to_disk(store: &StatsStore) {
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

pub fn load() -> StatsStore {
    let guard = STATS_BUFFER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((ref store, _)) = *guard {
        return store.clone();
    }
    drop(guard);
    load_from_disk()
}

pub fn save(store: &StatsStore) {
    save_to_disk(store);
}

const FLUSH_INTERVAL_SECS: u64 = 30;

static STATS_BUFFER: Mutex<Option<(StatsStore, Instant)>> = Mutex::new(None);

fn with_buffer<F, R>(f: F) -> R
where
    F: FnOnce(&mut StatsStore, &mut Instant) -> R,
{
    let mut guard = STATS_BUFFER.lock().unwrap_or_else(|e| e.into_inner());
    let (store, last_flush) = guard.get_or_insert_with(|| (load_from_disk(), Instant::now()));
    f(store, last_flush)
}

fn maybe_flush(store: &StatsStore, last_flush: &mut Instant) {
    if last_flush.elapsed().as_secs() >= FLUSH_INTERVAL_SECS {
        save_to_disk(store);
        *last_flush = Instant::now();
    }
}

pub fn flush() {
    let mut guard = STATS_BUFFER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((ref store, ref mut last_flush)) = *guard {
        save_to_disk(store);
        *last_flush = Instant::now();
    }
}

pub fn record(command: &str, input_tokens: usize, output_tokens: usize) {
    with_buffer(|store, last_flush| {
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

        maybe_flush(store, last_flush);
    });
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

pub fn reset_cep() {
    with_buffer(|store, last_flush| {
        store.cep = CepStats::default();
        save_to_disk(store);
        *last_flush = Instant::now();
    });
}

pub fn reset_all() {
    with_buffer(|store, last_flush| {
        *store = StatsStore::default();
        save_to_disk(store);
        *last_flush = Instant::now();
    });
}

pub struct GainSummary {
    pub total_saved: u64,
    pub total_calls: u64,
}

pub fn load_stats() -> GainSummary {
    let store = load();
    let input_saved = store
        .total_input_tokens
        .saturating_sub(store.total_output_tokens);
    GainSummary {
        total_saved: input_saved,
        total_calls: store.total_commands,
    }
}

fn cmd_total_saved(s: &CommandStats, _cm: &CostModel) -> u64 {
    s.input_tokens.saturating_sub(s.output_tokens)
}

fn day_total_saved(d: &DayStats, _cm: &CostModel) -> u64 {
    d.input_tokens.saturating_sub(d.output_tokens)
}

#[allow(clippy::too_many_arguments)]
pub fn record_cep_session(
    score: u32,
    cache_hits: u64,
    cache_reads: u64,
    tokens_original: u64,
    tokens_compressed: u64,
    modes: &HashMap<String, u64>,
    tool_calls: u64,
    complexity: &str,
) {
    with_buffer(|store, last_flush| {
        let cep = &mut store.cep;

        let pid = std::process::id();
        let prev_original = cep.last_session_original.unwrap_or(0);
        let prev_compressed = cep.last_session_compressed.unwrap_or(0);
        let is_same_session = cep.last_session_pid == Some(pid);

        if is_same_session {
            let delta_original = tokens_original.saturating_sub(prev_original);
            let delta_compressed = tokens_compressed.saturating_sub(prev_compressed);
            cep.total_tokens_original += delta_original;
            cep.total_tokens_compressed += delta_compressed;
        } else {
            cep.sessions += 1;
            cep.total_cache_hits += cache_hits;
            cep.total_cache_reads += cache_reads;
            cep.total_tokens_original += tokens_original;
            cep.total_tokens_compressed += tokens_compressed;

            for (mode, count) in modes {
                *cep.modes.entry(mode.clone()).or_insert(0) += count;
            }
        }

        cep.last_session_pid = Some(pid);
        cep.last_session_original = Some(tokens_original);
        cep.last_session_compressed = Some(tokens_compressed);

        let cache_hit_rate = if cache_reads > 0 {
            (cache_hits as f64 / cache_reads as f64 * 100.0).round() as u32
        } else {
            0
        };

        let compression_rate = if tokens_original > 0 {
            ((tokens_original - tokens_compressed) as f64 / tokens_original as f64 * 100.0).round()
                as u32
        } else {
            0
        };

        let total_modes = 6u32;
        let mode_diversity =
            ((modes.len() as f64 / total_modes as f64).min(1.0) * 100.0).round() as u32;

        let tokens_saved = tokens_original.saturating_sub(tokens_compressed);

        cep.scores.push(CepSessionSnapshot {
            timestamp: chrono::Local::now().to_rfc3339(),
            score,
            cache_hit_rate,
            mode_diversity,
            compression_rate,
            tool_calls,
            tokens_saved,
            complexity: complexity.to_string(),
        });

        if cep.scores.len() > 100 {
            cep.scores.drain(..cep.scores.len() - 100);
        }

        maybe_flush(store, last_flush);
    });
}

use super::theme::{self, Theme};

fn active_theme() -> Theme {
    let cfg = super::config::Config::load();
    theme::load_theme(&cfg.theme)
}

/// Average LLM pricing per 1M tokens (blended across Claude, GPT, Gemini).
pub const DEFAULT_INPUT_PRICE_PER_M: f64 = 2.50;
pub const DEFAULT_OUTPUT_PRICE_PER_M: f64 = 10.0;

pub struct CostModel {
    pub input_price_per_m: f64,
    pub output_price_per_m: f64,
    pub avg_verbose_output_per_call: u64,
    pub avg_concise_output_per_call: u64,
}

impl Default for CostModel {
    fn default() -> Self {
        Self {
            input_price_per_m: DEFAULT_INPUT_PRICE_PER_M,
            output_price_per_m: DEFAULT_OUTPUT_PRICE_PER_M,
            avg_verbose_output_per_call: 180,
            avg_concise_output_per_call: 120,
        }
    }
}

pub struct CostBreakdown {
    pub input_cost_without: f64,
    pub input_cost_with: f64,
    pub output_cost_without: f64,
    pub output_cost_with: f64,
    pub total_cost_without: f64,
    pub total_cost_with: f64,
    pub total_saved: f64,
    pub estimated_output_tokens_without: u64,
    pub estimated_output_tokens_with: u64,
    pub output_tokens_saved: u64,
}

impl CostModel {
    pub fn calculate(&self, store: &StatsStore) -> CostBreakdown {
        let input_cost_without =
            store.total_input_tokens as f64 / 1_000_000.0 * self.input_price_per_m;
        let input_cost_with =
            store.total_output_tokens as f64 / 1_000_000.0 * self.input_price_per_m;

        let input_saved = store
            .total_input_tokens
            .saturating_sub(store.total_output_tokens);
        let compression_rate = if store.total_input_tokens > 0 {
            input_saved as f64 / store.total_input_tokens as f64
        } else {
            0.0
        };
        let est_output_without = store.total_commands * self.avg_verbose_output_per_call;
        let est_output_with = if compression_rate > 0.01 {
            store.total_commands * self.avg_concise_output_per_call
        } else {
            est_output_without
        };
        let output_saved = est_output_without.saturating_sub(est_output_with);

        let output_cost_without = est_output_without as f64 / 1_000_000.0 * self.output_price_per_m;
        let output_cost_with = est_output_with as f64 / 1_000_000.0 * self.output_price_per_m;

        let total_without = input_cost_without + output_cost_without;
        let total_with = input_cost_with + output_cost_with;

        CostBreakdown {
            input_cost_without,
            input_cost_with,
            output_cost_without,
            output_cost_with,
            total_cost_without: total_without,
            total_cost_with: total_with,
            total_saved: total_without - total_with,
            estimated_output_tokens_without: est_output_without,
            estimated_output_tokens_with: est_output_with,
            output_tokens_saved: output_saved,
        }
    }
}

fn format_usd(amount: f64) -> String {
    if amount >= 0.01 {
        format!("${amount:.2}")
    } else {
        format!("${amount:.3}")
    }
}

fn usd_estimate(tokens: u64) -> String {
    let cost = tokens as f64 * DEFAULT_INPUT_PRICE_PER_M / 1_000_000.0;
    format_usd(cost)
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

fn format_cep_live(lv: &serde_json::Value, t: &Theme) -> String {
    let mut o = Vec::new();
    let r = theme::rst();
    let b = theme::bold();
    let d = theme::dim();

    let score = lv["cep_score"].as_u64().unwrap_or(0) as u32;
    let cache_util = lv["cache_utilization"].as_u64().unwrap_or(0);
    let mode_div = lv["mode_diversity"].as_u64().unwrap_or(0);
    let comp_rate = lv["compression_rate"].as_u64().unwrap_or(0);
    let tok_saved = lv["tokens_saved"].as_u64().unwrap_or(0);
    let tok_orig = lv["tokens_original"].as_u64().unwrap_or(0);
    let tool_calls = lv["tool_calls"].as_u64().unwrap_or(0);
    let cache_hits = lv["cache_hits"].as_u64().unwrap_or(0);
    let total_reads = lv["total_reads"].as_u64().unwrap_or(0);
    let complexity = lv["task_complexity"].as_str().unwrap_or("Standard");

    o.push(String::new());
    o.push(format!(
        "  {icon} {brand} {cep}  {d}Live Session (no historical data yet){r}",
        icon = t.header_icon(),
        brand = t.brand_title(),
        cep = t.section_title("CEP"),
    ));
    o.push(format!("  {ln}", ln = t.border_line(56)));
    o.push(String::new());

    let txt = t.text.fg();
    let sc = t.success.fg();
    let sec = t.secondary.fg();

    o.push(format!(
        "  {b}{txt}CEP Score{r}         {b}{pc}{score:>3}/100{r}",
        pc = t.pct_color(score as f64),
    ));
    o.push(format!(
        "  {b}{txt}Cache Hit Rate{r}    {b}{pc}{cache_util}%{r}  {d}({cache_hits} hits / {total_reads} reads){r}",
        pc = t.pct_color(cache_util as f64),
    ));
    o.push(format!(
        "  {b}{txt}Mode Diversity{r}    {b}{pc}{mode_div}%{r}",
        pc = t.pct_color(mode_div as f64),
    ));
    o.push(format!(
        "  {b}{txt}Compression{r}       {b}{pc}{comp_rate}%{r}  {d}({} → {}){r}",
        format_big(tok_orig),
        format_big(tok_orig.saturating_sub(tok_saved)),
        pc = t.pct_color(comp_rate as f64),
    ));
    o.push(format!(
        "  {b}{txt}Tokens Saved{r}      {b}{sc}{}{r}  {d}(≈ {}){r}",
        format_big(tok_saved),
        usd_estimate(tok_saved),
    ));
    o.push(format!(
        "  {b}{txt}Tool Calls{r}        {b}{sec}{tool_calls}{r}"
    ));
    o.push(format!("  {b}{txt}Complexity{r}        {d}{complexity}{r}"));
    o.push(String::new());
    o.push(format!("  {ln}", ln = t.border_line(56)));
    o.push(format!(
        "  {d}This is live data from the current MCP session.{r}"
    ));
    o.push(format!(
        "  {d}Historical CEP trends appear after more sessions.{r}"
    ));
    o.push(String::new());

    o.join("\n")
}

fn load_mcp_live() -> Option<serde_json::Value> {
    let path = dirs::home_dir()?.join(".lean-ctx/mcp-live.json");
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn format_cep_report() -> String {
    let t = active_theme();
    let store = load();
    let cep = &store.cep;
    let live = load_mcp_live();
    let mut o = Vec::new();
    let r = theme::rst();
    let b = theme::bold();
    let d = theme::dim();

    if cep.sessions == 0 && live.is_none() {
        return format!(
            "{d}No CEP sessions recorded yet.{r}\n\
             Use lean-ctx as an MCP server in your editor to start tracking.\n\
             CEP metrics are recorded automatically during MCP sessions."
        );
    }

    if cep.sessions == 0 {
        if let Some(ref lv) = live {
            return format_cep_live(lv, &t);
        }
    }

    let total_saved = cep
        .total_tokens_original
        .saturating_sub(cep.total_tokens_compressed);
    let overall_compression = if cep.total_tokens_original > 0 {
        total_saved as f64 / cep.total_tokens_original as f64 * 100.0
    } else {
        0.0
    };
    let cache_hit_rate = if cep.total_cache_reads > 0 {
        cep.total_cache_hits as f64 / cep.total_cache_reads as f64 * 100.0
    } else {
        0.0
    };
    let avg_score = if !cep.scores.is_empty() {
        cep.scores.iter().map(|s| s.score as f64).sum::<f64>() / cep.scores.len() as f64
    } else {
        0.0
    };
    let latest_score = cep.scores.last().map(|s| s.score).unwrap_or(0);

    let shell_saved = store
        .total_input_tokens
        .saturating_sub(store.total_output_tokens)
        .saturating_sub(total_saved);
    let total_all_saved = store
        .total_input_tokens
        .saturating_sub(store.total_output_tokens);
    let cep_share = if total_all_saved > 0 {
        total_saved as f64 / total_all_saved as f64 * 100.0
    } else {
        0.0
    };

    let txt = t.text.fg();
    let sc = t.success.fg();
    let sec = t.secondary.fg();
    let wrn = t.warning.fg();

    o.push(String::new());
    o.push(format!(
        "  {icon} {brand} {cep}  {d}Cognitive Efficiency Protocol Report{r}",
        icon = t.header_icon(),
        brand = t.brand_title(),
        cep = t.section_title("CEP"),
    ));
    o.push(format!("  {ln}", ln = t.border_line(56)));
    o.push(String::new());

    o.push(format!(
        "  {b}{txt}CEP Score{r}         {b}{pc}{:>3}/100{r}  {d}(avg: {avg_score:.0}, latest: {latest_score}){r}",
        latest_score,
        pc = t.pct_color(latest_score as f64),
    ));
    o.push(format!(
        "  {b}{txt}Sessions{r}          {b}{sec}{}{r}",
        cep.sessions
    ));
    o.push(format!(
        "  {b}{txt}Cache Hit Rate{r}    {b}{pc}{:.1}%{r}  {d}({} hits / {} reads){r}",
        cache_hit_rate,
        cep.total_cache_hits,
        cep.total_cache_reads,
        pc = t.pct_color(cache_hit_rate),
    ));
    o.push(format!(
        "  {b}{txt}MCP Compression{r}   {b}{pc}{:.1}%{r}  {d}({} → {}){r}",
        overall_compression,
        format_big(cep.total_tokens_original),
        format_big(cep.total_tokens_compressed),
        pc = t.pct_color(overall_compression),
    ));
    o.push(format!(
        "  {b}{txt}Tokens Saved{r}      {b}{sc}{}{r}  {d}(≈ {}){r}",
        format_big(total_saved),
        usd_estimate(total_saved),
    ));
    o.push(String::new());

    o.push(format!("  {}", t.section_title("Savings Breakdown")));
    o.push(format!("  {ln}", ln = t.border_line(56)));

    let bar_w = 30;
    let shell_ratio = if total_all_saved > 0 {
        shell_saved as f64 / total_all_saved as f64
    } else {
        0.0
    };
    let cep_ratio = if total_all_saved > 0 {
        total_saved as f64 / total_all_saved as f64
    } else {
        0.0
    };
    let m = t.muted.fg();
    let shell_bar = theme::pad_right(&t.gradient_bar(shell_ratio, bar_w), bar_w);
    o.push(format!(
        "  {m}Shell Hook{r}   {shell_bar} {b}{:>6}{r} {d}({:.0}%){r}",
        format_big(shell_saved),
        (1.0 - cep_share) * 100.0 / 100.0 * 100.0,
    ));
    let cep_bar = theme::pad_right(&t.gradient_bar(cep_ratio, bar_w), bar_w);
    o.push(format!(
        "  {m}MCP/CEP{r}      {cep_bar} {b}{:>6}{r} {d}({cep_share:.0}%){r}",
        format_big(total_saved),
    ));
    o.push(String::new());

    if total_saved == 0 && cep.modes.is_empty() {
        o.push(format!(
            "  {wrn}⚠  MCP server not configured.{r} Shell hook compresses output, but"
        ));
        o.push(
            "     full token savings require MCP tools (ctx_read, ctx_shell, ctx_search)."
                .to_string(),
        );
        o.push(format!(
            "     Run {sec}lean-ctx setup{r} to auto-configure your editors."
        ));
        o.push(String::new());
    }

    if !cep.modes.is_empty() {
        o.push(format!("  {}", t.section_title("Read Modes Used")));
        o.push(format!("  {ln}", ln = t.border_line(56)));

        let mut sorted_modes: Vec<_> = cep.modes.iter().collect();
        sorted_modes.sort_by(|a, b2| b2.1.cmp(a.1));
        let max_mode = *sorted_modes.first().map(|(_, c)| *c).unwrap_or(&1);
        let max_mode = max_mode.max(1);

        for (mode, count) in &sorted_modes {
            let ratio = **count as f64 / max_mode as f64;
            let bar = theme::pad_right(&t.gradient_bar(ratio, 20), 20);
            o.push(format!("  {sec}{:<14}{r} {:>4}x  {bar}", mode, count,));
        }

        let total_mode_calls: u64 = sorted_modes.iter().map(|(_, c)| **c).sum();
        let full_count = cep.modes.get("full").copied().unwrap_or(0);
        let optimized = total_mode_calls.saturating_sub(full_count);
        let opt_pct = if total_mode_calls > 0 {
            optimized as f64 / total_mode_calls as f64 * 100.0
        } else {
            0.0
        };
        o.push(format!(
            "  {d}{optimized}/{total_mode_calls} reads used optimized modes ({opt_pct:.0}% non-full){r}"
        ));
    }

    if cep.scores.len() >= 2 {
        o.push(String::new());
        o.push(format!("  {}", t.section_title("CEP Score Trend")));
        o.push(format!("  {ln}", ln = t.border_line(56)));

        let score_values: Vec<u64> = cep.scores.iter().map(|s| s.score as u64).collect();
        let spark = t.gradient_sparkline(&score_values);
        o.push(format!("  {spark}"));

        let recent: Vec<_> = cep.scores.iter().rev().take(5).collect();
        for snap in recent.iter().rev() {
            let ts = snap.timestamp.get(..16).unwrap_or(&snap.timestamp);
            let pc = t.pct_color(snap.score as f64);
            o.push(format!(
                "  {m}{ts}{r}  {pc}{b}{:>3}{r}/100  cache:{:>3}%  modes:{:>3}%  {d}{}{r}",
                snap.score, snap.cache_hit_rate, snap.mode_diversity, snap.complexity,
            ));
        }
    }

    o.push(String::new());
    o.push(format!("  {ln}", ln = t.border_line(56)));
    o.push(format!("  {d}Improve your CEP score:{r}"));
    if cache_hit_rate < 50.0 {
        o.push(format!(
            "    {wrn}↑{r} Re-read files with ctx_read to leverage caching"
        ));
    }
    let modes_count = cep.modes.len();
    if modes_count < 3 {
        o.push(format!(
            "    {wrn}↑{r} Use map/signatures modes for context-only files"
        ));
    }
    if avg_score >= 70.0 {
        o.push(format!(
            "    {sc}✓{r} Great score! You're using lean-ctx effectively"
        ));
    }
    o.push(String::new());

    o.join("\n")
}

pub fn format_gain() -> String {
    format_gain_themed(&active_theme())
}

pub fn format_gain_themed(t: &Theme) -> String {
    let store = load();
    let mut o = Vec::new();
    let r = theme::rst();
    let b = theme::bold();
    let d = theme::dim();

    if store.total_commands == 0 {
        return format!(
            "{d}No commands recorded yet.{r} Use {cmd}lean-ctx -c \"command\"{r} to start tracking.",
            cmd = t.secondary.fg(),
        );
    }

    let input_saved = store
        .total_input_tokens
        .saturating_sub(store.total_output_tokens);
    let pct = if store.total_input_tokens > 0 {
        input_saved as f64 / store.total_input_tokens as f64 * 100.0
    } else {
        0.0
    };
    let cost_model = CostModel::default();
    let cost = cost_model.calculate(&store);
    let total_saved = input_saved;
    let days_active = store.daily.len();

    let w = 62;
    let side = t.box_side();

    let box_line = |content: &str| -> String {
        let padded = theme::pad_right(content, w);
        format!("  {side}{padded}{side}")
    };

    o.push(String::new());
    o.push(format!("  {}", t.box_top(w)));
    o.push(box_line(""));

    let header = format!(
        "    {icon}  {b}{title}{r}   {d}Token Savings Dashboard{r}",
        icon = t.header_icon(),
        title = t.brand_title(),
    );
    o.push(box_line(&header));
    o.push(box_line(""));
    o.push(format!("  {}", t.box_mid(w)));
    o.push(box_line(""));

    let tok_val = format_big(total_saved);
    let pct_val = format!("{pct:.1}%");
    let cmd_val = format_num(store.total_commands);
    let usd_val = format_usd(cost.total_saved);

    let c1 = t.success.fg();
    let c2 = t.secondary.fg();
    let c3 = t.warning.fg();
    let c4 = t.accent.fg();

    let kw = 14;
    let v1 = theme::pad_right(&format!("{c1}{b}{tok_val}{r}"), kw);
    let v2 = theme::pad_right(&format!("{c2}{b}{pct_val}{r}"), kw);
    let v3 = theme::pad_right(&format!("{c3}{b}{cmd_val}{r}"), kw);
    let v4 = theme::pad_right(&format!("{c4}{b}{usd_val}{r}"), kw);
    o.push(box_line(&format!("    {v1}{v2}{v3}{v4}")));

    let l1 = theme::pad_right(&format!("{d}tokens saved{r}"), kw);
    let l2 = theme::pad_right(&format!("{d}compression{r}"), kw);
    let l3 = theme::pad_right(&format!("{d}commands{r}"), kw);
    let l4 = theme::pad_right(&format!("{d}USD saved{r}"), kw);
    o.push(box_line(&format!("    {l1}{l2}{l3}{l4}")));
    o.push(box_line(""));
    o.push(format!("  {}", t.box_bottom(w)));

    // Token Guardian Buddy
    {
        let cfg = crate::core::config::Config::load();
        if cfg.buddy_enabled {
            let buddy = crate::core::buddy::BuddyState::compute();
            o.push(crate::core::buddy::format_buddy_block(&buddy, t));
        }
    }

    o.push(String::new());

    let cost_title = t.section_title("Cost Breakdown");
    o.push(format!(
        "  {cost_title}  {d}@ ${}/M input · ${}/M output{r}",
        DEFAULT_INPUT_PRICE_PER_M, DEFAULT_OUTPUT_PRICE_PER_M,
    ));
    o.push(format!("  {ln}", ln = t.border_line(w)));
    o.push(String::new());
    let lbl_w = 20;
    let lbl_without = theme::pad_right(&format!("{m}Without lean-ctx{r}", m = t.muted.fg()), lbl_w);
    let lbl_with = theme::pad_right(&format!("{m}With lean-ctx{r}", m = t.muted.fg()), lbl_w);
    let lbl_saved = theme::pad_right(&format!("{c}{b}You saved{r}", c = t.success.fg()), lbl_w);

    o.push(format!(
        "    {lbl_without} {:>8}   {d}{} input + {} output{r}",
        format_usd(cost.total_cost_without),
        format_usd(cost.input_cost_without),
        format_usd(cost.output_cost_without),
    ));
    o.push(format!(
        "    {lbl_with} {:>8}   {d}{} input + {} output{r}",
        format_usd(cost.total_cost_with),
        format_usd(cost.input_cost_with),
        format_usd(cost.output_cost_with),
    ));
    o.push(String::new());
    o.push(format!(
        "    {lbl_saved} {c}{b}{:>8}{r}   {d}input {} + output {}{r}",
        format_usd(cost.total_saved),
        format_usd(cost.input_cost_without - cost.input_cost_with),
        format_usd(cost.output_cost_without - cost.output_cost_with),
        c = t.success.fg(),
    ));

    o.push(String::new());

    if let (Some(first), Some(_last)) = (&store.first_use, &store.last_use) {
        let first_short = first.get(..10).unwrap_or(first);
        let daily_savings: Vec<u64> = store
            .daily
            .iter()
            .map(|d2| day_total_saved(d2, &cost_model))
            .collect();
        let spark = t.gradient_sparkline(&daily_savings);
        o.push(format!(
            "    {d}Since {first_short} · {days_active} day{plural}{r}   {spark}",
            plural = if days_active != 1 { "s" } else { "" }
        ));
        o.push(String::new());
    }

    o.push(String::new());

    if !store.commands.is_empty() {
        o.push(format!("  {}", t.section_title("Top Commands")));
        o.push(format!("  {ln}", ln = t.border_line(w)));
        o.push(String::new());

        let mut sorted: Vec<_> = store.commands.iter().collect();
        sorted.sort_by(|a, b2| {
            let sa = cmd_total_saved(a.1, &cost_model);
            let sb = cmd_total_saved(b2.1, &cost_model);
            sb.cmp(&sa)
        });

        let max_cmd_saved = sorted
            .first()
            .map(|(_, s)| cmd_total_saved(s, &cost_model))
            .unwrap_or(1)
            .max(1);

        for (cmd, stats) in sorted.iter().take(10) {
            let cmd_saved = cmd_total_saved(stats, &cost_model);
            let cmd_input_saved = stats.input_tokens.saturating_sub(stats.output_tokens);
            let cmd_pct = if stats.input_tokens > 0 {
                cmd_input_saved as f64 / stats.input_tokens as f64 * 100.0
            } else {
                0.0
            };
            let ratio = cmd_saved as f64 / max_cmd_saved as f64;
            let bar = theme::pad_right(&t.gradient_bar(ratio, 22), 22);
            let pc = t.pct_color(cmd_pct);
            let cmd_col = theme::pad_right(
                &format!("{m}{}{r}", truncate_cmd(cmd, 16), m = t.muted.fg()),
                18,
            );
            let saved_col = theme::pad_right(&format!("{b}{pc}{}{r}", format_big(cmd_saved)), 8);
            o.push(format!(
                "    {cmd_col} {:>5}x   {bar}  {saved_col} {d}{cmd_pct:>3.0}%{r}",
                stats.count,
            ));
        }

        if sorted.len() > 10 {
            o.push(format!(
                "    {d}... +{} more commands{r}",
                sorted.len() - 10
            ));
        }
    }

    if store.daily.len() >= 2 {
        o.push(String::new());
        o.push(String::new());
        o.push(format!("  {}", t.section_title("Recent Days")));
        o.push(format!("  {ln}", ln = t.border_line(w)));
        o.push(String::new());

        let recent: Vec<_> = store.daily.iter().rev().take(7).collect();
        for day in recent.iter().rev() {
            let day_saved = day_total_saved(day, &cost_model);
            let day_input_saved = day.input_tokens.saturating_sub(day.output_tokens);
            let day_pct = if day.input_tokens > 0 {
                day_input_saved as f64 / day.input_tokens as f64 * 100.0
            } else {
                0.0
            };
            let pc = t.pct_color(day_pct);
            let date_short = day.date.get(5..).unwrap_or(&day.date);
            let date_col = theme::pad_right(&format!("{m}{date_short}{r}", m = t.muted.fg()), 7);
            let saved_col = theme::pad_right(&format!("{pc}{b}{}{r}", format_big(day_saved)), 9);
            o.push(format!(
                "    {date_col}  {:>5} cmds   {saved_col} saved   {pc}{day_pct:>5.1}%{r}",
                day.commands,
            ));
        }
    }

    o.push(String::new());
    o.push(String::new());

    if let Some(tip) = contextual_tip(&store) {
        o.push(format!("    {w}💡 {tip}{r}", w = t.warning.fg()));
        o.push(String::new());
    }

    // Bug Memory stats
    {
        let project_root = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if !project_root.is_empty() {
            let gotcha_store = crate::core::gotcha_tracker::GotchaStore::load(&project_root);
            if gotcha_store.stats.total_errors_detected > 0 || !gotcha_store.gotchas.is_empty() {
                let a = t.accent.fg();
                o.push(format!("    {a}🧠 Bug Memory{r}"));
                o.push(format!(
                    "    {m}   Active gotchas: {}{r}   Bugs prevented: {}{r}",
                    gotcha_store.gotchas.len(),
                    gotcha_store.stats.total_prevented,
                    m = t.muted.fg(),
                ));
                o.push(String::new());
            }
        }
    }

    let m = t.muted.fg();
    o.push(format!(
        "    {m}🐛 Found a bug? Run: lean-ctx report-issue{r}"
    ));
    o.push(format!(
        "    {m}📊 Help improve lean-ctx: lean-ctx contribute{r}"
    ));
    o.push(format!("    {m}🧠 View bug memory: lean-ctx gotchas{r}"));

    o.push(String::new());
    o.push(String::new());

    o.join("\n")
}

fn contextual_tip(store: &StatsStore) -> Option<String> {
    let tips = build_tips(store);
    if tips.is_empty() {
        return None;
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86400;
    Some(tips[(seed as usize) % tips.len()].clone())
}

fn build_tips(store: &StatsStore) -> Vec<String> {
    let mut tips = Vec::new();

    if store.cep.modes.get("map").copied().unwrap_or(0) == 0 {
        tips.push("Try mode=\"map\" for files you only need as context — shows deps + exports, skips implementation.".into());
    }

    if store.cep.modes.get("signatures").copied().unwrap_or(0) == 0 {
        tips.push("Try mode=\"signatures\" for large files — returns only the API surface.".into());
    }

    if store.cep.total_cache_reads > 0
        && store.cep.total_cache_hits as f64 / store.cep.total_cache_reads as f64 > 0.8
    {
        tips.push(
            "High cache hit rate! Use ctx_compress periodically to keep context compact.".into(),
        );
    }

    if store.total_commands > 50 && store.cep.sessions == 0 {
        tips.push("Use ctx_session to track your task — enables cross-session memory.".into());
    }

    if store.cep.modes.get("entropy").copied().unwrap_or(0) == 0 && store.total_commands > 20 {
        tips.push("Try mode=\"entropy\" for maximum compression on large files.".into());
    }

    if store.daily.len() >= 7 {
        tips.push("Run lean-ctx gain --graph for a 30-day sparkline chart.".into());
    }

    tips.push("Run ctx_overview(task) at session start for a task-aware project map.".into());
    tips.push("Run lean-ctx dashboard for a live web UI with all your stats.".into());

    let cfg = crate::core::config::Config::load();
    if cfg.theme == "default" {
        tips.push(
            "Customize your dashboard! Try: lean-ctx theme set cyberpunk (or neon, ocean, sunset, monochrome)".into(),
        );
        tips.push(
            "Want a unique look? Run lean-ctx theme list to see all available themes.".into(),
        );
    } else {
        tips.push(format!(
            "Current theme: {}. Run lean-ctx theme list to explore others.",
            cfg.theme
        ));
    }

    tips.push(
        "Create your own theme with lean-ctx theme create <name> and set custom colors!".into(),
    );

    tips
}

pub fn gain_live() {
    use std::io::Write;

    let interval = std::time::Duration::from_secs(2);
    let mut line_count = 0usize;
    let d = theme::dim();
    let r = theme::rst();

    eprintln!("  {d}▸ Live mode (2s refresh) · Ctrl+C to exit{r}");

    loop {
        if line_count > 0 {
            print!("\x1B[{line_count}A\x1B[J");
        }

        let output = format_gain();
        let footer = format!("\n  {d}▸ Live · updates every 2s · Ctrl+C to exit{r}\n");
        let full = format!("{output}{footer}");
        line_count = full.lines().count();

        print!("{full}");
        let _ = std::io::stdout().flush();

        std::thread::sleep(interval);
    }
}

pub fn format_gain_graph() -> String {
    let t = active_theme();
    let store = load();
    let r = theme::rst();
    let b = theme::bold();
    let d = theme::dim();

    if store.daily.is_empty() {
        return format!("{d}No daily data yet.{r} Use lean-ctx for a few days to see the graph.");
    }

    let cm = CostModel::default();
    let days: Vec<_> = store
        .daily
        .iter()
        .rev()
        .take(30)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    let savings: Vec<u64> = days.iter().map(|day| day_total_saved(day, &cm)).collect();

    let max_saved = *savings.iter().max().unwrap_or(&1);
    let max_saved = max_saved.max(1);

    let bar_width = 36;
    let mut o = Vec::new();

    o.push(String::new());
    o.push(format!(
        "  {icon} {title}  {d}Token Savings Graph (last 30 days){r}",
        icon = t.header_icon(),
        title = t.brand_title(),
    ));
    o.push(format!("  {ln}", ln = t.border_line(58)));
    o.push(format!(
        "  {d}{:>58}{r}",
        format!("peak: {}", format_big(max_saved))
    ));
    o.push(String::new());

    for (i, day) in days.iter().enumerate() {
        let saved = savings[i];
        let ratio = saved as f64 / max_saved as f64;
        let bar = theme::pad_right(&t.gradient_bar(ratio, bar_width), bar_width);

        let input_saved = day.input_tokens.saturating_sub(day.output_tokens);
        let pct = if day.input_tokens > 0 {
            input_saved as f64 / day.input_tokens as f64 * 100.0
        } else {
            0.0
        };
        let date_short = day.date.get(5..).unwrap_or(&day.date);

        o.push(format!(
            "  {m}{date_short}{r} {brd}│{r} {bar} {b}{:>6}{r} {d}{pct:.0}%{r}",
            format_big(saved),
            m = t.muted.fg(),
            brd = t.border.fg(),
        ));
    }

    let total_saved: u64 = savings.iter().sum();
    let total_cmds: u64 = days.iter().map(|day| day.commands).sum();
    let spark = t.gradient_sparkline(&savings);

    o.push(String::new());
    o.push(format!("  {ln}", ln = t.border_line(58)));
    o.push(format!(
        "  {spark}  {b}{txt}{}{r} saved across {b}{}{r} commands",
        format_big(total_saved),
        format_num(total_cmds),
        txt = t.text.fg(),
    ));
    o.push(String::new());

    o.join("\n")
}

pub fn format_gain_daily() -> String {
    let t = active_theme();
    let store = load();
    let r = theme::rst();
    let b = theme::bold();
    let d = theme::dim();

    if store.daily.is_empty() {
        return format!("{d}No daily data yet.{r}");
    }

    let mut o = Vec::new();
    let w = 64;

    let side = t.box_side();
    let daily_box = |content: &str| -> String {
        let padded = theme::pad_right(content, w);
        format!("  {side}{padded}{side}")
    };

    o.push(String::new());
    o.push(format!(
        "  {icon} {title}  {d}Daily Breakdown{r}",
        icon = t.header_icon(),
        title = t.brand_title(),
    ));
    o.push(format!("  {}", t.box_top(w)));
    let hdr = format!(
        " {b}{txt}{:<12} {:>6}  {:>10}  {:>10}  {:>7}  {:>6}{r}",
        "Date",
        "Cmds",
        "Input",
        "Saved",
        "Rate",
        "USD",
        txt = t.text.fg(),
    );
    o.push(daily_box(&hdr));
    o.push(format!("  {}", t.box_mid(w)));

    let days: Vec<_> = store
        .daily
        .iter()
        .rev()
        .take(30)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .cloned()
        .collect();

    let cm = CostModel::default();
    for day in &days {
        let saved = day_total_saved(day, &cm);
        let input_saved = day.input_tokens.saturating_sub(day.output_tokens);
        let pct = if day.input_tokens > 0 {
            input_saved as f64 / day.input_tokens as f64 * 100.0
        } else {
            0.0
        };
        let pc = t.pct_color(pct);
        let usd = usd_estimate(saved);
        let row = format!(
            " {m}{:<12}{r} {:>6}  {:>10}  {pc}{b}{:>10}{r}  {pc}{:>6.1}%{r}  {d}{:>6}{r}",
            &day.date,
            day.commands,
            format_big(day.input_tokens),
            format_big(saved),
            pct,
            usd,
            m = t.muted.fg(),
        );
        o.push(daily_box(&row));
    }

    let total_input: u64 = store.daily.iter().map(|day| day.input_tokens).sum();
    let total_saved: u64 = store
        .daily
        .iter()
        .map(|day| day_total_saved(day, &cm))
        .sum();
    let total_pct = if total_input > 0 {
        let input_saved: u64 = store
            .daily
            .iter()
            .map(|day| day.input_tokens.saturating_sub(day.output_tokens))
            .sum();
        input_saved as f64 / total_input as f64 * 100.0
    } else {
        0.0
    };
    let total_usd = usd_estimate(total_saved);
    let sc = t.success.fg();

    o.push(format!("  {}", t.box_mid(w)));
    let total_row = format!(
        " {b}{txt}{:<12}{r} {:>6}  {:>10}  {sc}{b}{:>10}{r}  {sc}{b}{:>6.1}%{r}  {b}{:>6}{r}",
        "TOTAL",
        format_num(store.total_commands),
        format_big(total_input),
        format_big(total_saved),
        total_pct,
        total_usd,
        txt = t.text.fg(),
    );
    o.push(daily_box(&total_row));
    o.push(format!("  {}", t.box_bottom(w)));

    let daily_savings: Vec<u64> = days.iter().map(|day| day_total_saved(day, &cm)).collect();
    let spark = t.gradient_sparkline(&daily_savings);
    o.push(format!("  {d}Trend:{r} {spark}"));
    o.push(String::new());

    o.join("\n")
}

pub fn format_gain_json() -> String {
    let store = load();
    serde_json::to_string_pretty(&store).unwrap_or_else(|_| "{}".to_string())
}
