//! Declared capability model for addons (P1 — platform keystone).
//!
//! An addon's optional `[capabilities]` block tells lean-ctx exactly what the
//! addon needs: outbound network, filesystem writes, and which host environment
//! variables it may receive. The declaration is **secure-by-default** — an
//! addon that declares a `[capabilities]` block but omits a field gets the most
//! restrictive value (no network, read-only filesystem, scrubbed environment).
//!
//! A declared block drives two real, *enforced* controls at the single gateway
//! spawn point ([`crate::core::gateway::client`]):
//!
//! 1. the per-addon OS sandbox profile ([`super::sandbox`]) — network egress and
//!    filesystem writes are wrapped via `sandbox-exec` (macOS) / `bwrap` (Linux),
//! 2. the environment allowlist — host secrets never reach the child unless the
//!    addon explicitly lists the variable name,
//!
//! and is surfaced to the user for explicit consent at install time
//! ([`crate::cli::addon_cmd`]). Child processes inherit the OS sandbox, so a
//! subprocess an addon spawns is bound by the same network/filesystem limits;
//! the declared `exec` capability is therefore disclosed + audited rather than
//! OS-enforced (see [`super::sandbox`]).
//!
//! Unlike the legacy blanket `addons.sandbox` mode, this is *per addon* and
//! bound to the manifest, so a marketplace addon is granted exactly what it
//! asked for — no more. Addons **without** a `[capabilities]` block keep the
//! legacy behaviour (governed by `addons.sandbox`) so existing installs do not
//! change.

use serde::{Deserialize, Serialize};

/// Host environment variables a scrubbed child is always allowed to see, on top
/// of whatever the addon declares. Chosen to let normal programs start (binary
/// resolution, locale, temp dir) without exposing ambient secrets. Reuses the
/// plugin allowlist so the two sandboxes converge on one list (P0).
pub use crate::core::plugins::sandbox::ENV_ALLOWLIST as BASE_ENV_ALLOWLIST;

/// Outbound-network capability a stdio addon declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NetworkAccess {
    /// No outbound network. The default — most local tools never need it, and
    /// blocking egress is the single highest-value sandbox control.
    #[default]
    None,
    /// Full outbound network (the addon talks to the internet / remote APIs).
    Full,
}

impl NetworkAccess {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Full => "full",
        }
    }

    /// Whether the OS sandbox should permit outbound network.
    #[must_use]
    pub fn allowed(self) -> bool {
        matches!(self, Self::Full)
    }
}

/// Filesystem capability a stdio addon declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemAccess {
    /// Read-only filesystem; writes restricted to a scratch tmp. The default.
    #[default]
    ReadOnly,
    /// Read-write filesystem (the addon needs to write outside tmp).
    ReadWrite,
}

impl FilesystemAccess {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::ReadWrite => "read_write",
        }
    }

    /// Whether the OS sandbox should permit filesystem writes.
    #[must_use]
    pub fn writable(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

/// Subprocess-execution capability a stdio addon declares.
///
/// Modeled as an untagged enum so the manifest can write either a mode string
/// or a binary allowlist:
///
/// ```toml
/// exec = "none"              # block all child process execution (default)
/// exec = "full"             # may execute any binary
/// exec = ["lean-ctx", "git"] # may execute exactly these binaries (by name/path)
/// ```
///
/// `exec` is a **declared, audited and consented** capability — it is *not*
/// OS-enforced (see [`super::sandbox`]): path-allowlisting `execve` is not
/// portable (`bwrap`/seccomp cannot do it) and breaks interpreted servers,
/// whose own interpreter chain is itself a `process-exec`. The real data-safety
/// guarantees come from the network/filesystem sandbox, which child processes
/// inherit — so a subprocess an addon spawns still cannot exfiltrate or tamper.
/// Declaring `exec` keeps the audit honest (an addon that shells out must say
/// so) and is surfaced for consent at install on every platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExecAccess {
    /// A bare mode: `"none"` (block all child exec) or `"full"` (unrestricted).
    Mode(ExecMode),
    /// An explicit allowlist of binary names / absolute paths the addon may
    /// `execve`. An empty list is equivalent to [`ExecMode::None`].
    Allowlist(Vec<String>),
}

/// The two bare exec modes (the non-allowlist forms of [`ExecAccess`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecMode {
    /// No child process execution. The default — most addons never spawn
    /// subprocesses, and arbitrary `execve` is the highest-impact escape.
    #[default]
    None,
    /// May execute any binary (the legacy, unrestricted behaviour).
    Full,
}

impl Default for ExecAccess {
    fn default() -> Self {
        Self::Mode(ExecMode::None)
    }
}

