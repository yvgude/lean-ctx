//! Pinned, reproducible model adapter (#234).
//!
//! The harness talks to a model through one small [`ModelRunner`] trait. Two real
//! implementations ship:
//!
//! * [`OpenAiRunner`] — a synchronous OpenAI-compatible chat client (OpenAI, Azure OpenAI,
//!   vLLM, llama.cpp, Ollama's `/v1` …). Decoding is pinned (`temperature = 0`, fixed `seed`)
//!   so a compliant provider is as deterministic as it can be.
//! * [`RecordedRunner`] — strict replay of responses previously captured from a real provider.
//!   Missing keys are a hard error (never a silent fallback), which is what makes CI runs and
//!   the determinism digest byte-stable across machines.
//!
//! [`RecordingRunner`] wraps any real runner and captures every response so an operator can
//! produce a replay file once with `eval ab --record`. Secrets (API keys) never enter a
//! fingerprint, recording, or report.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};

use super::sha256_hex;

/// Provider label for the OpenAI-compatible chat runner.
pub const PROVIDER_OPENAI: &str = "openai-compatible";
/// Provider label for the strict replay runner.
pub const PROVIDER_RECORDED: &str = "recorded";

/// Model identity and decoding parameters pinned for reproducibility. The lean-ctx
/// version is recorded on the report (`AbReport::lean_ctx_version`), outside every
/// digest, so a version bump never invalidates a committed recording.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelParams {
    /// Provider model identifier, e.g. `gpt-4o-mini` or `qwen2.5-coder:7b`.
    pub model: String,
    /// Provider-reported model revision or tag when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub temperature: f64,
    pub top_p: f64,
    pub max_tokens: u32,
    /// Best-effort decoding seed (forwarded to providers that honour it).
    pub seed: u64,
}

impl Default for ModelParams {
    fn default() -> Self {
        Self {
            model: String::new(),
            version: None,
            temperature: 0.0,
            top_p: 1.0,
            max_tokens: 1024,
            seed: 7,
        }
    }
}

/// Identifies exactly which model + params produced a set of answers. Embedded in the report
/// and the determinism digest so a third party knows precisely what was run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelFingerprint {
    /// `openai-compatible` or `recorded`.
    pub provider: String,
    /// Base URL or recording path — never contains credentials.
    pub endpoint: String,
    pub params: ModelParams,
}

impl ModelFingerprint {
    /// Stable hex digest of the fingerprint (canonical JSON over sorted keys via serde).
    pub fn digest(&self) -> String {
        let canonical = serde_json::to_vec(self).unwrap_or_default();
        sha256_hex(&canonical)
    }
}

/// A single chat request (one system + one user turn). Deliberately minimal so the recording
/// key is stable across runs.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRequest {
    pub system: String,
    pub user: String,
}

impl ModelRequest {
    /// Content-addressed replay key: hex SHA-256 over `system` and `user`.
    pub fn key(&self) -> String {
        let mut joined = Vec::with_capacity(self.system.len() + self.user.len() + 1);
        joined.extend_from_slice(self.system.as_bytes());
        joined.push(0);
        joined.extend_from_slice(self.user.as_bytes());
        sha256_hex(&joined)
    }
}

/// The model's answer plus a content digest for auditing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelResponse {
    pub text: String,
    /// What a real exchange with a provider looked like from here. `None` for every
    /// answer that did not come from one (synthetic runners, recordings made before
    /// this field existed) — and then not serialized, so those recordings keep their
    /// bytes. Never part of [`ModelResponse::digest`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchange: Option<ProviderExchange>,
}

/// Whether the metadata of an answer was seen in this run or read from a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationOrigin {
    /// The runner made the request itself, in this run.
    Observed,
    /// A recording was replayed: what an earlier run saw, not a fresh measurement,
    /// whatever provider the inherited fingerprint names.
    Replayed,
}

