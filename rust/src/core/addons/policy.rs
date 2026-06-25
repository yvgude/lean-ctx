//! Install policy for addons — the org-controllable floor (#865).
//!
//! [`AddonsConfig`] is the `[addons]` config block. Like `[gateway]`, it is
//! **global-only** (never merged from a project-local `.lean-ctx.toml`), so a
//! cloned, untrusted repo cannot loosen it; an org distributes it via
//! MDM / config-management or pins it through the signed org-policy floor.
//!
//! [`gate`] is the single enforcement point, called by [`super::install`] before
//! any addon is wired into the gateway. It is pure (config + findings in,
//! verdict out) and fully unit-tested.

use serde::{Deserialize, Serialize};

use super::manifest::AddonManifest;
use super::sandbox::SandboxMode;
use super::trust::{RiskFinding, RiskLevel, TrustTier};

/// What the endpoint allows to be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddonPolicy {
    /// Any registry addon may be installed (default — friction-free).
    #[default]
    Open,
    /// Only `verified` (maintainer-vouched) addons may be installed.
    VerifiedOnly,
    /// Only addons whose slug is on [`AddonsConfig::allowlist`].
    Allowlist,
    /// Installing addons is disabled entirely.
    Locked,
}

impl AddonPolicy {
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "verified_only" | "verified" => Self::VerifiedOnly,
            "allowlist" => Self::Allowlist,
            "locked" | "off" | "disabled" => Self::Locked,
            _ => Self::Open,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::VerifiedOnly => "verified_only",
            Self::Allowlist => "allowlist",
            Self::Locked => "locked",
        }
    }
}

/// `[addons]` configuration. Global-only; default is fully permissive so the
/// out-of-the-box experience is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AddonsConfig {
    /// Install policy: `open` | `verified_only` | `allowlist` | `locked`.
    pub policy: String,
    /// Slugs permitted when `policy = allowlist`.
    pub allowlist: Vec<String>,
    /// Honour a user-override registry (`<data_dir>/addon_registry.json`) only
    /// when it carries a valid signature by a trusted org key.
    pub require_signature: bool,
    /// Sandbox spawned stdio servers without a declared `[capabilities]` block:
    /// `off` | `auto` | `strict` (the legacy global mode).
    pub sandbox: String,
    /// Refuse to install an addon that has a high-risk (`Danger`) capability.
    pub block_risky: bool,
    /// Fail closed when an addon declares restricted `[capabilities]` but no OS
    /// sandbox launcher (sandbox-exec / bwrap) is available to enforce them. Off
    /// by default → best-effort (warn + run) so a missing launcher never blocks
    /// a spawn; orgs that require real enforcement set this to `true`.
    pub enforce_capabilities: bool,
    /// Record per-addon / per-tool gateway usage counters to
    /// `<data_dir>/addons/usage.json` (local-only; basis for analytics + billing,
    /// P5). On by default; set `false` to disable all usage accounting.
    pub metering: bool,
}

impl Default for AddonsConfig {
    fn default() -> Self {
        Self {
            policy: AddonPolicy::Open.as_str().to_string(),
            allowlist: Vec::new(),
            require_signature: false,
            sandbox: SandboxMode::Off.as_str().to_string(),
            block_risky: false,
            enforce_capabilities: false,
            metering: true,
        }
    }
}

impl AddonsConfig {
    /// The parsed install policy.
    #[must_use]
    pub fn policy(&self) -> AddonPolicy {
        AddonPolicy::parse(&self.policy)
    }

    /// The parsed sandbox mode.
    #[must_use]
    pub fn sandbox_mode(&self) -> SandboxMode {
        SandboxMode::parse(&self.sandbox)
    }

    fn allows_slug(&self, slug: &str) -> bool {
        self.allowlist
            .iter()
            .any(|a| a.trim().eq_ignore_ascii_case(slug.trim()))
    }
}

