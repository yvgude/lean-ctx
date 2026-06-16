//! Opt-in background auto-push of the local savings batch to the team server.
//!
//! When `team_auto_push` is enabled **and** both `team_url` and `team_token` are
//! configured, the running daemon periodically (re-)pushes this machine's signed
//! savings snapshot, so the team roll-up (`GET /v1/savings/summary`) fills itself
//! — no manual `lean-ctx savings push` per developer. This is what turns the
//! Team savings dashboard from "empty by default" into a self-proving ROI view.
//!
//! Privacy: off by default and strictly opt-in. The batch carries only token
//! counts, model names, tool names and chain hashes — never prompts or code —
//! consistent with the zero-telemetry default.

use std::time::Duration;

use crate::core::config::Config;
use crate::core::savings_ledger::push::{PushError, push_batch};

/// How often the daemon re-snapshots + pushes. The batch is a cumulative
/// whole-ledger snapshot, so re-pushing is idempotent on the server.
const PUSH_INTERVAL: Duration = Duration::from_hours(6);

/// Delay before the first push so daemon startup stays light.
const INITIAL_DELAY: Duration = Duration::from_mins(1);

/// Resolved auto-push settings (only present when enabled + fully configured).
struct AutoPushConfig {
    url: String,
    token: String,
}

/// Resolve auto-push config: enabled flag + non-empty `team_url` + a bearer token
/// (`LEAN_CTX_TEAM_TOKEN` env wins over `team_token` in config, matching the CLI).
fn resolve(cfg: Config) -> Option<AutoPushConfig> {
    if !cfg.team_auto_push {
        return None;
    }
    let url = cfg.team_url.filter(|s| !s.trim().is_empty())?;
    let token = std::env::var("LEAN_CTX_TEAM_TOKEN")
        .ok()
        .or(cfg.team_token)
        .filter(|s| !s.trim().is_empty())?;
    Some(AutoPushConfig { url, token })
}

/// Spawn the auto-push loop if enabled; otherwise a silent no-op. Safe to call
/// once at daemon/server startup (must run inside a Tokio runtime).
pub fn spawn_if_enabled() {
    let Some(cfg) = resolve(Config::load()) else {
        return;
    };
    tracing::info!(
        "savings auto-push enabled → {} (every {}h)",
        cfg.url,
        PUSH_INTERVAL.as_secs() / 3600
    );
    tokio::spawn(async move {
        tokio::time::sleep(INITIAL_DELAY).await;
        loop {
            run_once(&cfg).await;
            tokio::time::sleep(PUSH_INTERVAL).await;
        }
    });
}

/// One best-effort push. `push_batch` is blocking (ureq), so it runs on the
/// blocking pool to keep the async reactor free. Failures are logged, never fatal.
async fn run_once(cfg: &AutoPushConfig) {
    let url = cfg.url.clone();
    let token = cfg.token.clone();
    let result = tokio::task::spawn_blocking(move || push_batch(&url, Some(&token))).await;
    match result {
        Ok(Ok(outcome)) => tracing::info!(
            "savings auto-push ok: net {} tokens (~${:.2})",
            outcome.net_saved_tokens,
            outcome.saved_usd
        ),
        Ok(Err(PushError::Empty)) => {
            tracing::debug!("savings auto-push: ledger empty, nothing to send");
        }
        Ok(Err(e)) => tracing::warn!("savings auto-push failed: {e}"),
        Err(e) => tracing::warn!("savings auto-push task join error: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        Config {
            team_url: Some("https://team.example.com".into()),
            team_token: Some("tok".into()),
            team_auto_push: true,
            ..Default::default()
        }
    }

    #[test]
    fn disabled_when_flag_off() {
        let mut c = base();
        c.team_auto_push = false;
        assert!(resolve(c).is_none());
    }

    #[test]
    fn disabled_when_url_missing() {
        let mut c = base();
        c.team_url = None;
        assert!(resolve(c).is_none());
    }

    #[test]
    fn disabled_when_url_blank() {
        let mut c = base();
        c.team_url = Some("   ".into());
        assert!(resolve(c).is_none());
    }

    #[test]
    fn enabled_when_fully_configured() {
        // Guard against a CI env that sets the token override.
        let prior = std::env::var("LEAN_CTX_TEAM_TOKEN").ok();
        unsafe { std::env::remove_var("LEAN_CTX_TEAM_TOKEN") };
        let resolved = resolve(base());
        assert!(resolved.is_some());
        let cfg = resolved.unwrap();
        assert_eq!(cfg.url, "https://team.example.com");
        assert_eq!(cfg.token, "tok");
        if let Some(v) = prior {
            unsafe { std::env::set_var("LEAN_CTX_TEAM_TOKEN", v) };
        }
    }
}