/// One request/response exchange as this process saw it.
///
/// The two digests are over the request body exactly as it was sent and the
/// response body exactly as it was read (no header, so no credential). They
/// bind a local observation of bytes: they are not an authorization by the
/// provider, not a signature, and not a receipt of this engine's tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderExchange {
    pub origin: ObservationOrigin,
    pub request_body_sha256: String,
    pub response_body_sha256: String,
    /// `None`: the provider reported no usage (absent or `null`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<ProviderUsage>,
}

/// Token counts as the provider REPORTED them in its answer — reported, not
/// signed, and never estimated here: a count the provider did not send stays
/// `None`, and a reported `0` is `Some(0)`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Input tokens the provider says it read from its cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    /// Known counters that WERE sent but are no count (a boolean, a fraction, a
    /// negative or too large number, a text): named here and left `None` above.
    /// A usage with an entry here is damaged evidence, not a measurement.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invalid: Vec<String>,
}

impl ProviderUsage {
    /// Whether every counter that was sent is a count.
    pub fn is_well_formed(&self) -> bool {
        self.invalid.is_empty()
    }

    /// A usage that is not even an object: invalid as a whole.
    fn not_an_object() -> Self {
        Self {
            invalid: vec!["usage".to_owned()],
            ..Self::default()
        }
    }
}

// The usage of an OpenAI-compatible answer is read key by key, as it was sent.
// A generic JSON value would keep only one of two equal keys, and then
// `{"prompt_tokens": true, "prompt_tokens": 7}` would look like a count of 7.

/// One known counter as it was sent.
#[derive(Clone, Copy)]
enum Counted {
    Null,
    Count(u64),
    /// Sent, and no count: a boolean, a fraction, a negative or too large
    /// number, a text, a list or an object.
    Invalid,
}

impl<'de> Deserialize<'de> for Counted {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Sent;
        impl<'de> Visitor<'de> for Sent {
            type Value = Counted;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a token count as the provider sent it")
            }
            fn visit_unit<E>(self) -> Result<Counted, E> {
                Ok(Counted::Null)
            }
            fn visit_u64<E>(self, count: u64) -> Result<Counted, E> {
                Ok(Counted::Count(count))
            }
            fn visit_i64<E>(self, count: i64) -> Result<Counted, E> {
                Ok(u64::try_from(count).map_or(Counted::Invalid, Counted::Count))
            }
            fn visit_f64<E>(self, _: f64) -> Result<Counted, E> {
                Ok(Counted::Invalid)
            }
            fn visit_bool<E>(self, _: bool) -> Result<Counted, E> {
                Ok(Counted::Invalid)
            }
            fn visit_str<E>(self, _: &str) -> Result<Counted, E> {
                Ok(Counted::Invalid)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Counted, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Counted::Invalid)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Counted, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Counted::Invalid)
            }
        }
        deserializer.deserialize_any(Sent)
    }
}

/// Everything that was sent under ONE known counter name.
#[derive(Default)]
struct Slot {
    times: u8,
    count: Option<u64>,
    invalid: bool,
}

impl Slot {
    fn see(&mut self, counted: Counted) {
        self.times = self.times.saturating_add(1);
        match counted {
            Counted::Null => {}
            Counted::Count(count) => self.count = Some(count),
            Counted::Invalid => self.invalid = true,
        }
    }

    /// A count only if the name was sent ONCE and as a count. Sent twice —
    /// whatever the two values, in whatever order, equal or not — it is named
    /// as invalid and is no count: neither the first nor the last one wins.
    fn settle(self, name: &'static str, invalid: &mut Vec<String>) -> Option<u64> {
        if self.invalid || self.times > 1 {
            invalid.push(name.to_owned());
            None
        } else {
            self.count
        }
    }
}

/// `prompt_tokens_details` as it was sent.
#[derive(Default)]
struct ReportedDetails {
    cached: Slot,
    not_an_object: bool,
}

