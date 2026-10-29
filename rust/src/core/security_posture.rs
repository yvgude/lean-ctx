//! Unified, human-facing view of lean-ctx's **two independent security planes**:
//!
//! 1. **Containment** — the path jail + shell-command gating. Protects *the
//!    machine from the agent* (what files a tool may touch, what binaries the
//!    shell may run).
//! 2. **Secret-exfiltration defense** — secret/`.env` redaction. Protects *your
//!    secrets from the LLM provider* (API keys masked before they reach the
//!    model).
//!
//! These are orthogonal by design: a usability-first user can drop containment
//! (`lean-ctx yolo`) while still never leaking credentials to the provider, and
//! vice-versa. This module is the single source of truth both the
//! `lean-ctx security` command and `lean-ctx doctor` read from, so the CLI
//! status screen and the doctor board can never disagree.
//!
//! It is a **pure read** of config + env (no side effects), which keeps it cheap
//! to call and safe to use inside deterministic output paths.

use crate::core::config::Config;
use crate::core::shell_allowlist::ShellSecurity;
use std::path::Path;

/// Effective state of the filesystem path jail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JailState {
    /// Fully enforced: tools are confined to the project root (+ any configured
    /// `allow_paths`/`extra_roots`), with no blanket relaxation active.
    Enforced,
    /// Enforced, but widened by one or more knobs (e.g. `LEAN_CTX_ALLOW_PATH`,
    /// `extra_roots`, IDE-config dirs). Carries the source labels for display.
    Relaxed(Vec<String>),
    /// Disabled outright — every tool path is allowed. Carries the knob that
    /// turned it off (`path_jail = false`, the `no-jail` build, or
    /// `allow_paths = ["/"]`).
    Disabled(String),
    /// Bound to a launcher Workspace (#2120): only its roots, regardless of
    /// every widening knob and off-switch. Carries the binding for display.
    Workspace(String),
}

impl JailState {
    /// True when containment over the filesystem is effectively gone.
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        matches!(self, JailState::Disabled(_))
    }
}

/// Coarse, derived label summarising the whole posture for at-a-glance display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostureLevel {
    /// Both containment planes fully enforced (the secure default).
    Strict,
    /// Partially relaxed (e.g. jail widened, or shell in `warn`).
    Relaxed,
    /// Containment fully off (`yolo`) — jail disabled *and* shell gating off.
    Open,
}

impl PostureLevel {
    /// Lower-case, stable name (used in status output and tests).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PostureLevel::Strict => "strict",
            PostureLevel::Relaxed => "relaxed",
            PostureLevel::Open => "open",
        }
    }
}

/// A snapshot of every security-relevant switch, resolved exactly the way the
/// runtime enforces it (env → config → secure default).
#[derive(Debug, Clone)]
pub struct SecurityPosture {
    /// Filesystem path-jail state.
    pub jail: JailState,
    /// Shell-command gating mode.
    pub shell: ShellSecurity,
    /// Whether secret/`.env` detection runs on tool output.
    pub secrets_enabled: bool,
    /// Whether detected secrets are actually masked (vs only flagged).
    pub secrets_redact: bool,
}

impl SecurityPosture {
    /// Resolve the live posture from config + env. Pure read, no side effects.
    #[must_use]
    pub fn detect() -> Self {
        let cfg = Config::load();
        Self {
            jail: detect_jail(&cfg),
            shell: ShellSecurity::resolve(),
            secrets_enabled: cfg.secret_detection.enabled,
            secrets_redact: cfg.secret_detection.redact,
        }
    }

    /// Derived coarse label. `Open` only when *both* containment planes are off,
    /// so it precisely reflects what `lean-ctx yolo` produces.
    #[must_use]
    pub fn level(&self) -> PostureLevel {
        let containment_off = self.jail.is_disabled() && self.shell == ShellSecurity::Off;
        if containment_off {
            return PostureLevel::Open;
        }
        let strict = matches!(self.jail, JailState::Enforced | JailState::Workspace(_))
            && self.shell == ShellSecurity::Enforce;
        if strict {
            PostureLevel::Strict
        } else {
            PostureLevel::Relaxed
        }
    }

    /// True when secrets still cannot leak to the provider (detection + masking
    /// both on). This stays independent of [`Self::level`] on purpose.
    #[must_use]
    pub fn secrets_protected(&self) -> bool {
        self.secrets_enabled && self.secrets_redact
    }
}