impl ExecAccess {
    /// Whether the addon is permitted to execute *any* child process at all.
    /// `full` and a non-empty allowlist are permissive; `none` / empty list are
    /// not.
    #[must_use]
    pub fn allowed(&self) -> bool {
        match self {
            Self::Mode(ExecMode::Full) => true,
            Self::Mode(ExecMode::None) => false,
            Self::Allowlist(list) => !list.is_empty(),
        }
    }

    /// Whether this is a restricted declaration (`none` or an allowlist) vs.
    /// blanket `full`. Drives the audit/consent disclosure, not OS enforcement.
    #[must_use]
    pub fn is_restricted(&self) -> bool {
        !matches!(self, Self::Mode(ExecMode::Full))
    }

    /// The declared allowlist of binaries, if any (`full`/`none` have none).
    #[must_use]
    pub fn allowlist(&self) -> &[String] {
        match self {
            Self::Allowlist(list) => list,
            Self::Mode(_) => &[],
        }
    }

    /// Short human label for the consent preview.
    #[must_use]
    fn label(&self) -> String {
        match self {
            Self::Mode(ExecMode::None) => "none (no subprocesses)".to_string(),
            Self::Mode(ExecMode::Full) => "full (any binary)".to_string(),
            Self::Allowlist(list) if list.is_empty() => "none (empty allowlist)".to_string(),
            Self::Allowlist(list) => format!("only {}", list.join(", ")),
        }
    }
}

/// `[capabilities]` — what an addon is permitted to do. A present-but-empty
/// block resolves to the strictest profile (see module docs). Secure-by-default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AddonCapabilities {
    /// Outbound network access.
    pub network: NetworkAccess,
    /// Filesystem access.
    pub filesystem: FilesystemAccess,
    /// Host environment variable names the addon may receive, in addition to
    /// [`BASE_ENV_ALLOWLIST`]. Anything not listed is scrubbed before spawn, so
    /// ambient secrets never reach the child process.
    pub env: Vec<String>,
    /// Subprocess-execution permission. Defaults to [`ExecMode::None`] inside a
    /// declared block (secure-by-default): an addon that spawns child processes
    /// (e.g. shells out, or calls back into `lean-ctx call`) must declare it.
    pub exec: ExecAccess,
}

impl AddonCapabilities {
    /// True when the addon declares no elevated capability — the strictest,
    /// default profile and the safest to run.
    #[must_use]
    pub fn is_minimal(&self) -> bool {
        self.network == NetworkAccess::None
            && self.filesystem == FilesystemAccess::ReadOnly
            && self.env.is_empty()
            && !self.exec.allowed()
    }

    /// Whether the addon may execute child processes at all.
    #[must_use]
    pub fn exec_allowed(&self) -> bool {
        self.exec.allowed()
    }

    /// Whether the addon declares a restricted exec profile (`none` or an
    /// allowlist, vs. blanket `full`). Used by the audit + consent surface;
    /// `exec` is not OS-enforced (see [`super::sandbox`]).
    #[must_use]
    pub fn exec_restricted(&self) -> bool {
        self.exec.is_restricted()
    }

    /// Whether exec is a blanket `full` grant (vs. an allowlist or `none`). Used
    /// by the audit to nudge blanket grants toward least privilege.
    #[must_use]
    pub fn exec_is_blanket(&self) -> bool {
        matches!(self.exec, ExecAccess::Mode(ExecMode::Full))
    }

    /// Whether the OS sandbox should permit outbound network for this addon.
    #[must_use]
    pub fn network_allowed(&self) -> bool {
        self.network.allowed()
    }

    /// Whether the OS sandbox should permit filesystem writes for this addon.
    #[must_use]
    pub fn filesystem_writable(&self) -> bool {
        self.filesystem.writable()
    }

