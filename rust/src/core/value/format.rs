//! Shared renderers for the value surface. Subtle by construction: one dim
//! line, no exclamation, nothing when there is nothing measured to say.

use crate::core::security_events::SecurityCounts;
use crate::core::wrapped::format_tokens;

use super::snapshot::ValueSnapshot;

/// How a channel can render: colour (ANSI dim) and Unicode glyphs are
/// independent — a Claude status line has colour but no TTY, a legacy Windows
/// console has a TTY but no reliable glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub color: bool,
    pub unicode: bool,
}

impl Style {
    pub const PLAIN: Self = Self {
        color: false,
        unicode: true,
    };

    /// Colour unless `NO_COLOR` is set; ASCII when `LEAN_CTX_ASCII` is set.
    /// TTY detection is the caller's job (status lines are not TTYs).
    pub fn from_env() -> Self {
        Self {
            color: std::env::var_os("NO_COLOR").is_none(),
            unicode: std::env::var_os("LEAN_CTX_ASCII").is_none(),
        }
    }

    pub(crate) fn mark(self) -> &'static str {
        if self.unicode { "◆" } else { "*" }
    }

    pub(crate) fn shield(self) -> &'static str {
        if self.unicode { "⛨" } else { "sec" }
    }

    pub(crate) fn minus(self) -> &'static str {
        if self.unicode { "−" } else { "-" }
    }

    pub(crate) fn sep(self) -> &'static str {
        if self.unicode { " · " } else { " | " }
    }
}

/// `◆ lean-ctx −1.2M tok · 41 cached · ⛨ 3` — or `None` when nothing measured
/// happened yet (a zero would read like lean-ctx is not working).
pub fn one_line(snap: &ValueSnapshot, style: Style) -> Option<String> {
    if snap.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if snap.tokens_saved > 0 {
        parts.push(format!(
            "{}{} tok",
            style.minus(),
            format_tokens(snap.tokens_saved)
        ));
    }
    if snap.cache_hits > 0 {
        parts.push(format!("{} cached", snap.cache_hits));
    }
    let security = snap.security.total();
    if security > 0 {
        parts.push(format!("{} {security}", style.shield()));
    }
    let body = format!("{} lean-ctx {}", style.mark(), parts.join(style.sep()));
    Some(if style.color {
        format!("\x1b[2m{body}\x1b[0m")
    } else {
        body
    })
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Human wording for a security tally, in the contract's words: secrets are
/// *kept out of context*, commands and paths *blocked*, injections *flagged*.
pub fn security_phrases(counts: &SecurityCounts) -> Vec<String> {
    let mut out = Vec::new();
    if counts.secrets_redacted > 0 {
        out.push(format!(
            "{} kept out of context",
            plural(counts.secrets_redacted, "secret", "secrets")
        ));
    }
    if counts.shell_blocked > 0 {
        out.push(format!(
            "{} blocked",
            plural(counts.shell_blocked, "risky command", "risky commands")
        ));
    }
    if counts.path_blocked > 0 {
        out.push(format!(
            "{} outside the project blocked",
            plural(counts.path_blocked, "path", "paths")
        ));
    }
    if counts.injection_flagged > 0 {
        out.push(format!(
            "{} flagged",
            plural(
                counts.injection_flagged,
                "prompt-injection pattern",
                "prompt-injection patterns"
            )
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(saved: u64, cache: u64, secrets: u64) -> ValueSnapshot {
        let mut s = ValueSnapshot {
            tokens_saved: saved,
            tokens_input: saved * 2,
            cache_hits: cache,
            ..ValueSnapshot::default()
        };
        s.security.secrets_redacted = secrets;
        s
    }

    #[test]
    fn nothing_measured_renders_nothing() {
        assert_eq!(one_line(&ValueSnapshot::default(), Style::PLAIN), None);
    }

    #[test]
    fn plain_unicode_line() {
        assert_eq!(
            one_line(&snap(1_200_000, 41, 3), Style::PLAIN).unwrap(),
            "◆ lean-ctx −1.2M tok · 41 cached · ⛨ 3"
        );
    }

    #[test]
    fn ascii_fallback_has_no_multibyte_glyphs() {
        let style = Style {
            color: false,
            unicode: false,
        };
        let line = one_line(&snap(312_000, 0, 1), style).unwrap();
        assert!(line.is_ascii(), "{line}");
        assert_eq!(line, "* lean-ctx -312.0K tok | sec 1");
    }

    #[test]
    fn color_is_dim_and_reset() {
        let style = Style {
            color: true,
            unicode: true,
        };
        let line = one_line(&snap(5_000, 0, 0), style).unwrap();
        assert!(line.starts_with("\x1b[2m") && line.ends_with("\x1b[0m"));
    }

    #[test]
    fn security_only_session_still_shows() {
        let line = one_line(&snap(0, 0, 2), Style::PLAIN).unwrap();
        assert_eq!(line, "◆ lean-ctx ⛨ 2");
    }

    #[test]
    fn phrases_never_claim_neutralization() {
        let counts = SecurityCounts {
            secrets_redacted: 1,
            shell_blocked: 2,
            path_blocked: 1,
            injection_flagged: 3,
        };
        let text = security_phrases(&counts).join(", ");
        assert_eq!(
            text,
            "1 secret kept out of context, 2 risky commands blocked, \
             1 path outside the project blocked, 3 prompt-injection patterns flagged"
        );
        assert!(!text.contains("neutraliz"));
    }
}