impl<'de> Deserialize<'de> for ReportedDetails {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Sent;
        impl Sent {
            fn other<E>() -> Result<ReportedDetails, E> {
                Ok(ReportedDetails {
                    not_an_object: true,
                    ..ReportedDetails::default()
                })
            }
        }
        impl<'de> Visitor<'de> for Sent {
            type Value = ReportedDetails;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("the prompt token details as the provider sent them")
            }
            fn visit_unit<E>(self) -> Result<ReportedDetails, E> {
                Ok(ReportedDetails::default())
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ReportedDetails, A::Error> {
                let mut details = ReportedDetails::default();
                while let Some(key) = map.next_key::<String>()? {
                    if key == "cached_tokens" {
                        details.cached.see(map.next_value()?);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(details)
            }
            fn visit_u64<E>(self, _: u64) -> Result<ReportedDetails, E> {
                Self::other()
            }
            fn visit_i64<E>(self, _: i64) -> Result<ReportedDetails, E> {
                Self::other()
            }
            fn visit_f64<E>(self, _: f64) -> Result<ReportedDetails, E> {
                Self::other()
            }
            fn visit_bool<E>(self, _: bool) -> Result<ReportedDetails, E> {
                Self::other()
            }
            fn visit_str<E>(self, _: &str) -> Result<ReportedDetails, E> {
                Self::other()
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<ReportedDetails, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Self::other()
            }
        }
        deserializer.deserialize_any(Sent)
    }
}

/// The `usage` of an answer, typed: `None` for absent, `null`, or an object with
/// none of the known counters. Names a provider adds are ignored.
#[derive(Default)]
struct ReportedUsage(Option<ProviderUsage>);

impl<'de> Deserialize<'de> for ReportedUsage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Sent;
        impl Sent {
            fn other<E>() -> Result<ReportedUsage, E> {
                Ok(ReportedUsage(Some(ProviderUsage::not_an_object())))
            }
        }
        impl<'de> Visitor<'de> for Sent {
            type Value = ReportedUsage;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("the usage as the provider sent it")
            }
            fn visit_unit<E>(self) -> Result<ReportedUsage, E> {
                Ok(ReportedUsage(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ReportedUsage, A::Error> {
                let (mut input, mut output, mut total) =
                    (Slot::default(), Slot::default(), Slot::default());
                let mut details = ReportedDetails::default();
                let mut details_times = 0_u8;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "prompt_tokens" => input.see(map.next_value()?),
                        "completion_tokens" => output.see(map.next_value()?),
                        "total_tokens" => total.see(map.next_value()?),
                        "prompt_tokens_details" => {
                            details_times = details_times.saturating_add(1);
                            details = map.next_value()?;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                // Each name is settled once, so `invalid` holds every known name at
                // most once.
                let mut invalid = Vec::new();
                let input_tokens = input.settle("prompt_tokens", &mut invalid);
                let output_tokens = output.settle("completion_tokens", &mut invalid);
                let total_tokens = total.settle("total_tokens", &mut invalid);
                let cached_input_tokens = if details_times > 1 || details.not_an_object {
                    invalid.push("prompt_tokens_details".to_owned());
                    None
                } else {
                    details.cached.settle("cached_tokens", &mut invalid)
                };
                let usage = ProviderUsage {
                    input_tokens,
                    output_tokens,
                    total_tokens,
                    cached_input_tokens,
                    invalid,
                };
                Ok(ReportedUsage(
                    (usage != ProviderUsage::default()).then_some(usage),
                ))
            }
            fn visit_u64<E>(self, _: u64) -> Result<ReportedUsage, E> {
                Self::other()
            }
            fn visit_i64<E>(self, _: i64) -> Result<ReportedUsage, E> {
                Self::other()
            }
            fn visit_f64<E>(self, _: f64) -> Result<ReportedUsage, E> {
                Self::other()
            }
            fn visit_bool<E>(self, _: bool) -> Result<ReportedUsage, E> {
                Self::other()
            }
            fn visit_str<E>(self, _: &str) -> Result<ReportedUsage, E> {
                Self::other()
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<ReportedUsage, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Self::other()
            }
        }
        deserializer.deserialize_any(Sent)
    }
}

impl ModelResponse {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            exchange: None,
        }
    }

