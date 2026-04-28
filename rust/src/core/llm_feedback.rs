use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const LLM_FEEDBACK_FILE: &str = "llm_feedback.jsonl";
const LLM_FEEDBACK_MAX_EVENTS: usize = 5_000;
const LLM_FEEDBACK_MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmFeedbackEvent {
    pub agent_id: String,
    pub intent: Option<String>,
    pub model: Option<String>,
    pub llm_input_tokens: u64,
    pub llm_output_tokens: u64,
    pub latency_ms: Option<u64>,
    pub note: Option<String>,
    pub ctx_read_last_mode: Option<String>,
    pub ctx_read_modes: Option<BTreeMap<String, u64>>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmFeedbackSummary {
    pub total_events: usize,
    pub avg_output_ratio: f64,
    pub avg_latency_ms: Option<f64>,
    pub max_output_tokens: u64,
    pub max_output_ratio: f64,
    pub by_model: BTreeMap<String, ModelSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelSummary {
    pub events: usize,
    pub avg_output_ratio: f64,
    pub avg_latency_ms: Option<f64>,
    pub max_output_tokens: u64,
}

pub struct LlmFeedbackStore;

impl LlmFeedbackStore {
    pub fn record(mut event: LlmFeedbackEvent) -> Result<(), String> {
        if event.agent_id.trim().is_empty() {
            return Err("agent_id is required".to_string());
        }
        if event.llm_input_tokens == 0 {
            return Err("llm_input_tokens must be > 0".to_string());
        }
        if event.llm_output_tokens == 0 {
            return Err("llm_output_tokens must be > 0".to_string());
        }
        if let Some(n) = event.note.as_ref() {
            if n.len() > 2000 {
                event.note = Some(n.chars().take(2000).collect());
            }
        }

        let path = feedback_path();
        ensure_parent_dir(&path)?;

        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;

        let line = serde_json::to_string(&event).map_err(|e| format!("serialize: {e}"))?;
        f.write_all(line.as_bytes())
            .and_then(|()| f.write_all(b"\n"))
            .map_err(|e| format!("write {}: {e}", path.display()))?;

        maybe_compact(&path)?;
        Ok(())
    }

    pub fn status() -> LlmFeedbackStatus {
        let path = feedback_path();
        let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
        LlmFeedbackStatus {
            path,
            bytes,
            max_events: LLM_FEEDBACK_MAX_EVENTS,
            max_bytes: LLM_FEEDBACK_MAX_BYTES,
        }
    }

    pub fn reset() -> Result<(), String> {
        let path = feedback_path();
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("remove {}: {e}", path.display()))?;
        }
        Ok(())
    }

    pub fn recent(limit: usize) -> Vec<LlmFeedbackEvent> {
        let path = feedback_path();
        let mut out: VecDeque<LlmFeedbackEvent> = VecDeque::with_capacity(limit.max(1));
        let Ok(f) = File::open(&path) else {
            return Vec::new();
        };
        let reader = BufReader::new(f);
        for line in reader.lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<LlmFeedbackEvent>(&line) {
                out.push_back(ev);
                while out.len() > limit {
                    out.pop_front();
                }
            }
        }
        out.into_iter().collect()
    }

    pub fn summarize(limit: usize) -> LlmFeedbackSummary {
        let events = Self::recent(limit);
        summarize_events(&events)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmFeedbackStatus {
    pub path: PathBuf,
    pub bytes: u64,
    pub max_events: usize,
    pub max_bytes: u64,
}

fn summarize_events(events: &[LlmFeedbackEvent]) -> LlmFeedbackSummary {
    if events.is_empty() {
        return LlmFeedbackSummary::default();
    }

    let mut by_model: BTreeMap<String, Vec<&LlmFeedbackEvent>> = BTreeMap::new();
    let mut ratio_sum = 0.0;
    let mut ratio_max: f64 = 0.0;
    let mut max_out = 0u64;
    let mut latency_sum = 0u64;
    let mut latency_n = 0u64;

    for ev in events {
        let ratio = ev.llm_output_tokens as f64 / ev.llm_input_tokens.max(1) as f64;
        ratio_sum += ratio;
        ratio_max = ratio_max.max(ratio);
        max_out = max_out.max(ev.llm_output_tokens);
        if let Some(ms) = ev.latency_ms {
            latency_sum = latency_sum.saturating_add(ms);
            latency_n += 1;
        }
        by_model
            .entry(ev.model.clone().unwrap_or_else(|| "unknown".to_string()))
            .or_default()
            .push(ev);
    }

    let avg_latency_ms = if latency_n > 0 {
        Some(latency_sum as f64 / latency_n as f64)
    } else {
        None
    };

    let mut model_summaries = BTreeMap::new();
    for (model, evs) in by_model {
        let mut r_sum = 0.0;
        let mut max_out = 0u64;
        let mut l_sum = 0u64;
        let mut l_n = 0u64;
        let n = evs.len();
        for ev in &evs {
            r_sum += ev.llm_output_tokens as f64 / ev.llm_input_tokens.max(1) as f64;
            max_out = max_out.max(ev.llm_output_tokens);
            if let Some(ms) = ev.latency_ms {
                l_sum = l_sum.saturating_add(ms);
                l_n += 1;
            }
        }
        model_summaries.insert(
            model,
            ModelSummary {
                events: n,
                avg_output_ratio: r_sum / n.max(1) as f64,
                avg_latency_ms: if l_n > 0 {
                    Some(l_sum as f64 / l_n as f64)
                } else {
                    None
                },
                max_output_tokens: max_out,
            },
        );
    }

    LlmFeedbackSummary {
        total_events: events.len(),
        avg_output_ratio: ratio_sum / events.len().max(1) as f64,
        avg_latency_ms,
        max_output_tokens: max_out,
        max_output_ratio: ratio_max,
        by_model: model_summaries,
    }
}

fn feedback_path() -> PathBuf {
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("feedback")
        .join(LLM_FEEDBACK_FILE)
}

fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|e| format!("create_dir_all {}: {e}", parent.display()))
}

fn maybe_compact(path: &Path) -> Result<(), String> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if meta.len() <= LLM_FEEDBACK_MAX_BYTES {
        return Ok(());
    }

    let f = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let reader = BufReader::new(f);

    let mut keep: VecDeque<String> = VecDeque::with_capacity(LLM_FEEDBACK_MAX_EVENTS);
    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        keep.push_back(line);
        while keep.len() > LLM_FEEDBACK_MAX_EVENTS {
            keep.pop_front();
        }
    }

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(".llm_feedback.compact.tmp");
    {
        let mut out = File::create(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
        for line in keep {
            out.write_all(line.as_bytes())
                .and_then(|()| out.write_all(b"\n"))
                .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        }
        out.flush()
            .map_err(|e| format!("flush {}: {e}", tmp.display()))?;
    }

    std::fs::rename(&tmp, path)
        .map_err(|e| format!("rename {} -> {}: {e}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_empty_is_default() {
        let s = summarize_events(&[]);
        assert_eq!(s.total_events, 0);
        assert!(s.by_model.is_empty());
    }
}
