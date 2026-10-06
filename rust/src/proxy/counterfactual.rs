//! Counterfactual savings metering (#701) — provider-authoritative receipts.
//!
//! Local tokenizer counts (`o200k_base`) are an *estimate* of what a request
//! would have cost without lean-ctx. Anthropic's `count_tokens` endpoint is
//! **free** and takes the identical body shape, so for each rewritten
//! `/v1/messages` request the proxy can fire a probe with the original,
//! uncompressed body concurrently with the real forward and read back the
//! provider-counted answer: "this exact request would have billed N input
//! tokens". Paired with the billed `usage` block of the same response, that is
//! a confound-free, provider-authoritative saving per request — the
//! methodology pxpipe's FAQ documents, adopted per #701.
//!
//! Isolation guarantees:
//! - The probe **never** mutates, delays or fails the forwarded request: it is
//!   spawned as a detached task and its result lands in a lock-free slot the
//!   usage recorder reads at response end (streams end seconds later, so the
//!   probe has long finished; a slow probe merely degrades that row to the
//!   local estimate).
//! - Opt-in (`proxy.counterfactual_metering`, default off) and fired only for
//!   requests the proxy actually rewrote — an untouched body's billed input
//!   *is* its counterfactual, no probe needed.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::http::request::Parts;
use serde_json::Value;

/// Probe timeout: `count_tokens` typically answers in well under a second; a
/// probe slower than the model's own response is useless (the row degrades to
/// the estimate), so give up early and free the connection.
const PROBE_TIMEOUT_SECS: u64 = 10;

/// Lock-free result slot shared between the detached probe task and the usage
/// recorder. `0` means "no provider count" (pending or failed) — a real
/// `count_tokens` answer is never 0 (`model` + `messages` always tokenize to
/// something), and [`CounterfactualSlot::set`] clamps to ≥ 1 regardless.
#[derive(Clone, Debug, Default)]
pub struct CounterfactualSlot(Arc<AtomicU64>);

impl PartialEq for CounterfactualSlot {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl CounterfactualSlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, tokens: u64) {
        self.0.store(tokens.max(1), Ordering::Relaxed);
    }

    pub fn get(&self) -> Option<u64> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n),
        }
    }
}

/// The exact parameter set Anthropic's `count_tokens` accepts — it rejects
/// unknown fields (`max_tokens`, `stream`, `metadata`, … → 400), so the probe
/// body is a whitelist projection of the original request, never the request
/// itself.
const COUNT_TOKENS_FIELDS: &[&str] = &[
    "model",
    "messages",
    "system",
    "tools",
    "tool_choice",
    "thinking",
];

/// Project the original (pre-compression) request body onto the
/// `count_tokens` parameter set. `original_model` restores the client's model
/// when the router rewrote it in place — the counterfactual asks what the
/// *original* request would have cost. Returns `None` when the body has no
/// `model`/`messages` (nothing meaningful to count). Read-only: the forwarded
/// request is never touched (#701 regression contract).
pub(crate) fn probe_body(original: &Value, original_model: Option<&str>) -> Option<Vec<u8>> {
    let obj = original.as_object()?;
    if !obj.contains_key("model") || !obj.contains_key("messages") {
        return None;
    }
    let mut probe = serde_json::Map::new();
    for &field in COUNT_TOKENS_FIELDS {
        if let Some(v) = obj.get(field) {
            probe.insert(field.to_string(), v.clone());
        }
    }
    if let Some(model) = original_model {
        probe.insert("model".to_string(), Value::String(model.to_string()));
    }
    serde_json::to_vec(&Value::Object(probe)).ok()
}

/// Auth/version headers the probe replays from the client request. Everything
/// else (content-encoding, content-length, tracing) is request-specific and
/// must not leak onto the probe.
const PROBE_HEADERS: &[&str] = &[
    "x-api-key",
    "authorization",
    "anthropic-version",
    "anthropic-beta",
];

