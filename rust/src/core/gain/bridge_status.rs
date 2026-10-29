//! Bridge engagement diagnostics for the `gain` command.
//!
//! `gain` derives `tokens_saved` from the proxy's request statistics. When the
//! lean-ctx bridge (proxy + MCP server) is not engaged those stats stay empty
//! and `gain` silently reports `0`. Users and external benchmarks (GitHub #361,
//! #271) cannot then tell "bridge off" apart from "bridge on, genuinely 0
//! savings". This module observes the engagement state so `gain` can emit an
//! honest diagnostic instead of a silent zero.

use serde::Serialize;

/// How far back a persisted proxy-introspect record still counts as "the bridge
/// has intercepted traffic". 24h keeps a running-but-idle proxy classified as
/// engaged across a normal working day.
const INTROSPECT_MAX_AGE_SECS: u64 = 86_400;

/// Observable engagement state of the lean-ctx bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeEngagement {
    /// Proxy process is not reachable — savings cannot be measured at all.
    ProxyDown,
    /// Proxy is up but has not intercepted any LLM request (fresh start, or the
    /// editor is not routed through the bridge). Savings are unmeasured.
    NoRequests,
    /// Proxy is up and has intercepted requests. A reported `0` saved is real.
    Engaged,
}

/// Snapshot of the bridge state used to explain `gain`'s savings numbers.
#[derive(Debug, Clone, Serialize)]
pub struct BridgeStatus {
    pub engagement: BridgeEngagement,
    pub proxy_running: bool,
    /// Whether the proxy is enabled in config at all (`proxy_enabled = true`).
    pub proxy_configured: bool,
    /// Cumulative LLM requests the proxy has intercepted (within the freshness
    /// window). `0` means the bridge has not seen compressible traffic.
    pub total_requests: u64,
    /// Number of MCP tools the registry exposes — the `/lean-ctx` tool-count.
    pub tool_count: usize,
}

/// Pure classification of the engagement state from the two raw signals.
///
/// Kept separate from [`BridgeStatus::detect`] so the decision logic is unit
/// testable without touching the network or the filesystem.
#[must_use]
pub fn classify(proxy_running: bool, total_requests: u64) -> BridgeEngagement {
    match (proxy_running, total_requests > 0) {
        (false, _) => BridgeEngagement::ProxyDown,
        (true, false) => BridgeEngagement::NoRequests,
        (true, true) => BridgeEngagement::Engaged,
    }
}

impl BridgeStatus {
    /// Check the configured proxy endpoint and persisted introspection data to
    /// determine whether the bridge is engaged. Uses the same source of truth as
    /// `doctor`: the proxy must be enabled in config and answer as lean-ctx (a
    /// foreign process on the port does not count). Never panics: every signal
    /// degrades to a conservative default (proxy down / 0 requests) on error.
    #[must_use]
    pub fn detect() -> Self {
        let proxy_configured = crate::core::config::Config::load().proxy_enabled == Some(true);
        let proxy_running = crate::proxy_setup::configured_proxy_live();
        let total_requests = persisted_request_count(INTROSPECT_MAX_AGE_SECS);
        let tool_count = crate::server::registry::tool_count();
        let engagement = classify(proxy_running, total_requests);
        Self {
            engagement,
            proxy_running,
            proxy_configured,
            total_requests,
            tool_count,
        }
    }

    /// One-line `/lean-ctx`-style status shown above the savings numbers so the
    /// precondition for savings (bridge connected + tool-count) is always
    /// explicit.
    #[must_use]
    pub fn summary_line(&self) -> String {
        match self.engagement {
            BridgeEngagement::Engaged => format!(
                "Bridge: connected — {} tools, {} requests intercepted",
                self.tool_count, self.total_requests
            ),
            BridgeEngagement::NoRequests => format!(
                "Bridge: proxy up, 0 requests intercepted — {} tools exposed (route the editor through lean-ctx)",
                self.tool_count
            ),
            BridgeEngagement::ProxyDown if !self.proxy_configured => format!(
                "Bridge: OFF — proxy not configured (enable: lean-ctx proxy enable); savings cannot be measured ({} tools registered)",
                self.tool_count
            ),
            BridgeEngagement::ProxyDown => format!(
                "Bridge: OFF — proxy not reachable; savings cannot be measured ({} tools registered)",
                self.tool_count
            ),
        }
    }