    /// Hex SHA-256 of the answer text (goes into the determinism digest).
    pub fn digest(&self) -> String {
        sha256_hex(self.text.as_bytes())
    }
}

/// Anything that can turn a request into a response under a fixed fingerprint.
pub trait ModelRunner {
    /// The pinned identity of this runner (model, params, provider).
    fn fingerprint(&self) -> &ModelFingerprint;
    /// Executes one request. Implementations must be deterministic given the same fingerprint.
    fn run(&self, req: &ModelRequest) -> Result<ModelResponse>;
}

// ---------------------------------------------------------------------------
// OpenAI-compatible runner (real HTTP)
// ---------------------------------------------------------------------------

const ENDPOINT_ADMISSION_ERROR: &str = "model endpoint is not admitted";

// Parse only for admission: accepted destinations keep their exact existing
// trailing-slash-trimmed bytes. This key list is not a generic secret scanner.
fn admitted_endpoint(base_url: &str) -> Option<String> {
    let endpoint = base_url.trim_end_matches('/');
    let parsed = reqwest::Url::parse(endpoint).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
        || parsed.query_pairs().any(|(key, _)| {
            [
                "api_key",
                "api-key",
                "apikey",
                "access_token",
                "token",
                "authorization",
                "password",
                "client_secret",
                "signature",
                "sig",
            ]
            .iter()
            .any(|blocked| key.eq_ignore_ascii_case(blocked))
        })
    {
        return None;
    }
    Some(endpoint.to_owned())
}

/// Synchronous OpenAI-compatible chat client. Credentials are held only in memory.
pub struct OpenAiRunner {
    fingerprint: ModelFingerprint,
    api_key: String,
    endpoint_admitted: bool,
}

impl OpenAiRunner {
    /// Builds a runner against `base_url` (e.g. `https://api.openai.com/v1`).
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        params: ModelParams,
    ) -> Self {
        let endpoint = admitted_endpoint(&base_url.into());
        let endpoint_admitted = endpoint.is_some();
        Self {
            fingerprint: ModelFingerprint {
                provider: PROVIDER_OPENAI.to_string(),
                endpoint: endpoint.unwrap_or_default(),
                params,
            },
            api_key: api_key.into(),
            endpoint_admitted,
        }
    }

    /// Builds a runner from the standard environment:
    /// `LEAN_CTX_EVAL_MODEL_URL`, `LEAN_CTX_EVAL_MODEL_KEY`, `LEAN_CTX_EVAL_MODEL`.
    pub fn from_env() -> Result<Self> {
        let base = std::env::var("LEAN_CTX_EVAL_MODEL_URL")
            .context("LEAN_CTX_EVAL_MODEL_URL not set (OpenAI-compatible base URL)")?;
        if admitted_endpoint(&base).is_none() {
            bail!(ENDPOINT_ADMISSION_ERROR);
        }
        let key = std::env::var("LEAN_CTX_EVAL_MODEL_KEY").unwrap_or_default();
        let model = std::env::var("LEAN_CTX_EVAL_MODEL")
            .context("LEAN_CTX_EVAL_MODEL not set (provider model id)")?;
        let seed = std::env::var("LEAN_CTX_EVAL_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(ModelParams::default().seed);
        let version = std::env::var("LEAN_CTX_EVAL_MODEL_VERSION")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| model.rsplit_once(':').map(|(_, tag)| tag.to_string()));
        let params = ModelParams {
            model,
            version,
            seed,
            ..ModelParams::default()
        };
        Ok(Self::new(base, key, params))
    }
}

#[derive(Serialize)]
struct ChatPayload<'a> {
    model: &'a str,
    temperature: f64,
    top_p: f64,
    max_tokens: u32,
    seed: u64,
    messages: Vec<ChatMessage<'a>>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    /// Read key by key ([`ReportedUsage`]): a counter of the wrong kind, or one
    /// sent twice, is named as invalid instead of failing the whole answer.
    #[serde(default)]
    usage: ReportedUsage,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

