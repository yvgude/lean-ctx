// SPDX-License-Identifier: Apache-2.0
//! Whether this engine may use LeanCTX | Hive: the release switch, the preview
//! switch for a loopback Hive, and the lines printed while Hive is off.
//! The engine reads it to keep Hive tools and commands out of every list;
//! the Hive connector re-exports it.

/// Release switch (owner, 3.12.0): the connector code ships, but `hive
/// connect` and `hive link` refuse and `hive` stays out of help, suggestions
/// and completions. 3.12.1 sets this to `true`; nothing else changes.
pub const HIVE_AVAILABLE: bool = false;
/// Test switch: `LEAN_CTX_HIVE_PREVIEW=1` opens the connector before the
/// release, for a loopback Hive only (the FakeHive of the test suite).
pub const PREVIEW_ENV: &str = "LEAN_CTX_HIVE_PREVIEW";
/// The one line `hive connect`/`hive link` print while the switch is off.
pub const NOT_AVAILABLE: &str =
    "LeanCTX | Hive comes with 3.12.1; this version does not connect to a Hive.";
/// The one line a call to a Hive MCP tool (`ctx_tasks`)
/// answers while the switch is off; the tools stay out of every tool list.
pub const TOOL_NOT_AVAILABLE: &str =
    "LeanCTX | Hive comes with 3.12.1; this tool is not available in this version.";
/// Hive MCP tools. They ship with the connector but follow its release switch
/// ([`available_locally`], owner 28.10.): while it is off they are in no tool
/// list and every call answers [`TOOL_NOT_AVAILABLE`].
pub const HIVE_TOOLS: &[&str] = &["ctx_tasks"];

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

/// Whether this engine may connect to the Hive at `origin`.
pub fn available_for(origin: &str) -> bool {
    HIVE_AVAILABLE || (preview() && is_loopback(origin))
}

/// Whether Hive commands that name no origin (`hive link`) may run.
pub fn available_locally() -> bool {
    HIVE_AVAILABLE || preview()
}
