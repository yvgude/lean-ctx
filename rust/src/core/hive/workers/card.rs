// SPDX-License-Identifier: Apache-2.0
//! The capability card a worker presents (C0 2.0 `worker.schema.json`
//! `$defs/card`; SPEC-ENGINE §3.6). `history` is server-computed and
//! never sent.

use serde::{Deserialize, Serialize};

/// Most tools a card lists, and the longest tool or model name.
const TOOLS_MAX: usize = 64;
const NAME_MAX: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Online,
    Idle,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    SelfReported,
    Unknown,
}

/// Pledge summary; USD amounts are integer cents (C0 carries no floats).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks_per_day: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd_cents_per_day: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCard {
    pub host_app: String,
    pub model: String,
    pub model_source: ModelSource,
    pub tools: Vec<String>,
    pub budget: Budget,
    pub availability: Availability,
}

impl WorkerCard {
    /// A card within the contract bounds: the model is the host's
    /// self-report when there is one, else `unknown`; tools are the lean-ctx
    /// tool names the host was offered, deduplicated, sorted and capped.
    pub fn new(
        host_app: &str,
        model: Option<&str>,
        tools: impl IntoIterator<Item = String>,
        budget: Budget,
        availability: Availability,
    ) -> Self {
        let model = model
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(|m| m.chars().take(NAME_MAX).collect::<String>());
        let mut tools: Vec<String> = tools
            .into_iter()
            .filter(|t| !t.is_empty() && t.chars().count() <= NAME_MAX)
            .collect();
        tools.sort();
        tools.dedup();
        tools.truncate(TOOLS_MAX);
        let budget = Budget {
            tasks_per_day: budget.tasks_per_day.map(|n| n.clamp(1, 200)),
            usd_cents_per_day: budget.usd_cents_per_day.map(|n| n.min(50_000)),
        };
        Self {
            host_app: host_app.to_string(),
            model_source: if model.is_some() {
                ModelSource::SelfReported
            } else {
                ModelSource::Unknown
            },
            model: model.unwrap_or_else(|| "unknown".to_string()),
            tools,
            budget,
            availability,
        }
    }
}

/// The model the host reports about itself, from its environment.
pub fn self_reported_model<I, K, V>(env: I) -> Option<String>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    const KEYS: [&str; 4] = [
        "ANTHROPIC_MODEL",
        "CLAUDE_CODE_MODEL",
        "CODEX_MODEL",
        "GEMINI_MODEL",
    ];
    let mut found: Vec<(usize, String)> = env
        .into_iter()
        .filter_map(|(k, v)| {
            let rank = KEYS.iter().position(|key| *key == k.as_ref())?;
            let v = v.as_ref().trim();
            (!v.is_empty()).then(|| (rank, v.to_string()))
        })
        .collect();
    found.sort();
    found.into_iter().next().map(|(_, v)| v)
}
