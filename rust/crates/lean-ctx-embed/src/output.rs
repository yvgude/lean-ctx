//! The SDK's stable output type, returned by engine tool calls.
//!
//! Mirrors the engine's internal tool output but is owned by the façade (and
//! derives `Debug`/`Clone`) so embedders never depend on engine internals.

use lean_ctx::server::tool_trait::ToolOutput;

/// The result of an [`crate::Engine`] tool call.
#[derive(Debug, Clone)]
pub struct Output {
    /// Rendered text the embedder feeds to the model.
    pub text: String,
    /// Tokens the raw (uncompressed, uncached) result would have cost.
    pub original_tokens: usize,
    /// Tokens saved versus that raw result (compression + cache delta).
    pub saved_tokens: usize,
    /// The mode the engine actually used, when applicable (e.g. `ctx_read`).
    pub mode: Option<String>,
}

impl Output {
    /// Percentage of tokens saved versus the raw result, clamped to `0.0..=100.0`.
    #[must_use]
    pub fn saved_pct(&self) -> f64 {
        if self.original_tokens == 0 {
            return 0.0;
        }
        let saved = self.saved_tokens.min(self.original_tokens);
        (saved as f64 / self.original_tokens as f64) * 100.0
    }
}

impl From<ToolOutput> for Output {
    fn from(o: ToolOutput) -> Self {
        Self {
            text: o.text,
            original_tokens: o.original_tokens,
            saved_tokens: o.saved_tokens,
            mode: o.mode,
        }
    }
}
