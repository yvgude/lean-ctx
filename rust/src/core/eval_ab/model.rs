//! Pinned, reproducible model adapter (#234).
//!
//! The harness talks to a model through one small [`ModelRunner`] trait. Two real
//! implementations ship:
//!
//! * [`OpenAiRunner`] — a synchronous OpenAI-compatible chat client (`OpenAI`, Azure `OpenAI`,
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
use serde::{Deserialize, Serialize};

use super::sha256_hex;

/// Provider label for the OpenAI-compatible chat runner.
pub const PROVIDER_OPENAI: &str = "openai-compatible";
/// Provider label for the strict replay runner.
pub const PROVIDER_RECORDED: &str = "recorded";

/// Decoding parameters pinned for reproducibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelParams {
    /// Provider model identifier, e.g. `gpt-4o-mini` or `qwen2.5-coder:7b`.
    pub model: String,
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
    #[must_use]
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
    #[must_use]
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
}

impl ModelResponse {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// Hex SHA-256 of the answer text (goes into the determinism digest).
    #[must_use]
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

/// Synchronous OpenAI-compatible chat client. Credentials are held only in memory.
pub struct OpenAiRunner {
    fingerprint: ModelFingerprint,
    api_key: String,
}

impl OpenAiRunner {
    /// Builds a runner against `base_url` (e.g. `https://api.openai.com/v1`).
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        params: ModelParams,
    ) -> Self {
        let endpoint = base_url.into().trim_end_matches('/').to_string();
        Self {
            fingerprint: ModelFingerprint {
                provider: PROVIDER_OPENAI.to_string(),
                endpoint,
                params,
            },
            api_key: api_key.into(),
        }
    }

    /// Builds a runner from the standard environment:
    /// `LEAN_CTX_EVAL_MODEL_URL`, `LEAN_CTX_EVAL_MODEL_KEY`, `LEAN_CTX_EVAL_MODEL`.
    pub fn from_env() -> Result<Self> {
        let base = std::env::var("LEAN_CTX_EVAL_MODEL_URL")
            .context("LEAN_CTX_EVAL_MODEL_URL not set (OpenAI-compatible base URL)")?;
        let key = std::env::var("LEAN_CTX_EVAL_MODEL_KEY").unwrap_or_default();
        let model = std::env::var("LEAN_CTX_EVAL_MODEL")
            .context("LEAN_CTX_EVAL_MODEL not set (provider model id)")?;
        let seed = std::env::var("LEAN_CTX_EVAL_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(ModelParams::default().seed);
        let params = ModelParams {
            model,
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
            .map_err(|e| anyhow!("model request to {url} failed: {e}"))?;
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
        Ok(ModelResponse::new(answer))
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
    #[must_use]
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
    #[must_use]
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
        self.recording
            .entries
            .get(&req.key())
            .cloned()
            .ok_or_else(|| anyhow!("no recorded response for request key {}", req.key()))
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