/// Mirror of the precedence in `pathjail` + the doctor `path_jail_outcome`, kept
/// here as the single classifier so CLI and doctor agree on what "disabled"
/// means.
fn detect_jail(cfg: &Config) -> JailState {
    match crate::core::workspace_authority::load_current() {
        crate::core::workspace_authority::Authority::Legacy => {}
        crate::core::workspace_authority::Authority::Workspace(snapshot) => {
            return JailState::Workspace(format!(
                "Workspace {} rev {}",
                snapshot.id, snapshot.revision
            ));
        }
        crate::core::workspace_authority::Authority::Unavailable(_) => {
            return JailState::Workspace("Workspace unavailable — every path denied".to_string());
        }
    }
    if cfg!(feature = "no-jail") {
        return JailState::Disabled("no-jail build feature".to_string());
    }
    if cfg.path_jail == Some(false) {
        return JailState::Disabled("path_jail = false".to_string());
    }
    // `allow_paths`/`extra_roots` containing "/" is a prefix of everything, so
    // it grants blanket access just like `path_jail = false` (GH #392).
    let grants_everything = cfg
        .allow_paths
        .iter()
        .chain(cfg.extra_roots.iter())
        .any(|raw| crate::core::pathjail::expand_user_path(raw) == Path::new("/"));
    if grants_everything {
        return JailState::Disabled("allow_paths contains \"/\"".to_string());
    }

    // Remaining relaxations (env channels, IDE-config dirs) only *widen* the
    // jail; the full-disable sources above are handled by the early returns.
    let relaxed: Vec<String> = crate::core::pathjail::active_relaxations()
        .into_iter()
        .map(|r| r.source.to_string())
        .collect();
    if relaxed.is_empty() {
        JailState::Enforced
    } else {
        JailState::Relaxed(relaxed)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn posture(
        jail: JailState,
        shell: ShellSecurity,
        secrets: bool,
        redact: bool,
    ) -> SecurityPosture {
        SecurityPosture {
            jail,
            shell,
            secrets_enabled: secrets,
            secrets_redact: redact,
        }
    }

    #[test]
    fn level_strict_when_both_planes_enforced() {
        let p = posture(JailState::Enforced, ShellSecurity::Enforce, true, true);
        assert_eq!(p.level(), PostureLevel::Strict);
    }

    #[test]
    fn level_open_only_when_jail_disabled_and_shell_off() {
        let p = posture(
            JailState::Disabled("path_jail = false".into()),
            ShellSecurity::Off,
            true,
            true,
        );
        assert_eq!(p.level(), PostureLevel::Open);
    }

    #[test]
    fn level_relaxed_when_only_one_plane_dropped() {
        // Jail off but shell still enforcing → not fully open.
        let jail_only = posture(
            JailState::Disabled("path_jail = false".into()),
            ShellSecurity::Enforce,
            true,
            true,
        );
        assert_eq!(jail_only.level(), PostureLevel::Relaxed);

        // Shell off but jail enforced → not fully open.
        let shell_only = posture(JailState::Enforced, ShellSecurity::Off, true, true);
        assert_eq!(shell_only.level(), PostureLevel::Relaxed);

        // Jail merely widened (not disabled) → relaxed.
        let widened = posture(
            JailState::Relaxed(vec!["LEAN_CTX_ALLOW_PATH".into()]),
            ShellSecurity::Enforce,
            true,
            true,
        );
        assert_eq!(widened.level(), PostureLevel::Relaxed);
    }

    #[test]
    fn secrets_protection_is_independent_of_containment() {
        // Fully open containment, yet secrets are still protected.
        let open_but_secret_safe = posture(
            JailState::Disabled("path_jail = false".into()),
            ShellSecurity::Off,
            true,
            true,
        );
        assert_eq!(open_but_secret_safe.level(), PostureLevel::Open);
        assert!(open_but_secret_safe.secrets_protected());

        // Strict containment, yet redaction explicitly turned off.
        let strict_but_leaky = posture(JailState::Enforced, ShellSecurity::Enforce, false, true);
        assert_eq!(strict_but_leaky.level(), PostureLevel::Strict);
        assert!(!strict_but_leaky.secrets_protected());
    }

    #[test]
    fn jail_state_is_disabled_helper() {
        assert!(JailState::Disabled("x".into()).is_disabled());
        assert!(!JailState::Enforced.is_disabled());
        assert!(!JailState::Relaxed(vec!["y".into()]).is_disabled());
    }

    #[test]
    fn posture_level_names_are_stable() {
        assert_eq!(PostureLevel::Strict.as_str(), "strict");
        assert_eq!(PostureLevel::Relaxed.as_str(), "relaxed");
        assert_eq!(PostureLevel::Open.as_str(), "open");
    }
}
