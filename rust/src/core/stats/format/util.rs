//! Shared number/string formatting helpers for the stats views.

use crate::core::theme::{self, Theme};

use super::super::model::{CommandStats, CostModel, DayStats};

pub(super) fn active_theme() -> Theme {
    let cfg = crate::core::config::Config::load();
    theme::load_theme(&cfg.theme)
}

pub(super) fn format_usd(amount: f64) -> String {
    if amount >= 0.01 {
        format!("${amount:.2}")
    } else {
        format!("${amount:.3}")
    }
}

pub(super) fn usd_estimate(tokens: u64) -> String {
    let env_model = std::env::var("LEAN_CTX_MODEL")
        .or_else(|_| std::env::var("LCTX_MODEL"))
        .ok();
    let pricing = crate::core::gain::model_pricing::ModelPricing::load();
    let quote = pricing.quote(env_model.as_deref());
    let cost = tokens as f64 * quote.cost.input_per_m / 1_000_000.0;
    format_usd(cost)
}

pub(super) fn format_pct_1dp(val: f64) -> String {
    if val == 0.0 {
        "0.0%".to_string()
    } else if val > 0.0 && val < 0.1 {
        "<0.1%".to_string()
    } else {
        format!("{val:.1}%")
    }
}

pub(super) fn format_big(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

pub(super) fn format_num(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{},{:03}", n / 1_000, n % 1_000)
    } else {
        format!("{n}")
    }
}

pub(super) fn truncate_cmd(cmd: &str, max: usize) -> String {
    if cmd.len() <= max {
        cmd.to_string()
    } else {
        format!("{}…", &cmd[..max - 1])
    }
}

pub(super) fn cmd_total_saved(s: &CommandStats, _cm: &CostModel) -> u64 {
    s.input_tokens.saturating_sub(s.output_tokens)
}

pub(super) fn day_total_saved(d: &DayStats, _cm: &CostModel) -> u64 {
    d.input_tokens.saturating_sub(d.output_tokens)
}

pub(crate) fn normalize_command(command: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::format_pct_1dp;

    #[test]
    fn format_pct_1dp_normal() {
        assert_eq!(format_pct_1dp(50.0), "50.0%");
        assert_eq!(format_pct_1dp(100.0), "100.0%");
        assert_eq!(format_pct_1dp(33.333), "33.3%");
    }

    #[test]
    fn format_pct_1dp_small_values() {
        assert_eq!(format_pct_1dp(0.0), "0.0%");
        assert_eq!(format_pct_1dp(0.05), "<0.1%");
        assert_eq!(format_pct_1dp(0.09), "<0.1%");
        assert_eq!(format_pct_1dp(0.1), "0.1%");
        assert_eq!(format_pct_1dp(0.5), "0.5%");
    }
}
