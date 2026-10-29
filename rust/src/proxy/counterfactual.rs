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

// The `http` crate's type (what axum re-exports), so the slot builds without
// the `http-server` feature (#586).
#[cfg(feature = "http-server")]
use http::request::Parts;
#[cfg(feature = "http-server")]
use serde_json::Value;

#[cfg(feature = "http-server")]
use super::admitted::{self, Admission, AdmittedBody, Payload};
#[cfg(feature = "http-server")]
use crate::core::context_admission::egress::{EgressTarget, TrustedDestinationBinding};

/// Probe timeout: `count_tokens` typically answers in well under a second; a
/// probe slower than the model's own response is useless (the row degrades to
/// the estimate), so give up early and free the connection.
#[cfg(feature = "http-server")]
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
#[cfg(feature = "http-server")]
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
#[cfg(feature = "http-server")]
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
#[cfg(feature = "http-server")]
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
#[cfg(feature = "http-server")]
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
    // The probe is a separate egress. Inspect the final count_tokens shape,
    // including its explicit original model, and retain its own receipt gate.
    let original_body = probe_body(original?, original_model)?;
    let probe_doc: Value = serde_json::from_slice(&original_body).ok()?;
    let target = EgressTarget {
        provider: "Anthropic",
        model: probe_doc.get("model").and_then(Value::as_str),
        upstream_base,
    };
    let mut admission = admitted::admit(
        Payload::Document(&probe_doc),
        &target,
        TrustedDestinationBinding::default(),
    )
    .ok()?;
    if admission.is_refused() {
        return None;
    }
    let original_size = original_body.len();
    let body = match admission.take_rewritten() {
        Some(admitted_doc) => serde_json::to_vec(&admitted_doc).ok()?,
        None => original_body,
    };
    let probe = Probe {
        client: client.clone(),
        upstream_base: upstream_base.to_owned(),
        headers: PROBE_HEADERS
            .iter()
            .filter_map(|&name| Some((name, parts.headers.get(name)?.clone())))
            .collect(),
        body,
        original_size,
    };

    let slot = CounterfactualSlot::new();
    let task_slot = slot.clone();
    tokio::spawn(async move {
        dispatch_probe(probe, admission, task_slot).await;
    });
    Some(slot)
}

/// Everything the detached probe needs besides its admission.
#[cfg(feature = "http-server")]
struct Probe {
    client: reqwest::Client,
    upstream_base: String,
    headers: Vec<(&'static str, axum::http::HeaderValue)>,
    body: Vec<u8>,
    original_size: usize,
}

/// The probe leaves through the proxy's one send path: bound to its receipt
/// and to the admitted upstream, or not sent at all.
#[cfg(feature = "http-server")]
async fn dispatch_probe(probe: Probe, admission: Admission, task_slot: CounterfactualSlot) {
    let url = format!(
        "{}/v1/messages/count_tokens",
        probe.upstream_base.trim_end_matches('/')
    );
    let head = crate::proxy::admitted::RequestHead::new(
        reqwest::Method::POST,
        std::iter::once((
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        ))
        .chain(
            probe
                .headers
                .into_iter()
                .map(|(name, value)| (axum::http::HeaderName::from_static(name), value)),
        ),
    );
    let Ok(admitted_probe) = AdmittedBody::bind(
        admission,
        probe.body,
        &probe.upstream_base,
        &url,
        head,
        probe.original_size,
        None,
    )
    .await
    else {
        return;
    };
    let attempt = admitted_probe
        .send(&probe.client, 1, |request| {
            request.timeout(std::time::Duration::from_secs(PROBE_TIMEOUT_SECS))
        })
        .await;
    let Ok(attempt) = attempt else {
        return;
    };
    match attempt.response {
        Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
            Ok(v) => {
                if let Some(tokens) = v.get("input_tokens").and_then(Value::as_u64) {
                    task_slot.set(tokens);
                } else {
                    tracing::debug!("counterfactual probe: response without input_tokens");
                }
            }
            Err(e) => tracing::debug!(
                "counterfactual probe: unreadable response: {}",
                admitted::transport_failure(&e)
            ),
        },
        Ok(resp) => tracing::debug!(
            "counterfactual probe: count_tokens returned {}",
            resp.status()
        ),
        Err(e) => tracing::debug!("counterfactual probe: {}", admitted::transport_failure(&e)),
    }
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

    #[tokio::test(flavor = "current_thread")]
    async fn mandatory_receipt_failure_prevents_counterfactual_network_request() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let config = crate::core::paths::config_dir_read_only().expect("config directory");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("governed config");
        let data = crate::core::data_dir::lean_ctx_data_dir().expect("data directory");
        std::fs::create_dir_all(&data).expect("data directory");
        std::fs::write(data.join("gateway"), b"blocked test store").expect("store obstruction");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let upstream = format!("http://{}", listener.local_addr().expect("address"));
        let body = probe_body(&full_request(), None).expect("probe bytes");
        let document: Value = serde_json::from_slice(&body).expect("probe JSON");
        let admission = admitted::admit(
            Payload::Document(&document),
            &EgressTarget {
                provider: "Anthropic",
                model: document.get("model").and_then(Value::as_str),
                upstream_base: &upstream,
            },
            TrustedDestinationBinding::default(),
        )
        .expect("probe admission");
        let probe = Probe {
            client: reqwest::Client::new(),
            upstream_base: upstream.clone(),
            headers: Vec::new(),
            original_size: body.len(),
            body,
        };
        let slot = CounterfactualSlot::new();
        dispatch_probe(probe, admission, slot.clone()).await;
        assert!(slot.get().is_none());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
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
