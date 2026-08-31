//! The SDK's own error type.
//!
//! Engine handlers speak `rmcp::ErrorData` internally; the façade collapses that
//! into a small, stable enum so embedders never depend on the protocol crate.

use std::fmt;

/// Errors returned by [`crate::Engine`] operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The requested tool is not registered in the engine.
    #[error("unknown tool: {0}")]
    UnknownTool(String),

    /// A path argument escaped the project root (`PathJail`), pointed at a secret,
    /// or could not be resolved.
    #[error("path rejected: {0}")]
    Path(String),

    /// The engine could not be constructed (e.g. the project root does not
    /// exist or is not a directory).
    #[error("engine init failed: {0}")]
    Init(String),

    /// The tool ran but returned an error (invalid params, internal failure).
    #[error("tool '{tool}' failed: {message}")]
    Tool { tool: String, message: String },

    /// A write/exec tool was invoked without the corresponding opt-in on the
    /// [`crate::EngineBuilder`]. Read-mostly is the default.
    #[error("'{0}' requires opt-in: enable it on the EngineBuilder (allow_write/allow_exec)")]
    NotPermitted(String),

    /// The handler panicked or was abandoned by its watchdog; the engine is
    /// still usable.
    #[error("tool '{0}' did not complete (isolated panic or timeout)")]
    Incomplete(String),
}

impl Error {
    /// Build a [`Error::Tool`] from any displayable message.
    pub(crate) fn tool(name: &str, message: impl fmt::Display) -> Self {
        Self::Tool {
            tool: name.to_string(),
            message: message.to_string(),
        }
    }
}
