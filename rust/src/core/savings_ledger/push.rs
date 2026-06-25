//! Reusable savings push: sign this machine's whole ledger and POST it to a team
//! server's ingest endpoint.
//!
//! Shared by the `lean-ctx savings push` CLI and the opt-in daemon auto-push
//! ([`crate::core::savings_autopush`]) so there is exactly **one** push path.
//! The batch is a cumulative whole-ledger snapshot (`period = "all"`), so
//! re-pushing is idempotent on the server (the summary takes each signer's
//! latest batch). It carries only counts, model names, tool names and chain
//! hashes — never prompts or code.

use super::SignedSavingsBatchV1;

/// Outcome of a successful push (for human-readable reporting).
#[derive(Debug, Clone, Copy)]
pub struct PushOutcome {
    pub net_saved_tokens: u64,
    pub saved_usd: f64,
}

/// Why a push could not be completed.
#[derive(Debug)]
pub enum PushError {
    /// The local ledger has no events yet — nothing to report.
    Empty,
    /// Signing the batch failed.
    Sign(String),
    /// The batch could not be serialized.
    Serialize(String),
    /// The server rejected the bearer token (HTTP 401/403).
    Unauthorized,
    /// The server returned a non-2xx status with a body.
    Rejected { status: u16, body: String },
    /// The server could not be reached.
    Unreachable(String),
}

impl std::fmt::Display for PushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "Savings ledger is empty — nothing to push."),
            Self::Sign(e) => write!(f, "Signing failed: {e}"),
            Self::Serialize(e) => write!(f, "Serialization failed: {e}"),
            Self::Unauthorized => write!(f, "Team server denied the push (HTTP 401/403)."),
            Self::Rejected { status, body } => {
                write!(f, "Team server rejected the batch (HTTP {status}): {body}")
            }
            Self::Unreachable(e) => write!(f, "Failed to reach team server: {e}"),
        }
    }
}

impl std::error::Error for PushError {}

/// Resolve the signing identity (same precedence as the ledger's attribution).
#[must_use]
pub fn agent_id() -> String {
    std::env::var("LEAN_CTX_AGENT_ID")
        .or_else(|_| std::env::var("LCTX_AGENT_ID"))
        .unwrap_or_else(|_| "local".to_string())
}

/// The ingest endpoint for a team server base URL.
#[must_use]
pub fn ingest_endpoint(url: &str) -> String {
    format!("{}/api/v1/savings/ingest", url.trim_end_matches('/'))
}

/// Build + sign the whole local ledger and POST it to `{url}/api/v1/savings/ingest`.
///
/// `token` is the team bearer token. Real servers gate ingest behind a valid
/// token, so `None` only succeeds against an unauthenticated/dev server.
pub fn push_batch(url: &str, token: Option<&str>) -> Result<PushOutcome, PushError> {
    let agent = agent_id();
    let mut batch = SignedSavingsBatchV1::build_all(&agent);
    if batch.totals.total_events == 0 {
        return Err(PushError::Empty);
    }
    batch.sign(&agent).map_err(PushError::Sign)?;

    let endpoint = ingest_endpoint(url);
    let body = serde_json::to_vec(&batch).map_err(|e| PushError::Serialize(e.to_string()))?;

    let mut request = ureq::post(&endpoint).header("Content-Type", "application/json");
    if let Some(tok) = token {
        request = request.header("Authorization", &format!("Bearer {tok}"));
    }

    match request.send(&body[..]) {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status == 401 || status == 403 {
                return Err(PushError::Unauthorized);
            }
            if status == 200 {
                Ok(PushOutcome {
                    net_saved_tokens: batch.totals.net_saved_tokens,
                    saved_usd: batch.totals.saved_usd,
                })
            } else {
                let body = resp.into_body().read_to_string().unwrap_or_default();
                Err(PushError::Rejected { status, body })
            }
        }
        Err(e) => Err(PushError::Unreachable(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_endpoint_trims_trailing_slash() {
        assert_eq!(
            ingest_endpoint("https://team.example.com/"),
            "https://team.example.com/api/v1/savings/ingest"
        );
        assert_eq!(
            ingest_endpoint("https://team.example.com"),
            "https://team.example.com/api/v1/savings/ingest"
        );
    }

    #[test]
    fn push_error_display_is_actionable() {
        assert!(PushError::Empty.to_string().contains("empty"));
        assert!(PushError::Unauthorized.to_string().contains("401/403"));
        assert!(
            PushError::Rejected {
                status: 500,
                body: "boom".into()
            }
            .to_string()
            .contains("500")
        );
    }
}