impl ModelRunner for OpenAiRunner {
    fn fingerprint(&self) -> &ModelFingerprint {
        &self.fingerprint
    }

    fn run(&self, req: &ModelRequest) -> Result<ModelResponse> {
        if !self.endpoint_admitted {
            bail!(ENDPOINT_ADMISSION_ERROR);
        }
        let p = &self.fingerprint.params;
        let payload = ChatPayload {
            model: &p.model,
            temperature: p.temperature,
            top_p: p.top_p,
            max_tokens: p.max_tokens,
            seed: p.seed,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: &req.system,
                },
                ChatMessage {
                    role: "user",
                    content: &req.user,
                },
            ],
        };
        let url = format!("{}/chat/completions", self.fingerprint.endpoint);
        let body = serde_json::to_vec(&payload).context("serialize chat payload")?;
        let mut request = ureq::post(&url).header("Content-Type", "application/json");
        if !self.api_key.is_empty() {
            request = request.header("Authorization", &format!("Bearer {}", self.api_key));
        }
        let resp = request
            .send(&body[..])
            .map_err(|_| anyhow!("model request failed"))?;
        let status = resp.status().as_u16();
        let text = resp
            .into_body()
            .read_to_string()
            .map_err(|e| anyhow!("reading model response failed: {e}"))?;
        if status != 200 {
            bail!("model endpoint returned HTTP {status}: {text}");
        }
        let parsed: ChatResponse = serde_json::from_str(&text)
            .with_context(|| format!("parsing model response: {text}"))?;
        let answer = parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| anyhow!("model response contained no choices"))?;
        Ok(ModelResponse {
            text: answer,
            // Of this very exchange: the bytes that were sent and read, and what
            // the provider said it counted. Nothing is requested again for it.
            exchange: Some(ProviderExchange {
                origin: ObservationOrigin::Observed,
                request_body_sha256: sha256_hex(&body),
                response_body_sha256: sha256_hex(text.as_bytes()),
                usage: parsed.usage.0,
            }),
        })
    }
}

// ---------------------------------------------------------------------------
// Recording + strict replay
// ---------------------------------------------------------------------------

/// A captured set of real responses, keyed by [`ModelRequest::key`]. Serialized as JSON with a
/// `BTreeMap` so the on-disk form is byte-stable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    /// `lean-ctx.eval-recording`.
    pub kind: String,
    /// The fingerprint that produced these responses (used to label the replay runner).
    pub fingerprint: ModelFingerprint,
    /// `request key -> response`.
    pub entries: BTreeMap<String, ModelResponse>,
}

const RECORDING_KIND: &str = "lean-ctx.eval-recording";

impl Recording {
    pub fn new(fingerprint: ModelFingerprint) -> Self {
        Self {
            kind: RECORDING_KIND.to_string(),
            fingerprint,
            entries: BTreeMap::new(),
        }
    }

    /// Loads + validates a recording file.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading recording {}", path.display()))?;
        let rec: Recording = serde_json::from_str(&raw)
            .with_context(|| format!("parsing recording {}", path.display()))?;
        if rec.kind != RECORDING_KIND {
            bail!("not a {RECORDING_KIND} file (kind = {:?})", rec.kind);
        }
        Ok(rec)
    }

    /// Writes the recording as pretty JSON (creating parent dirs).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let json = serde_json::to_string_pretty(self).context("serialize recording")?;
        std::fs::write(path, json)
            .with_context(|| format!("writing recording {}", path.display()))?;
        Ok(())
    }
}

/// Strict replay runner: every request must hit a recorded entry, or it errors. This guarantees
/// the run is fully deterministic and machine-independent.
pub struct RecordedRunner {
    recording: Recording,
}

impl RecordedRunner {
    pub fn new(recording: Recording) -> Self {
        Self { recording }
    }

