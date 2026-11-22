// SPDX-License-Identifier: Apache-2.0
//! Whether this engine may use LeanCTX | Cloud: the release switch, the preview
//! switch for a loopback Cloud, and the lines printed while Cloud is off.
//! The engine reads it to keep Cloud tools and commands out of every list;
//! the Cloud connector re-exports it.

/// Release switch: the connector shipped switched off in 3.12.0 and 3.12.1
/// (`cloud connect`/`cloud link` refused, `cloud` hidden from help, suggestions
/// and completions). Owner 2026-11-04: LeanCTX | Cloud opens with 3.12.2.
pub const CLOUD_AVAILABLE: bool = true;
/// Test switch: `LEAN_CTX_CLOUD_PREVIEW=1` opens the connector before the
/// release, for a loopback Cloud only (the FakeCloud of the test suite).
pub const PREVIEW_ENV: &str = "LEAN_CTX_CLOUD_PREVIEW";
/// The one line `cloud connect`/`cloud link` print while the switch is off.
pub const NOT_AVAILABLE: &str =
    "LeanCTX | Cloud comes with 3.12.2; this version does not connect to a Cloud.";
/// The one line a call to a Cloud MCP tool (`ctx_tasks`)
/// answers while the switch is off; the tools stay out of every tool list.
pub const TOOL_NOT_AVAILABLE: &str =
    "LeanCTX | Cloud comes with 3.12.2; this tool is not available in this version.";
/// Cloud MCP tools. They ship with the connector but follow its release switch
/// ([`available_locally`], owner 28.10.): while it is off they are in no tool
/// list and every call answers [`TOOL_NOT_AVAILABLE`].
pub const CLOUD_TOOLS: &[&str] = &["ctx_tasks"];

fn preview() -> bool {
    std::env::var(PREVIEW_ENV).as_deref() == Ok("1")
}

/// Whether `origin` names this machine (`127.0.0.1`, `localhost`, `::1`).
pub fn is_loopback(origin: &str) -> bool {
    let rest = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .unwrap_or(origin);
    let host = if let Some(v6) = rest.strip_prefix('[') {
        v6.split(']').next().unwrap_or_default()
    } else {
        rest.split(['/', ':']).next().unwrap_or_default()
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// Whether this engine may connect to the Cloud at `origin`.
pub fn available_for(origin: &str) -> bool {
    CLOUD_AVAILABLE || (preview() && is_loopback(origin))
}

/// Whether Cloud commands that name no origin (`cloud link`) may run.
pub fn available_locally() -> bool {
    CLOUD_AVAILABLE || preview()
}
