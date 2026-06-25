use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const STORE_FILENAME: &str = "autonomy_drivers_v1.json";

// Hard bounds: autonomy reports are observability artifacts.
const MAX_EVENTS: usize = 128;
const MAX_DECISIONS_PER_EVENT: usize = 16;
const MAX_TOOL_CHARS: usize = 64;
const MAX_ACTION_CHARS: usize = 64;
const MAX_REASON_CODE_CHARS: usize = 48;
const MAX_REASON_CHARS: usize = 220;
const MAX_DETAIL_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyPhaseV1 {
    PreCall,
    PostRead,
    PostCall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyDriverKindV1 {
    Preload,
    Prefetch,
    Dedup,
    Response,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyVerdictV1 {
    Run,
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomyDriverDecisionV1 {
    pub driver: AutonomyDriverKindV1,
    pub verdict: AutonomyVerdictV1,
    pub reason_code: String,
    pub reason: String,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomyDriverEventV1 {
    pub seq: u64,
    pub created_at: String,
    pub phase: AutonomyPhaseV1,
    pub role: String,
    pub profile: String,
    pub tool: String,
    #[serde(default)]
    pub action: Option<String>,
    pub decisions: Vec<AutonomyDriverDecisionV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomyDriversV1 {
    pub schema_version: u32,
    pub created_at: String,
    pub updated_at: String,
    pub next_seq: u64,
    #[serde(default)]
    pub events: Vec<AutonomyDriverEventV1>,
}

impl Default for AutonomyDriversV1 {
    fn default() -> Self {
        Self::new()
    }
}

fn store_path() -> Option<PathBuf> {
    crate::core::paths::cache_dir()
        .ok()
        .map(|d| d.join(STORE_FILENAME))
}

impl AutonomyDriversV1 {
    #[must_use]
    pub fn new() -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            schema_version: crate::core::contracts::AUTONOMY_DRIVERS_V1_SCHEMA_VERSION,
            created_at: now.clone(),
            updated_at: now,
            next_seq: 1,
            events: Vec::new(),
        }
    }

    #[must_use]
    pub fn load() -> Self {
        let Some(path) = store_path() else {
            return Self::new();
        };
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        serde_json::from_str::<Self>(&content).unwrap_or_else(|_| Self::new())
    }

    pub fn save(&self) -> Result<(), String> {
        let Some(path) = store_path() else {
            return Err("no data dir".to_string());
        };
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // Reports may end up in CI logs; always redact (even for admin).
        let json = crate::core::redaction::redact_text(&json);
        crate::config_io::write_atomic(&path, &json)?;
        Ok(())
    }

    pub fn record(&mut self, mut ev: AutonomyDriverEventV1) {
        ev.seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.updated_at = chrono::Utc::now().to_rfc3339();

        bound_event_in_place(&mut ev);
        self.events.push(ev);
        self.prune_in_place();
    }

    #[must_use]
    pub fn latest(&self) -> Option<&AutonomyDriverEventV1> {
        self.events.last()
    }

    fn prune_in_place(&mut self) {
        if self.events.len() <= MAX_EVENTS {
            return;
        }
        let overflow = self.events.len() - MAX_EVENTS;
        self.events.drain(0..overflow);
    }
}

fn bound_event_in_place(ev: &mut AutonomyDriverEventV1) {
    ev.tool = truncate(&ev.tool, MAX_TOOL_CHARS);
    if let Some(a) = ev.action.clone() {
        let t = truncate(&a, MAX_ACTION_CHARS);
        ev.action = if t.trim().is_empty() { None } else { Some(t) };
    }
    if ev.decisions.len() > MAX_DECISIONS_PER_EVENT {
        ev.decisions.truncate(MAX_DECISIONS_PER_EVENT);
    }
    for d in &mut ev.decisions {
        d.reason_code = truncate(&d.reason_code, MAX_REASON_CODE_CHARS);
        d.reason = truncate(&d.reason, MAX_REASON_CHARS);
        if let Some(detail) = d.detail.clone() {
            let t = truncate(&detail, MAX_DETAIL_CHARS);
            d.detail = if t.trim().is_empty() { None } else { Some(t) };
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let mut out = s[..max].to_string();
    out.push('…');
    out
}

pub fn write_project_autonomy_drivers_v1(
    project_root: &Path,
    drivers: &AutonomyDriversV1,
    filename: Option<&str>,
) -> Result<PathBuf, String> {
    let proofs_dir = crate::core::pathutil::safe_project_data_dir(project_root)?.join("proofs");
    std::fs::create_dir_all(&proofs_dir).map_err(|e| e.to_string())?;

    let ts = chrono::Utc::now().format("%Y-%m-%d_%H%M%S");
    let name = filename.map_or_else(
        || format!("autonomy-drivers-v1_{ts}.json"),
        std::string::ToString::to_string,
    );
    let path = proofs_dir.join(name);

    let json = serde_json::to_string_pretty(drivers).map_err(|e| e.to_string())?;
    let json = crate::core::redaction::redact_text(&json);
    crate::config_io::write_atomic(&path, &json)?;
    Ok(path)
}

#[must_use]
pub fn format_compact_event(ev: &AutonomyDriverEventV1) -> String {
    let mut parts = Vec::new();
    for d in &ev.decisions {
        let driver = match d.driver {
            AutonomyDriverKindV1::Preload => "preload",
            AutonomyDriverKindV1::Prefetch => "prefetch",
            AutonomyDriverKindV1::Dedup => "dedup",
            AutonomyDriverKindV1::Response => "response",
        };
        let verdict = match d.verdict {
            AutonomyVerdictV1::Run => "run",
            AutonomyVerdictV1::Skip => "skip",
        };
        parts.push(format!("{driver}={verdict}({})", d.reason_code));
    }
    format!(
        "[autonomy:{}] {}",
        match ev.phase {
            AutonomyPhaseV1::PreCall => "pre",
            AutonomyPhaseV1::PostRead => "read",
            AutonomyPhaseV1::PostCall => "post",
        },
        parts.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_bounded_and_seq_increments() {
        let mut s = AutonomyDriversV1::new();
        for i in 0..(MAX_EVENTS + 5) {
            s.record(AutonomyDriverEventV1 {
                seq: 0,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                phase: AutonomyPhaseV1::PreCall,
                role: "coder".to_string(),
                profile: "exploration".to_string(),
                tool: format!("tool{i}"),
                action: None,
                decisions: vec![AutonomyDriverDecisionV1 {
                    driver: AutonomyDriverKindV1::Preload,
                    verdict: AutonomyVerdictV1::Skip,
                    reason_code: "disabled".to_string(),
                    reason: "disabled".to_string(),
                    detail: None,
                }],
            });
        }
        assert!(s.events.len() <= MAX_EVENTS);
        assert_eq!(s.events.last().unwrap().seq, s.next_seq - 1);
    }

    #[test]
    fn compact_format_includes_phase_and_drivers() {
        let ev = AutonomyDriverEventV1 {
            seq: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            phase: AutonomyPhaseV1::PostCall,
            role: "coder".to_string(),
            profile: "exploration".to_string(),
            tool: "ctx_read".to_string(),
            action: Some("full".to_string()),
            decisions: vec![AutonomyDriverDecisionV1 {
                driver: AutonomyDriverKindV1::Response,
                verdict: AutonomyVerdictV1::Run,
                reason_code: "output_large".to_string(),
                reason: "output large".to_string(),
                detail: None,
            }],
        };
        let s = format_compact_event(&ev);
        assert!(s.contains("autonomy:post"));
        assert!(s.contains("response=run"));
    }
}