    /// Loads a replay runner from a recording file.
    pub fn from_file(path: &Path) -> Result<Self> {
        Ok(Self::new(Recording::load(path)?))
    }
}

impl ModelRunner for RecordedRunner {
    fn fingerprint(&self) -> &ModelFingerprint {
        &self.recording.fingerprint
    }

    fn run(&self, req: &ModelRequest) -> Result<ModelResponse> {
        let mut response = self
            .recording
            .entries
            .get(&req.key())
            .cloned()
            .ok_or_else(|| anyhow!("no recorded response for request key {}", req.key()))?;
        // The answer and the inherited fingerprint are replayed unchanged. What an
        // earlier run saw of its exchange is handed on as exactly that: history.
        if let Some(exchange) = &mut response.exchange {
            exchange.origin = ObservationOrigin::Replayed;
        }
        Ok(response)
    }
}

/// Wraps a real runner and captures every response so it can be replayed later. Pass-through:
/// the wrapped runner's answers are returned unchanged.
pub struct RecordingRunner<R: ModelRunner> {
    inner: R,
    recording: RefCell<Recording>,
}

impl<R: ModelRunner> RecordingRunner<R> {
    pub fn new(inner: R) -> Self {
        let recording = Recording::new(inner.fingerprint().clone());
        Self {
            inner,
            recording: RefCell::new(recording),
        }
    }

    /// Consumes the wrapper and returns everything captured so far.
    pub fn into_recording(self) -> Recording {
        self.recording.into_inner()
    }
}

impl<R: ModelRunner> ModelRunner for RecordingRunner<R> {
    fn fingerprint(&self) -> &ModelFingerprint {
        self.inner.fingerprint()
    }

