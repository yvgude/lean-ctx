use std::sync::atomic::{AtomicU64, Ordering};

static LAST_ELICITATION_SEQ: AtomicU64 = AtomicU64::new(0);
static TOOL_CALL_SEQ: AtomicU64 = AtomicU64::new(0);

const MIN_CALLS_BETWEEN_ELICITATION: u64 = 20;
const PRESSURE_THRESHOLD: f64 = 0.90;
const LARGE_FILE_TOKENS: usize = 5000;

pub fn increment_call() -> u64 {
    TOOL_CALL_SEQ.fetch_add(1, Ordering::Relaxed) + 1
}

#[derive(Debug, Clone)]
pub enum ElicitationSuggestion {
    PressureEviction {
        utilization_pct: f64,
        candidates: Vec<String>,
    },
    LargeFileMode {
        path: String,
        tokens: usize,
    },
    BudgetExhausted {
        utilization_pct: f64,
    },
}

impl ElicitationSuggestion {
    #[must_use]
    pub fn format_fallback_hint(&self) -> String {
        match self {
            Self::PressureEviction {
                utilization_pct,
                candidates,
            } => {
                let names = candidates.join(", ");
                format!(
                    "[Context {utilization_pct:.0}% full] Evict: ctx_ledger(action=\"evict\", targets=\"{names}\")"
                )
            }
            Self::LargeFileMode { path, tokens } => {
                format!(
                    "[Large file: {path} ({tokens} tok)] Consider: ctx_read(\"{path}\", mode=\"map\") or mode=\"signatures\""
                )
            }
            Self::BudgetExhausted { utilization_pct } => {
                format!(
                    "[Budget {utilization_pct:.0}% used] Consider: ctx_control(action=\"set_view\", target=\"<large_file>\", value=\"signatures\")"
                )
            }
        }
    }
}

pub fn check_elicitation_needed(
    ledger: &crate::core::context_ledger::ContextLedger,
    current_path: Option<&str>,
    current_tokens: Option<usize>,
) -> Option<ElicitationSuggestion> {
    let current_seq = TOOL_CALL_SEQ.load(Ordering::Relaxed);
    let last = LAST_ELICITATION_SEQ.load(Ordering::Relaxed);
    if current_seq.saturating_sub(last) < MIN_CALLS_BETWEEN_ELICITATION {
        return None;
    }

    let pressure = ledger.pressure();

    if pressure.utilization > PRESSURE_THRESHOLD {
        let candidates = ledger.eviction_candidates_by_phi(3);
        if !candidates.is_empty() {
            LAST_ELICITATION_SEQ.store(current_seq, Ordering::Relaxed);
            let short_names: Vec<_> = candidates
                .iter()
                .take(5)
                .map(|p| crate::core::protocol::shorten_path(p))
                .collect();
            return Some(ElicitationSuggestion::PressureEviction {
                utilization_pct: pressure.utilization * 100.0,
                candidates: short_names,
            });
        }
    }

    if let (Some(path), Some(tokens)) = (current_path, current_tokens)
        && tokens > LARGE_FILE_TOKENS
    {
        LAST_ELICITATION_SEQ.store(current_seq, Ordering::Relaxed);
        return Some(ElicitationSuggestion::LargeFileMode {
            path: path.to_string(),
            tokens,
        });
    }

    if pressure.utilization > 0.95 {
        LAST_ELICITATION_SEQ.store(current_seq, Ordering::Relaxed);
        return Some(ElicitationSuggestion::BudgetExhausted {
            utilization_pct: pressure.utilization * 100.0,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_elicitation_on_low_pressure() {
        for _ in 0..25 {
            increment_call();
        }
        let ledger = crate::core::context_ledger::ContextLedger::new();
        let result = check_elicitation_needed(&ledger, None, None);
        assert!(result.is_none());
    }

    #[test]
    fn fallback_hint_format() {
        let hint = ElicitationSuggestion::PressureEviction {
            utilization_pct: 92.0,
            candidates: vec!["auth.rs".to_string(), "db.rs".to_string()],
        };
        let text = hint.format_fallback_hint();
        assert!(text.contains("92%"));
        assert!(text.contains("auth.rs"));
    }

    #[test]
    fn large_file_hint_format() {
        let hint = ElicitationSuggestion::LargeFileMode {
            path: "big.rs".to_string(),
            tokens: 8000,
        };
        let text = hint.format_fallback_hint();
        assert!(text.contains("8000"));
        assert!(text.contains("big.rs"));
    }
}