    /// Validate the declaration (fail-closed: a malformed declaration is a
    /// manifest error, not a silent grant). Env names must be plausible
    /// `[A-Za-z0-9_]` identifiers.
    pub fn validate(&self) -> Result<(), String> {
        for name in &self.env {
            let n = name.trim();
            if n.is_empty() || !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!(
                    "capabilities.env entry `{name}` is not a valid environment variable name \
                     (use [A-Za-z0-9_])"
                ));
            }
        }
        for bin in self.exec.allowlist() {
            let b = bin.trim();
            if b.is_empty() || b.contains(char::is_whitespace) {
                return Err(format!(
                    "capabilities.exec entry `{bin}` is not a valid binary name or path \
                     (no whitespace, non-empty)"
                ));
            }
        }
        Ok(())
    }

    /// Human-readable lines for the install-consent preview. Always returns the
    /// three dimensions so the user sees exactly what they are granting.
    #[must_use]
    pub fn summary(&self) -> Vec<String> {
        let network = if self.network_allowed() {
            "full (outbound internet)"
        } else {
            "none (egress blocked)"
        };
        let filesystem = if self.filesystem_writable() {
            "read-write"
        } else {
            "read-only (+ scratch tmp)"
        };
        let env = if self.env.is_empty() {
            "scrubbed (base allowlist only)".to_string()
        } else {
            format!("+ {}", self.env.join(", "))
        };
        vec![
            format!("network:    {network}"),
            format!("filesystem: {filesystem}"),
            format!("env:        {env}"),
            format!("exec:       {}", self.exec.label()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_minimal_and_locked_down() {
        let caps = AddonCapabilities::default();
        assert!(caps.is_minimal());
        assert!(!caps.network_allowed());
        assert!(!caps.filesystem_writable());
        assert!(caps.env.is_empty());
        assert!(!caps.exec_allowed());
        assert!(caps.exec_restricted());
    }

    #[test]
    fn parses_declared_block() {
        let caps: AddonCapabilities = toml::from_str(
            "network = \"full\"\nfilesystem = \"read_write\"\nenv = [\"GITHUB_TOKEN\"]\n",
        )
        .expect("parse");
        assert!(caps.network_allowed());
        assert!(caps.filesystem_writable());
        assert_eq!(caps.env, vec!["GITHUB_TOKEN".to_string()]);
        assert!(!caps.is_minimal());
        // exec was omitted → secure-by-default (none).
        assert!(!caps.exec_allowed());
    }

    #[test]
    fn parses_exec_modes_and_allowlist() {
        let full: AddonCapabilities = toml::from_str("exec = \"full\"\n").expect("parse full");
        assert!(full.exec_allowed());
        assert!(!full.exec_restricted());
        assert!(full.exec.allowlist().is_empty());

        let none: AddonCapabilities = toml::from_str("exec = \"none\"\n").expect("parse none");
        assert!(!none.exec_allowed());
        assert!(none.exec_restricted());

        let allow: AddonCapabilities =
            toml::from_str("exec = [\"lean-ctx\", \"git\"]\n").expect("parse allowlist");
        assert!(allow.exec_allowed());
        assert!(allow.exec_restricted());
        assert_eq!(allow.exec.allowlist(), &["lean-ctx", "git"]);
        assert!(!allow.is_minimal());

        let empty: AddonCapabilities = toml::from_str("exec = []\n").expect("parse empty");
        assert!(!empty.exec_allowed(), "empty allowlist == none");
        assert!(empty.exec_restricted());
    }

    #[test]
    fn exec_allowlist_rejects_whitespace_entries() {
        let bad = AddonCapabilities {
            exec: ExecAccess::Allowlist(vec!["ok-bin".into(), "bad bin".into()]),
            ..Default::default()
        };
        assert!(bad.validate().is_err());
        let good = AddonCapabilities {
            exec: ExecAccess::Allowlist(vec!["/usr/bin/git".into(), "lean-ctx".into()]),
            ..Default::default()
        };
        assert!(good.validate().is_ok());
    }

    #[test]
    fn empty_block_resolves_to_strictest() {
        let caps: AddonCapabilities = toml::from_str("").expect("parse");
        assert!(caps.is_minimal());
    }

    #[test]
    fn unknown_enum_value_is_rejected() {
        let err = toml::from_str::<AddonCapabilities>("network = \"halfway\"\n");
        assert!(err.is_err(), "unknown network value must fail-closed");
    }

    #[test]
    fn validate_rejects_bad_env_names() {
        let bad = AddonCapabilities {
            env: vec!["OK_NAME".into(), "bad name".into()],
            ..Default::default()
        };
        assert!(bad.validate().is_err());
        let good = AddonCapabilities {
            env: vec!["GITHUB_TOKEN".into(), "API_KEY_2".into()],
            ..Default::default()
        };
        assert!(good.validate().is_ok());
    }

    #[test]
    fn summary_always_lists_all_dimensions() {
        let s = AddonCapabilities::default().summary();
        assert_eq!(s.len(), 4);
        assert!(s[0].contains("none"));
        assert!(s[1].contains("read-only"));
        assert!(s[2].contains("scrubbed"));
        assert!(s[3].contains("exec") && s[3].contains("none"));

        let elevated = AddonCapabilities {
            network: NetworkAccess::Full,
            filesystem: FilesystemAccess::ReadWrite,
            env: vec!["TOKEN".into()],
            exec: ExecAccess::Allowlist(vec!["lean-ctx".into()]),
        };
        let s = elevated.summary();
        assert!(s[0].contains("full"));
        assert!(s[1].contains("read-write"));
        assert!(s[2].contains("TOKEN"));
        assert!(s[3].contains("lean-ctx"));
    }

    #[test]
    fn as_str_roundtrips() {
        assert_eq!(NetworkAccess::None.as_str(), "none");
        assert_eq!(NetworkAccess::Full.as_str(), "full");
        assert_eq!(FilesystemAccess::ReadOnly.as_str(), "read_only");
        assert_eq!(FilesystemAccess::ReadWrite.as_str(), "read_write");
    }
}