    fn run(&self, req: &ModelRequest) -> Result<ModelResponse> {
        let resp = self.inner.run(req)?;
        self.recording
            .borrow_mut()
            .entries
            .insert(req.key(), resp.clone());
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admitted_endpoints_preserve_fingerprint_bytes_and_benign_queries() {
        for endpoint in [
            "https://api.example.invalid/v1///",
            "http://localhost:11434/v1/",
            "https://EXAMPLE.invalid:443/v1?region=west&mode=fast",
            "https://example.invalid/v1?%72egion=west",
        ] {
            let params = ModelParams::default();
            let old_fingerprint = ModelFingerprint {
                provider: PROVIDER_OPENAI.to_owned(),
                endpoint: endpoint.trim_end_matches('/').to_owned(),
                params: params.clone(),
            };
            let runner = OpenAiRunner::new(endpoint, "", params);
            assert!(runner.endpoint_admitted);
            assert_eq!(runner.fingerprint(), &old_fingerprint);
            assert_eq!(runner.fingerprint().digest(), old_fingerprint.digest());
        }
    }

    #[test]
    fn credential_endpoint_keys_are_refused_after_percent_and_case_decoding() {
        for key in [
            "api_key",
            "api-key",
            "apikey",
            "access_token",
            "token",
            "authorization",
            "password",
            "client_secret",
            "signature",
            "sig",
            "%41PI%5FKEY",
            "%61pi-key",
            "AP%49KEY",
            "access%5Ftoken",
            "ToKeN",
            "AUTHORIZATION",
            "pass%77ord",
            "client%5Fsecret",
            "sign%61ture",
            "%73ig",
        ] {
            let endpoint = format!("https://example.invalid/v1?{key}=TEST_ENDPOINT_MARKER");
            assert!(admitted_endpoint(&endpoint).is_none());
        }
    }

    #[test]
    fn refused_endpoints_fail_before_request_and_record_no_rejected_url() {
        let request = ModelRequest {
            system: "test".into(),
            user: "test".into(),
        };
        for endpoint in [
            "https://TEST_ENDPOINT_MARKER@example.invalid/v1",
            "https://:TEST_ENDPOINT_MARKER@example.invalid/v1",
            "https://example.invalid/v1#TEST_ENDPOINT_MARKER",
            "https://example.invalid/v1#",
            "https://example.invalid/v1?token=TEST_ENDPOINT_MARKER",
            "file:///TEST_ENDPOINT_MARKER",
            "TEST_ENDPOINT_MARKER",
        ] {
            let runner = OpenAiRunner::new(endpoint, "", ModelParams::default());
            assert!(!runner.endpoint_admitted);
            assert!(runner.fingerprint().endpoint.is_empty());
            let recorder = RecordingRunner::new(runner);
            let error = recorder.run(&request).unwrap_err();
            assert_eq!(error.to_string(), ENDPOINT_ADMISSION_ERROR);
            assert_eq!(error.chain().count(), 1);
            let recording = recorder.into_recording();
            assert!(recording.fingerprint.endpoint.is_empty());
            assert!(recording.entries.is_empty());
            let serialized = serde_json::to_string(&recording).unwrap();
            assert!(!serialized.contains("TEST_ENDPOINT_MARKER"));
        }
    }

    fn fp() -> ModelFingerprint {
        ModelFingerprint {
            provider: PROVIDER_RECORDED.to_string(),
            endpoint: "rec".into(),
            params: ModelParams {
                model: "test-model".into(),
                ..ModelParams::default()
            },
        }
    }

    #[test]
    fn request_key_is_stable_and_order_sensitive() {
        let a = ModelRequest {
            system: "s".into(),
            user: "u".into(),
        };
        let b = ModelRequest {
            system: "s".into(),
            user: "u".into(),
        };
        assert_eq!(a.key(), b.key());
        let swapped = ModelRequest {
            system: "u".into(),
            user: "s".into(),
        };
        assert_ne!(a.key(), swapped.key());
    }

    #[test]
    fn fingerprint_digest_changes_with_params() {
        let mut f1 = fp();
        let d1 = f1.digest();
        f1.params.seed = 99;
        assert_ne!(d1, f1.digest());
        f1.params.seed = ModelParams::default().seed;
        f1.params.version = Some("revision-2".into());
        assert_ne!(d1, f1.digest());
    }

    #[test]
    fn legacy_recording_without_model_version_still_loads() {
        let legacy = r#"{"kind":"lean-ctx.eval-recording","fingerprint":{"provider":"openai-compatible","endpoint":"http://localhost:11434/v1","params":{"model":"gemma4:e4b","temperature":0.0,"top_p":1.0,"max_tokens":1024,"seed":7}},"entries":{}}"#;
        let recording: Recording = serde_json::from_str(legacy).unwrap();
        assert_eq!(recording.fingerprint.params.version, None);
        // Committed recordings must re-serialize byte-identically, or the
        // `gen_testbench_recording --check` gate breaks on every release.
        assert_eq!(serde_json::to_string(&recording).unwrap(), legacy);
    }

    #[test]
    fn recorded_runner_replays_and_errors_on_miss() {
        let mut rec = Recording::new(fp());
        let req = ModelRequest {
            system: "sys".into(),
            user: "hi".into(),
        };
        rec.entries.insert(req.key(), ModelResponse::new("hello"));
        let runner = RecordedRunner::new(rec);
        assert_eq!(runner.run(&req).unwrap().text, "hello");

        let miss = ModelRequest {
            system: "sys".into(),
            user: "missing".into(),
        };
        assert!(runner.run(&miss).is_err(), "unknown key must hard-error");
    }

    #[test]
    fn provider_usage_is_only_what_was_reported_and_a_replay_says_so() {
        // The raw JSON as a provider would send it, read the way an answer is read.
        let usage = |raw: &str| serde_json::from_str::<ReportedUsage>(raw).unwrap().0;
        // Absent, null and an object without a known counter are all "not reported".
        for raw in [
            "null",
            "{}",
            r#"{"prompt_tokens":null,"prompt_tokens_details":null}"#,
            r#"{"vendor_extension":{"anything":[1,true]}}"#,
        ] {
            assert_eq!(usage(raw), None, "{raw}");
        }
        // A reported zero is a count; a counter that was not sent stays unknown; names
        // a provider adds are ignored.
        let reported = usage(
            r#"{"prompt_tokens":12,"vendor":[1],"completion_tokens":0,"prompt_tokens_details":{"audio_tokens":0,"cached_tokens":4}}"#,
        )
        .unwrap();
        assert_eq!(
            (
                reported.input_tokens,
                reported.output_tokens,
                reported.total_tokens,
                reported.cached_input_tokens
            ),
            (Some(12), Some(0), None, Some(4))
        );
        assert!(reported.is_well_formed());
        // No count: named, never a number.
        for (raw, name) in [
            (r#"{"prompt_tokens":true}"#, "prompt_tokens"),
            (r#"{"prompt_tokens":1.0}"#, "prompt_tokens"),
            (r#"{"completion_tokens":-1}"#, "completion_tokens"),
            (r#"{"total_tokens":18446744073709551616}"#, "total_tokens"),
            (r#"{"total_tokens":"7"}"#, "total_tokens"),
            (r#"{"prompt_tokens_details":7}"#, "prompt_tokens_details"),
            (r#""none""#, "usage"),
            // A known name sent twice is no count, whichever value comes first and
            // even when both are the same count.
            (
                r#"{"prompt_tokens":true,"prompt_tokens":7}"#,
                "prompt_tokens",
            ),
            (
                r#"{"prompt_tokens":7,"prompt_tokens":true}"#,
                "prompt_tokens",
            ),
            (r#"{"prompt_tokens":7,"prompt_tokens":7}"#, "prompt_tokens"),
            (
                r#"{"prompt_tokens_details":{"cached_tokens":1,"cached_tokens":1}}"#,
                "cached_tokens",
            ),
            (
                r#"{"prompt_tokens_details":{"cached_tokens":1},"prompt_tokens_details":{"cached_tokens":1}}"#,
                "prompt_tokens_details",
            ),
        ] {
            let damaged = usage(raw).unwrap();
            assert_eq!(damaged.invalid, [name], "{raw}");
            assert!(!damaged.is_well_formed());
            assert_eq!(
                (
                    damaged.input_tokens,
                    damaged.output_tokens,
                    damaged.total_tokens
                ),
                (None, None, None),
                "{raw}"
            );
            assert_eq!(damaged.cached_input_tokens, None, "{raw}");
        }

        // A recorded exchange comes back as history; an answer without one is
        // unchanged and serializes as it always did.
        let req = ModelRequest {
            system: "sys".into(),
            user: "hi".into(),
        };
        let mut rec = Recording::new(fp());
        let mut seen = ModelResponse::new("hello");
        seen.exchange = Some(ProviderExchange {
            origin: ObservationOrigin::Observed,
            request_body_sha256: "a".repeat(64),
            response_body_sha256: "b".repeat(64),
            usage: Some(reported),
        });
        rec.entries.insert(req.key(), seen);
        let replayed = RecordedRunner::new(rec)
            .run(&req)
            .unwrap()
            .exchange
            .unwrap();
        assert_eq!(replayed.origin, ObservationOrigin::Replayed);
        assert_eq!(replayed.usage.unwrap().input_tokens, Some(12));
        assert_eq!(
            serde_json::to_string(&ModelResponse::new("hello")).unwrap(),
            r#"{"text":"hello"}"#
        );
    }

    #[test]
    fn recording_runner_captures_then_replays() {
        let mut seed = Recording::new(fp());
        let req = ModelRequest {
            system: "sys".into(),
            user: "q".into(),
        };
        seed.entries.insert(req.key(), ModelResponse::new("answer"));
        let recorder = RecordingRunner::new(RecordedRunner::new(seed));
        let _ = recorder.run(&req).unwrap();
        let captured = recorder.into_recording();
        assert_eq!(captured.entries.get(&req.key()).unwrap().text, "answer");
    }
}