/// Fire the free `count_tokens` probe for a rewritten Anthropic request, iff
/// counterfactual metering is enabled. Returns the slot the usage recorder
/// polls at response end, or `None` when no probe was spawned (feature off,
/// non-messages path, unparseable body). Never blocks: the probe runs as a
/// detached task; every failure mode just leaves the slot empty.
pub(crate) fn maybe_spawn_probe(
    client: &reqwest::Client,
    parts: &Parts,
    upstream_base: &str,
    original: Option<&Value>,
    original_model: Option<&str>,
    request_was_rewritten: bool,
) -> Option<CounterfactualSlot> {
    if !request_was_rewritten
        || !parts
            .uri
            .path()
            .trim_end_matches('/')
            .ends_with("/v1/messages")
        || !crate::core::config::Config::load()
            .proxy
            .counterfactual_metering_enabled()
    {
        return None;
    }
    // The probe is a second egress to the provider: it carries the admitted
    // request, never content the gateway masked or withheld (G6).
    let target = crate::core::context_admission::egress::EgressTarget {
        provider: "Anthropic",
        model: original_model,
        upstream_base,
    };
    let admitted = crate::core::context_admission::egress::admitted_copy(original?, &target)?;
    let body = probe_body(&admitted, original_model)?;

    let url = format!(
        "{}/v1/messages/count_tokens",
        upstream_base.trim_end_matches('/')
    );
    let mut req = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(PROBE_TIMEOUT_SECS))
        .header("content-type", "application/json")
        .body(body);
    for &name in PROBE_HEADERS {
        if let Some(v) = parts.headers.get(name) {
            req = req.header(name, v.clone());
        }
    }

    let slot = CounterfactualSlot::new();
    let task_slot = slot.clone();
    tokio::spawn(async move {
        match req.send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
                Ok(v) => {
                    if let Some(tokens) = v.get("input_tokens").and_then(Value::as_u64) {
                        task_slot.set(tokens);
                    } else {
                        tracing::debug!("counterfactual probe: response without input_tokens");
                    }
                }
                Err(e) => tracing::debug!("counterfactual probe: unreadable response: {e}"),
            },
            Ok(resp) => tracing::debug!(
                "counterfactual probe: count_tokens returned {}",
                resp.status()
            ),
            Err(e) => tracing::debug!("counterfactual probe: {e}"),
        }
    });
    Some(slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn full_request() -> Value {
        json!({
            "model": "claude-sonnet-4",
            "messages": [{"role": "user", "content": "hello"}],
            "system": "be terse",
            "tools": [{"name": "get_weather", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "auto"},
            "thinking": {"type": "enabled", "budget_tokens": 1024},
            "max_tokens": 4096,
            "stream": true,
            "temperature": 0.7,
            "metadata": {"user_id": "u1"}
        })
    }

    #[test]
    fn probe_body_is_the_count_tokens_whitelist() {
        let body = probe_body(&full_request(), None).expect("probe body");
        let v: Value = serde_json::from_slice(&body).unwrap();
        let obj = v.as_object().unwrap();

        // Everything count_tokens accepts is carried over…
        for field in COUNT_TOKENS_FIELDS {
            assert!(obj.contains_key(*field), "{field} must be projected");
        }
        // …and everything it rejects with a 400 is dropped.
        for rejected in ["max_tokens", "stream", "temperature", "metadata"] {
            assert!(!obj.contains_key(rejected), "{rejected} must be stripped");
        }
        assert_eq!(obj["model"], "claude-sonnet-4");
    }

    #[test]
    fn probe_body_restores_the_prerouting_model() {
        // The router downgraded the model in the body; the counterfactual asks
        // what the ORIGINAL request would have cost.
        let mut req = full_request();
        req["model"] = json!("claude-haiku-3.5");
        let body = probe_body(&req, Some("claude-sonnet-4")).unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["model"], "claude-sonnet-4");
    }

    #[test]
    fn probe_body_requires_model_and_messages() {
        assert!(probe_body(&json!({"messages": []}), None).is_none());
        assert!(probe_body(&json!({"model": "m"}), None).is_none());
        assert!(probe_body(&json!("not an object"), None).is_none());
    }

    #[test]
    fn slot_roundtrip_and_zero_means_empty() {
        let slot = CounterfactualSlot::new();
        assert_eq!(slot.get(), None, "fresh slot is empty");
        slot.set(1234);
        assert_eq!(slot.get(), Some(1234));
        // A pathological 0 from the provider is clamped, never read as empty.
        let zero = CounterfactualSlot::new();
        zero.set(0);
        assert_eq!(zero.get(), Some(1));
    }

    #[test]
    fn slots_share_state_across_clones() {
        // The forward path clones the slot into WireContext; the probe task
        // writes through its own clone. Both must observe the same cell.
        let slot = CounterfactualSlot::new();
        let clone = slot.clone();
        clone.set(77);
        assert_eq!(slot.get(), Some(77));
    }
}
