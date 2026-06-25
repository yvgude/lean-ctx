//! Trust tiers + static risk assessment for addons (#864).
//!
//! Two orthogonal questions about an addon:
//!
//! 1. **Trust tier** — *who vouches for it?* [`TrustTier`] is conferred by the
//!    curated registry (`addon.verified`), never by the entry claiming it.
//! 2. **Risk** — *what does its wiring actually do?* [`assess`] statically
//!    inspects the `[mcp]` block for signals that warrant a louder warning
//!    (remote endpoints, shelling out, unpinned upstreams, secret-bearing env).
//!
//! Both are pure + deterministic so the CLI preview, the registry validator and
//! the install policy gate all read from one source of truth.

use super::manifest::AddonManifest;
use crate::core::gateway::TransportKind;

/// How much an addon is trusted — set by the registry it ships in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustTier {
    /// Audited and vouched for by maintainers (`addon.verified = true`).
    Verified,
    /// Community-submitted: installable, but unaudited. The default.
    Community,
}

impl TrustTier {
    /// The tier the registry confers on this entry.
    #[must_use]
    pub fn of(manifest: &AddonManifest) -> Self {
        if manifest.addon.verified {
            Self::Verified
        } else {
            Self::Community
        }
    }

    /// Lower-case label for CLI / website (`verified` / `community`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Community => "community",
        }
    }
}

/// Severity of a [`RiskFinding`], ordered `Info < Warn < Danger`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    /// Worth disclosing, not alarming (e.g. passes env vars).
    Info,
    /// Deserves a second look before installing (e.g. unpinned upstream).
    Warn,
    /// High-impact capability (e.g. shells out, remote endpoint).
    Danger,
}

impl RiskLevel {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Danger => "danger",
        }
    }

    /// A glyph for the CLI preview.
    #[must_use]
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Info => "•",
            Self::Warn => "⚠",
            Self::Danger => "⛔",
        }
    }
}

/// One observation about an addon's wiring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskFinding {
    pub level: RiskLevel,
    /// Stable machine code (for tests / the validator), e.g. `"shell_exec"`.
    pub code: &'static str,
    pub message: String,
}

impl RiskFinding {
    fn new(level: RiskLevel, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            level,
            code,
            message: message.into(),
        }
    }

    /// Construct a finding from a sibling auditor (the capability/malware audit
    /// in [`super::audit`]) so all findings share one type + rendering.
    #[must_use]
    pub fn audit(level: RiskLevel, code: &'static str, message: impl Into<String>) -> Self {
        Self::new(level, code, message)
    }
}

/// Executables that hand an addon an arbitrary-code primitive.
const SHELL_BINS: &[&str] = &["sh", "bash", "zsh", "dash", "fish", "ksh"];
/// Fetch-and-run / eval primitives worth flagging when used as the command.
const FETCH_BINS: &[&str] = &["curl", "wget", "eval"];
/// Package runners that execute remote code; risky when unpinned.
const RUNNER_BINS: &[&str] = &["npx", "uvx", "pipx", "bunx", "pnpx"];

fn basename(cmd: &str) -> &str {
    cmd.rsplit(['/', '\\']).next().unwrap_or(cmd)
}