    /// Explanation for a reported `0` saved, distinguishing "bridge off" from a
    /// genuine zero. Returns `None` when savings are non-zero (no hint needed).
    #[must_use]
    pub fn zero_savings_reason(&self, tokens_saved: u64) -> Option<String> {
        if tokens_saved > 0 {
            return None;
        }
        Some(match self.engagement {
            BridgeEngagement::ProxyDown => "saved=0 because the bridge is OFF — the proxy is not \
                 running, so no requests are intercepted. Start it (`lean-ctx serve`) and confirm \
                 `/lean-ctx` shows connected; reads/commands will then record savings."
                .to_string(),
            BridgeEngagement::NoRequests => {
                "saved=0 because the proxy has not intercepted any LLM \
                 request yet. Verify your editor's mcp.json routes through lean-ctx (`/lean-ctx` → \
                 connected), then retry."
                    .to_string()
            }
            BridgeEngagement::Engaged => "saved=0 is real for this window — the bridge is engaged \
                 but no compressible context was seen yet (e.g. only cold first reads). Re-run a \
                 read to populate the cache and savings will appear."
                .to_string(),
        })
    }
}

/// Cumulative intercepted-request count from the persisted introspection file,
/// or `0` when the file is missing, stale, or malformed.
fn persisted_request_count(max_age_secs: u64) -> u64 {
    crate::proxy::introspect::load_persisted(max_age_secs)
        .as_ref()
        .and_then(|v| v.get("cumulative"))
        .and_then(|c| c.get("total_requests"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn classify_proxy_down_when_not_running() {
        assert_eq!(classify(false, 0), BridgeEngagement::ProxyDown);
        // Even with stale request counts, an unreachable proxy is OFF.
        assert_eq!(classify(false, 1234), BridgeEngagement::ProxyDown);
    }

    #[test]
    fn classify_no_requests_when_running_but_idle() {
        assert_eq!(classify(true, 0), BridgeEngagement::NoRequests);
    }

    #[test]
    fn classify_engaged_when_running_with_traffic() {
        assert_eq!(classify(true, 1), BridgeEngagement::Engaged);
        assert_eq!(classify(true, 50_000), BridgeEngagement::Engaged);
    }

    #[test]
    fn zero_savings_reason_is_none_when_savings_present() {
        let status = BridgeStatus {
            engagement: BridgeEngagement::Engaged,
            proxy_running: true,
            proxy_configured: true,
            total_requests: 10,
            tool_count: 69,
        };
        assert!(status.zero_savings_reason(42).is_none());
    }

    #[test]
    fn zero_savings_reason_distinguishes_off_from_real_zero() {
        let off = BridgeStatus {
            engagement: BridgeEngagement::ProxyDown,
            proxy_running: false,
            proxy_configured: true,
            total_requests: 0,
            tool_count: 69,
        };
        let real = BridgeStatus {
            engagement: BridgeEngagement::Engaged,
            proxy_running: true,
            proxy_configured: true,
            total_requests: 10,
            tool_count: 69,
        };
        let off_msg = off.zero_savings_reason(0).expect("off has a reason");
        let real_msg = real.zero_savings_reason(0).expect("engaged has a reason");
        assert!(off_msg.contains("bridge is OFF"), "got: {off_msg}");
        assert!(real_msg.contains("is real"), "got: {real_msg}");
        assert_ne!(off_msg, real_msg, "off and real-zero must differ");
    }

    #[test]
    fn summary_line_reflects_engagement() {
        let engaged = BridgeStatus {
            engagement: BridgeEngagement::Engaged,
            proxy_running: true,
            proxy_configured: true,
            total_requests: 7,
            tool_count: 69,
        };
        let line = engaged.summary_line();
        assert!(line.contains("connected"));
        assert!(line.contains("69 tools"));
        assert!(line.contains("7 requests"));
    }
}
