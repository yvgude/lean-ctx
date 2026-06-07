//! Error types for the lean-ctx client.
//!
//! The public surface never leaks the underlying HTTP backend: a transport
//! failure is reduced to a stable [`LeanCtxError`] so the HTTP implementation
//! can change without breaking embedders.

use serde_json::Value;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, LeanCtxError>;

/// Details of a non-2xx HTTP response (boxed inside [`LeanCtxError::Http`] to
/// keep the error enum small).
///
/// Branch on [`HttpError::error_code`] for stable, machine-readable handling;
/// `message` is for logs/humans only (per the HTTP-MCP contract).
#[derive(Debug, Clone)]
pub struct HttpError {
    /// HTTP status code (e.g. `401`, `404`, `429`).
    pub status: u16,
    /// HTTP method of the failed request.
    pub method: String,
    /// Fully-qualified request URL.
    pub url: String,
    /// Human-readable message (the envelope `error`, else a generic line).
    pub message: String,
    /// Stable machine code (the envelope `error_code`) clients switch on.
    pub error_code: Option<String>,
    /// Raw parsed response body, when available.
    pub body: Option<Value>,
}

/// Every failure mode of a client call.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LeanCtxError {
    /// The server returned a non-2xx status with the contract error envelope.
    #[error("HTTP {} {} {}: {}", .0.status, .0.method, .0.url, .0.message)]
    Http(Box<HttpError>),

    /// The request never produced an HTTP response (DNS, connect, TLS, I/O).
    #[error("transport error for {method} {url}: {message}")]
    Transport {
        /// HTTP method of the failed request.
        method: String,
        /// Fully-qualified request URL.
        url: String,
        /// Backend-provided description of the transport failure.
        message: String,
    },

    /// The response was received but its body could not be decoded as expected.
    #[error("decode error for {method} {url}: {message}")]
    Decode {
        /// HTTP method of the request.
        method: String,
        /// Fully-qualified request URL.
        url: String,
        /// Description of the decode failure.
        message: String,
    },

    /// The call was rejected locally before hitting the network.
    #[error("invalid request: {0}")]
    Config(String),
}

impl LeanCtxError {
    /// Build a boxed [`LeanCtxError::Http`] from its details.
    #[must_use]
    pub(crate) fn http(details: HttpError) -> Self {
        Self::Http(Box::new(details))
    }

    /// The stable machine code, if this is an [`LeanCtxError::Http`] carrying one.
    #[must_use]
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::Http(e) => e.error_code.as_deref(),
            _ => None,
        }
    }

    /// The HTTP status code, if this failure originated from an HTTP response.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http(e) => Some(e.status),
            _ => None,
        }
    }
}