/// Statically inspect an addon's `[mcp]` wiring. Pure + deterministic; the
/// returned findings are sorted by descending severity then code so output is
/// byte-stable (provider prompt-cache friendly, #498).
#[must_use]
pub fn assess(manifest: &AddonManifest) -> Vec<RiskFinding> {
    let mcp = &manifest.mcp;
    let mut out: Vec<RiskFinding> = Vec::new();

    match mcp.transport {
        TransportKind::Http => {
            let host = host_of(&mcp.url);
            out.push(RiskFinding::new(
                RiskLevel::Danger,
                "remote_endpoint",
                format!("HTTP transport — your context is sent to a remote endpoint ({host})."),
            ));
            if !mcp.url.trim().starts_with("https://") {
                out.push(RiskFinding::new(
                    RiskLevel::Danger,
                    "insecure_url",
                    "Endpoint is not HTTPS — traffic is unencrypted.",
                ));
            }
            if !mcp.headers.is_empty() {
                out.push(RiskFinding::new(
                    RiskLevel::Warn,
                    "request_headers",
                    format!(
                        "Sends request headers that may carry credentials: {}.",
                        keys(mcp.headers.keys())
                    ),
                ));
            }
        }
        TransportKind::Stdio => {
            let base = basename(mcp.command.trim());
            if SHELL_BINS.contains(&base) && mcp.args.iter().any(|a| a == "-c") {
                out.push(RiskFinding::new(
                    RiskLevel::Danger,
                    "shell_exec",
                    format!("Runs an inline shell (`{base} -c …`) — arbitrary command execution."),
                ));
            } else if FETCH_BINS.contains(&base) {
                out.push(RiskFinding::new(
                    RiskLevel::Danger,
                    "fetch_exec",
                    format!("Command is `{base}` — fetches/executes external content at startup."),
                ));
            }

            // Shell metacharacters anywhere in args → command injection surface.
            if mcp.args.iter().any(|a| has_shell_meta(a)) {
                out.push(RiskFinding::new(
                    RiskLevel::Warn,
                    "shell_meta",
                    "Arguments contain shell metacharacters (| ; & $ ` > <).",
                ));
            }

            // Unpinned package runner → upstream can change under you.
            if RUNNER_BINS.contains(&base) && !is_pinned(&mcp.args) {
                out.push(RiskFinding::new(
                    RiskLevel::Warn,
                    "unpinned",
                    format!(
                        "`{base}` without a pinned version — upstream code can change silently."
                    ),
                ));
            }
            if mcp.args.iter().any(|a| mentions_latest(a)) {
                out.push(RiskFinding::new(
                    RiskLevel::Warn,
                    "unpinned",
                    "Targets a `latest`/unpinned tag — pin an exact version instead.",
                ));
            }

            if !mcp.env.is_empty() {
                out.push(RiskFinding::new(
                    RiskLevel::Info,
                    "child_env",
                    format!(
                        "Passes environment variables to the child: {}.",
                        keys(mcp.env.keys())
                    ),
                ));
            }
        }
    }

    out.sort_by(|a, b| b.level.cmp(&a.level).then_with(|| a.code.cmp(b.code)));
    out.dedup();
    out
}

/// The highest severity among `findings`, if any.
#[must_use]
pub fn max_level(findings: &[RiskFinding]) -> Option<RiskLevel> {
    findings.iter().map(|f| f.level).max()
}

/// Whether the wiring inherently performs outbound network I/O — an HTTP
/// transport, or a stdio command that fetches/executes remote code or runs an
/// unpinned package from a remote registry. The capability audit
/// ([`super::audit`]) uses this to flag an addon that declares `network = none`
/// but actually needs the network (an under-declared capability).
#[must_use]
pub fn wiring_uses_network(manifest: &AddonManifest) -> bool {
    match manifest.mcp.transport {
        TransportKind::Http => true,
        TransportKind::Stdio => {
            let base = basename(manifest.mcp.command.trim());
            FETCH_BINS.contains(&base) || RUNNER_BINS.contains(&base)
        }
    }
}

/// Whether the wiring evidences spawning child processes — the command is a
/// shell run with `-c`, a fetch/eval primitive, or any argument carries shell
/// metacharacters that chain to another program. The capability audit
/// ([`super::audit`]) uses this to flag an addon that declares no `exec`
/// permission yet clearly shells out (an under-declared capability). HTTP
/// addons run no local child, so this is stdio-only.
#[must_use]
pub fn wiring_spawns_subprocess(manifest: &AddonManifest) -> bool {
    if manifest.mcp.transport != TransportKind::Stdio {
        return false;
    }
    let base = basename(manifest.mcp.command.trim());
    let shell_with_c = SHELL_BINS.contains(&base) && manifest.mcp.args.iter().any(|a| a == "-c");
    shell_with_c
        || FETCH_BINS.contains(&base)
        || manifest.mcp.args.iter().any(|a| has_shell_meta(a))
}

fn keys<'a>(it: impl Iterator<Item = &'a String>) -> String {
    let mut v: Vec<&str> = it.map(String::as_str).collect();
    v.sort_unstable();
    v.join(", ")
}

fn host_of(url: &str) -> String {
    url.trim()
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_string()
}

fn has_shell_meta(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '|' | ';' | '&' | '`' | '>' | '<'))
        || s.contains("$(")
}

fn mentions_latest(arg: &str) -> bool {
    let a = arg.to_ascii_lowercase();
    a == "latest" || a.ends_with("@latest") || a.ends_with(":latest")
}

