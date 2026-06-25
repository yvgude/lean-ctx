//! Extension trust & sandbox model (EPIC 12.3).
//!
//! Every plugin subprocess (hooks + manifest tools) runs under a
//! [`SandboxPolicy`] derived from the plugin's declared `[trust]` section. The
//! model is **least-privilege by default** and splits cleanly into two honest
//! categories so we never claim enforcement we do not perform:
//!
//! * **Enforced, deterministically** — environment isolation (the child gets a
//!   scrubbed env containing only a fixed allowlist, so host secrets in env do
//!   not leak), working-directory jail (cwd pinned to the plugin dir), and a
//!   per-call timeout (in [`super::executor`]).
//! * **Declared (consent surface)** — `network` / `fs_write`. These cannot be
//!   blocked portably without OS namespaces/seccomp, so they are *declared*
//!   capabilities surfaced to the user (and `/v1/capabilities`) for informed
//!   trust, not silent OS-level blocks.
//!
//! Granting `env_passthrough` opts a plugin out of env scrubbing (it then sees
//! the full host environment) — an explicit elevation a user can audit.

use std::path::Path;
use std::process::Command;

use serde::Deserialize;

/// Host environment variables a scrubbed child is still allowed to see. Chosen
/// to let normal programs run (binary resolution, locale, temp dir) without
/// exposing secrets that tend to live in the ambient environment.
pub const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "TEMP",
    "TMP",
    // Windows needs these for most binaries to start.
    "SystemRoot",
    "SYSTEMROOT",
    "ComSpec",
    "PATHEXT",
];

/// A single capability a plugin may request in its `[trust]` section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Plugin intends to make network calls (declared; surfaced, not blocked).
    Network,
    /// Plugin intends to write files outside its own dir (declared; surfaced).
    FsWrite,
    /// Plugin opts out of env scrubbing and receives the full host environment.
    EnvPassthrough,
}

impl Permission {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "network" => Some(Self::Network),
            "fs_write" => Some(Self::FsWrite),
            "env_passthrough" => Some(Self::EnvPassthrough),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::FsWrite => "fs_write",
            Self::EnvPassthrough => "env_passthrough",
        }
    }
}

/// Declarative `[trust]` section of a plugin manifest. Absent ⇒ least privilege.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TrustSpec {
    /// Requested capabilities (`network`, `fs_write`, `env_passthrough`).
    #[serde(default)]
    pub permissions: Vec<String>,
}

impl TrustSpec {
    /// Validate that every declared permission is recognized (fail-closed: an
    /// unknown permission is a manifest error, not a silent grant).
    pub fn validate(&self) -> Result<(), String> {
        for p in &self.permissions {
            if Permission::parse(p).is_none() {
                return Err(format!("unknown permission '{p}'"));
            }
        }
        Ok(())
    }

    /// Resolve the enforceable policy. Unknown strings are ignored here because
    /// `validate()` already rejects them at parse time.
    #[must_use]
    pub fn policy(&self) -> SandboxPolicy {
        let perms: Vec<Permission> = self
            .permissions
            .iter()
            .filter_map(|p| Permission::parse(p))
            .collect();
        SandboxPolicy::from_permissions(&perms)
    }
}

/// The resolved, enforceable sandbox for a plugin subprocess. The derived
/// `Default` is least privilege (all `false`): scrubbed env, nothing declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SandboxPolicy {
    /// When false (default) the child runs with a scrubbed env (allowlist only).
    pub env_passthrough: bool,
    /// Declared network intent (surfaced, not OS-enforced).
    pub allow_network: bool,
    /// Declared out-of-dir write intent (surfaced, not OS-enforced).
    pub allow_fs_write: bool,
}

impl SandboxPolicy {
    /// The strictest policy: scrubbed env, no declared capabilities.
    #[must_use]
    pub fn strict() -> Self {
        Self::default()
    }

    /// A policy mirroring legacy behavior (full host env). Used only where the
    /// caller is not a plugin (e.g. direct internal subprocess helpers/tests).
    #[must_use]
    pub fn permissive() -> Self {
        Self {
            env_passthrough: true,
            allow_network: true,
            allow_fs_write: true,
        }
    }