/// Decide whether `manifest` may be installed under `cfg`, given its risk
/// `findings` (from [`super::trust::assess`]). Pure + deterministic.
pub fn gate(
    manifest: &AddonManifest,
    cfg: &AddonsConfig,
    findings: &[RiskFinding],
) -> Result<(), String> {
    let name = &manifest.addon.name;
    match cfg.policy() {
        AddonPolicy::Open => {}
        AddonPolicy::VerifiedOnly => {
            if TrustTier::of(manifest) != TrustTier::Verified {
                return Err(format!(
                    "addons.policy = verified_only: `{name}` is community-tier (not maintainer-verified). \
                     Set addons.policy = open to install community addons."
                ));
            }
        }
        AddonPolicy::Allowlist => {
            if !cfg.allows_slug(name) {
                return Err(format!(
                    "addons.policy = allowlist: `{name}` is not on addons.allowlist. \
                     Add it with `lean-ctx config set addons.allowlist <slugs>`."
                ));
            }
        }
        AddonPolicy::Locked => {
            return Err(
                "addons.policy = locked: installing addons is disabled on this machine."
                    .to_string(),
            );
        }
    }

    if cfg.block_risky
        && let Some(danger) = findings.iter().find(|f| f.level == RiskLevel::Danger)
    {
        return Err(format!(
            "addons.block_risky is on: `{name}` has a high-risk capability — {} \
             Review it, then install with addons.block_risky = false if intended.",
            danger.message
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(name: &str, verified: bool) -> AddonManifest {
        AddonManifest::from_toml(&format!(
            "[addon]\nname = \"{name}\"\nverified = {verified}\n\
             [mcp]\ntransport = \"stdio\"\ncommand = \"x\"\n"
        ))
        .expect("parse")
    }

    #[test]
    fn default_policy_is_open_and_permissive() {
        let cfg = AddonsConfig::default();
        assert_eq!(cfg.policy(), AddonPolicy::Open);
        assert_eq!(cfg.sandbox_mode(), SandboxMode::Off);
        assert!(gate(&manifest("x", false), &cfg, &[]).is_ok());
    }

    #[test]
    fn policy_parse_is_lenient() {
        assert_eq!(
            AddonPolicy::parse("verified-only"),
            AddonPolicy::VerifiedOnly
        );
        assert_eq!(AddonPolicy::parse("LOCKED"), AddonPolicy::Locked);
        assert_eq!(AddonPolicy::parse("garbage"), AddonPolicy::Open);
    }

    #[test]
    fn verified_only_blocks_community() {
        let cfg = AddonsConfig {
            policy: "verified_only".into(),
            ..Default::default()
        };
        assert!(gate(&manifest("c", false), &cfg, &[]).is_err());
        assert!(gate(&manifest("v", true), &cfg, &[]).is_ok());
    }

    #[test]
    fn allowlist_only_permits_listed_slugs() {
        let cfg = AddonsConfig {
            policy: "allowlist".into(),
            allowlist: vec!["allowed".into()],
            ..Default::default()
        };
        assert!(gate(&manifest("allowed", false), &cfg, &[]).is_ok());
        assert!(gate(&manifest("other", false), &cfg, &[]).is_err());
    }

    #[test]
    fn locked_blocks_everything() {
        let cfg = AddonsConfig {
            policy: "locked".into(),
            ..Default::default()
        };
        assert!(gate(&manifest("v", true), &cfg, &[]).is_err());
    }

    #[test]
    fn block_risky_refuses_danger_findings() {
        let cfg = AddonsConfig {
            block_risky: true,
            ..Default::default()
        };
        let danger = vec![RiskFinding {
            level: RiskLevel::Danger,
            code: "shell_exec",
            message: "shells out".into(),
        }];
        assert!(gate(&manifest("x", false), &cfg, &danger).is_err());
        // A non-danger finding is fine.
        let info = vec![RiskFinding {
            level: RiskLevel::Info,
            code: "child_env",
            message: "env".into(),
        }];
        assert!(gate(&manifest("x", false), &cfg, &info).is_ok());
    }
}