/// A package-runner invocation is "pinned" when some positional arg carries an
/// explicit version (`pkg@1.2.3`, `pkg==1.2.3`, `pkg:1.2.3`).
fn is_pinned(args: &[String]) -> bool {
    args.iter().filter(|a| !a.starts_with('-')).any(|a| {
        let body = a.rsplit('/').next().unwrap_or(a);
        body.contains("==") || body.contains(':') || (body.contains('@') && !body.starts_with('@'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(toml: &str) -> AddonManifest {
        AddonManifest::from_toml(toml).expect("parse")
    }

    #[test]
    fn trust_tier_from_registry_flag() {
        let community = manifest("[addon]\nname = \"a\"\n");
        assert_eq!(TrustTier::of(&community), TrustTier::Community);
        let verified = manifest("[addon]\nname = \"a\"\nverified = true\n");
        assert_eq!(TrustTier::of(&verified), TrustTier::Verified);
        assert_eq!(TrustTier::Verified.label(), "verified");
    }

    #[test]
    fn clean_stdio_addon_has_no_danger() {
        let m = manifest(
            "[addon]\nname = \"ok\"\n[mcp]\ntransport = \"stdio\"\ncommand = \"my-mcp\"\nargs = [\"serve\"]\n",
        );
        let f = assess(&m);
        assert_eq!(max_level(&f), None, "clean addon → no findings");
    }

    #[test]
    fn http_is_danger_and_flags_headers() {
        let m = manifest(
            "[addon]\nname = \"r\"\n[mcp]\ntransport = \"http\"\nurl = \"https://x.example/mcp\"\n[mcp.headers]\nAuthorization = \"Bearer x\"\n",
        );
        let f = assess(&m);
        assert_eq!(max_level(&f), Some(RiskLevel::Danger));
        assert!(f.iter().any(|x| x.code == "remote_endpoint"));
        assert!(f.iter().any(|x| x.code == "request_headers"));
    }

    #[test]
    fn http_non_https_is_insecure() {
        let m = manifest(
            "[addon]\nname = \"r\"\n[mcp]\ntransport = \"http\"\nurl = \"http://x.example/mcp\"\n",
        );
        assert!(assess(&m).iter().any(|x| x.code == "insecure_url"));
    }

    #[test]
    fn shell_exec_is_danger() {
        let m = manifest(
            "[addon]\nname = \"s\"\n[mcp]\ntransport = \"stdio\"\ncommand = \"/bin/bash\"\nargs = [\"-c\", \"do-thing\"]\n",
        );
        assert!(
            assess(&m)
                .iter()
                .any(|x| x.code == "shell_exec" && x.level == RiskLevel::Danger)
        );
    }

    #[test]
    fn unpinned_runner_warns_pinned_does_not() {
        let unpinned = manifest(
            "[addon]\nname = \"u\"\n[mcp]\ntransport = \"stdio\"\ncommand = \"uvx\"\nargs = [\"some-pkg\"]\n",
        );
        assert!(assess(&unpinned).iter().any(|x| x.code == "unpinned"));

        let pinned = manifest(
            "[addon]\nname = \"p\"\n[mcp]\ntransport = \"stdio\"\ncommand = \"uvx\"\nargs = [\"some-pkg==1.2.3\"]\n",
        );
        assert!(!assess(&pinned).iter().any(|x| x.code == "unpinned"));
    }

    #[test]
    fn latest_tag_warns() {
        let m = manifest(
            "[addon]\nname = \"l\"\n[mcp]\ntransport = \"stdio\"\ncommand = \"npx\"\nargs = [\"pkg@latest\"]\n",
        );
        assert!(assess(&m).iter().any(|x| x.code == "unpinned"));
    }

    #[test]
    fn env_is_info() {
        let m = manifest(
            "[addon]\nname = \"e\"\n[mcp]\ntransport = \"stdio\"\ncommand = \"x\"\n[mcp.env]\nTOKEN = \"y\"\n",
        );
        let f = assess(&m);
        assert_eq!(max_level(&f), Some(RiskLevel::Info));
        assert!(f.iter().any(|x| x.code == "child_env"));
    }

    #[test]
    fn findings_are_severity_sorted() {
        let m = manifest(
            "[addon]\nname = \"m\"\n[mcp]\ntransport = \"http\"\nurl = \"http://x\"\n[mcp.headers]\nA = \"b\"\n",
        );
        let f = assess(&m);
        for w in f.windows(2) {
            assert!(w[0].level >= w[1].level, "sorted by descending severity");
        }
    }
}