    #[must_use]
    pub fn from_permissions(perms: &[Permission]) -> Self {
        Self {
            env_passthrough: perms.contains(&Permission::EnvPassthrough),
            allow_network: perms.contains(&Permission::Network),
            allow_fs_write: perms.contains(&Permission::FsWrite),
        }
    }

    /// The declared permissions, as stable strings (for capabilities/audit).
    #[must_use]
    pub fn declared_permissions(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.allow_network {
            out.push(Permission::Network.as_str());
        }
        if self.allow_fs_write {
            out.push(Permission::FsWrite.as_str());
        }
        if self.env_passthrough {
            out.push(Permission::EnvPassthrough.as_str());
        }
        out
    }

    /// Apply the *enforced* controls to a [`Command`] before spawn: env scrub
    /// (unless `env_passthrough`) and cwd jail to `plugin_dir` (when it exists).
    /// The timeout is enforced separately by the executor's wait loop.
    pub fn apply(&self, cmd: &mut Command, plugin_dir: &Path) {
        if !self.env_passthrough {
            cmd.env_clear();
            for key in ENV_ALLOWLIST {
                if let Ok(val) = std::env::var(key) {
                    cmd.env(key, val);
                }
            }
        }
        // Jail to the plugin dir so relative paths resolve there. Guard on
        // existence: pointing cwd at a missing dir would make spawn fail.
        if plugin_dir.is_dir() {
            cmd.current_dir(plugin_dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_least_privilege() {
        let p = SandboxPolicy::default();
        assert!(!p.env_passthrough);
        assert!(!p.allow_network);
        assert!(!p.allow_fs_write);
        assert!(p.declared_permissions().is_empty());
    }

    #[test]
    fn parse_permissions_roundtrip() {
        for s in ["network", "fs_write", "env_passthrough"] {
            assert_eq!(Permission::parse(s).unwrap().as_str(), s);
        }
        assert!(Permission::parse("rm_rf_everything").is_none());
    }

    #[test]
    fn trust_spec_rejects_unknown_permission() {
        let spec = TrustSpec {
            permissions: vec!["network".into(), "bogus".into()],
        };
        assert!(spec.validate().unwrap_err().contains("bogus"));
    }

    #[test]
    fn policy_reflects_declared_permissions() {
        let spec = TrustSpec {
            permissions: vec!["network".into(), "env_passthrough".into()],
        };
        let policy = spec.policy();
        assert!(policy.allow_network);
        assert!(policy.env_passthrough);
        assert!(!policy.allow_fs_write);
        let declared = policy.declared_permissions();
        assert!(declared.contains(&"network"));
        assert!(declared.contains(&"env_passthrough"));
    }

    #[cfg(unix)]
    #[test]
    fn scrubbed_env_hides_host_secret_but_keeps_path() {
        use std::time::Duration;
        // A secret in the host env must NOT reach a scrubbed child.
        crate::test_env::set_var("LEAN_CTX_TEST_SECRET", "top-secret");
        let out = crate::core::plugins::executor::run_subprocess(
            "env",
            std::path::Path::new("/tmp"),
            &[],
            "",
            Duration::from_secs(2),
            &SandboxPolicy::strict(),
        )
        .unwrap();
        let env_dump = String::from_utf8_lossy(&out.stdout);
        crate::test_env::remove_var("LEAN_CTX_TEST_SECRET");
        assert!(
            !env_dump.contains("top-secret"),
            "scrubbed child leaked host secret"
        );
        // PATH survives so binaries still resolve.
        assert!(env_dump.contains("PATH="));
    }

    #[cfg(unix)]
    #[test]
    fn passthrough_env_exposes_host_var() {
        use std::time::Duration;
        crate::test_env::set_var("LEAN_CTX_TEST_PASSTHRU", "visible");
        let out = crate::core::plugins::executor::run_subprocess(
            "env",
            std::path::Path::new("/tmp"),
            &[],
            "",
            Duration::from_secs(2),
            &SandboxPolicy::permissive(),
        )
        .unwrap();
        let env_dump = String::from_utf8_lossy(&out.stdout);
        crate::test_env::remove_var("LEAN_CTX_TEST_PASSTHRU");
        assert!(env_dump.contains("visible"));
    }
}
