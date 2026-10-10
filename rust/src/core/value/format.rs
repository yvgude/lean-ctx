// SPDX-License-Identifier: Apache-2.0
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

/// LeanCTX colours (leanctx.com, dark theme) as 24-bit SGR parameters.
const BRAND_PILL: &str = "1;38;2;17;22;38;48;2;102;140;255"; // canvas on signature
const BRAND_LABEL: &str = "1;38;2;102;140;255"; // signature
const BRAND_VALUE: &str = "1;38;2;241;243;255"; // foreground
const BRAND_DETAIL: &str = "38;2;175;184;208"; // secondary
const BRAND_RULE: &str = "38;2;53;62;90"; // hairline

fn paint(style: Style, sgr: &str, text: &str) -> String {
    if style.color {
        format!("\x1b[{sgr}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

/// The Claude Code status line in the Context Gateway's terms:
/// `◆ LeanCTX │ SELECT 12 files · 30 commands │ CONTROL ⛨ 3 enforced │ TOKENS −1.2M kept out · 60% leaner · 41 cached`.
/// SELECT is what reached Claude through the gateway, CONTROL what the rules
/// enforced before the handoff, TOKENS what stayed out of context. A group with
/// nothing measured is left out, and so is the line when all are.
pub fn brand_line(snap: &ValueSnapshot, style: Style) -> Option<String> {
    let sources = snap.files_read + snap.commands_run;
    if snap.is_empty() && sources == 0 {
        return None;
    }
    let sep = style.sep();
    let rule = if style.unicode { " │ " } else { " | " };
    let group = |label: &str, value: String, details: &[String]| {
        let mut out = format!(
            "{} {}",
            paint(style, BRAND_LABEL, label),
            paint(style, BRAND_VALUE, &value)
        );
        for detail in details {
            out.push_str(&paint(style, BRAND_DETAIL, &format!("{sep}{detail}")));
        }
        out
    };
    let mut groups = Vec::new();
    if sources > 0 {
        let mut parts = Vec::new();
        if snap.files_read > 0 {
            parts.push(plural(snap.files_read, "file", "files"));
        }
        if snap.commands_run > 0 {
            parts.push(plural(snap.commands_run, "command", "commands"));
        }
        let (value, details) = parts.split_first().expect("sources > 0 names one part");
        groups.push(group("SELECT", value.clone(), details));
    }
    let security = snap.security.total();
    if security > 0 {
        groups.push(group(
            "CONTROL",
            format!("{} {security} enforced", style.shield()),
            &[],
        ));
    }
    if snap.tokens_saved > 0 {
        let mut details = Vec::new();
        if let Some(pct) = snap.saved_pct() {
            details.push(format!("{pct:.0}% leaner"));
        }
        if snap.cache_hits > 0 {
            details.push(format!("{} cached", snap.cache_hits));
        }
        let value = format!(
            "{}{} kept out",
            style.minus(),
            format_tokens(snap.tokens_saved)
        );
        groups.push(group("TOKENS", value, &details));
    }
    let brand = format!("{} LeanCTX", style.mark());
    let lead = if style.color {
        paint(style, BRAND_PILL, &format!(" {brand} "))
    } else {
        brand
    };
    let rule = paint(style, BRAND_RULE, rule);
    Some(format!("{lead}{rule}{}", groups.join(&rule)))
}

/// `◆ −1.2M tok ⛨ 3` for a shell prompt: the shortest honest form, never
/// coloured (prompt escaping differs per shell, so the caller wraps it).
pub fn compact(snap: &ValueSnapshot, style: Style) -> Option<String> {
    if snap.is_empty() {
        return None;
    }
    let mut out = style.mark().to_string();
    if snap.tokens_saved > 0 {
        out.push_str(&format!(
            " {}{} tok",
            style.minus(),
            format_tokens(snap.tokens_saved)
        ));
    }
    let security = snap.security.total();
    if security > 0 {
        out.push_str(&format!(" {} {security}", style.shield()));
    }
    // Only cache hits measured: nothing short enough to be worth a prompt slot.
    (out.len() > style.mark().len()).then_some(out)
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
    if counts.pii_redacted > 0 {
        out.push(format!(
            "{} kept out of context",
            plural(
                counts.pii_redacted,
                "personal data value",
                "personal data values"
            )
        ));
    }
    if counts.content_withheld > 0 {
        out.push(format!(
            "{} withheld",
            plural(counts.content_withheld, "source", "sources")
        ));
    }
    if counts.coverage_incomplete > 0 {
        out.push(format!(
            "{} not fully inspected",
            plural(counts.coverage_incomplete, "source", "sources")
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
    fn compact_is_short_and_uncoloured() {
        let style = Style {
            color: true,
            unicode: true,
        };
        assert_eq!(
            compact(&snap(1_200_000, 41, 3), style).unwrap(),
            "◆ −1.2M tok ⛨ 3"
        );
        let ascii = Style {
            color: false,
            unicode: false,
        };
        assert_eq!(compact(&snap(0, 0, 2), ascii).unwrap(), "* sec 2");
        assert_eq!(compact(&snap(0, 9, 0), Style::PLAIN), None);
        assert_eq!(compact(&ValueSnapshot::default(), Style::PLAIN), None);
    }

    #[test]
    fn brand_line_groups_select_control_and_tokens() {
        let mut full = snap(1_200_000, 41, 3);
        full.files_read = 12;
        full.commands_run = 30;
        assert_eq!(
            brand_line(&full, Style::PLAIN).unwrap(),
            "◆ LeanCTX │ SELECT 12 files · 30 commands │ CONTROL ⛨ 3 enforced │ TOKENS −1.2M kept out · 50% leaner · 41 cached"
        );
        assert_eq!(
            brand_line(&snap(0, 0, 2), Style::PLAIN).unwrap(),
            "◆ LeanCTX │ CONTROL ⛨ 2 enforced"
        );
        // Sources read with nothing kept out still say what reached Claude.
        let read_only = ValueSnapshot {
            files_read: 1,
            ..ValueSnapshot::default()
        };
        assert_eq!(
            brand_line(&read_only, Style::PLAIN).unwrap(),
            "◆ LeanCTX │ SELECT 1 file"
        );
        assert_eq!(brand_line(&ValueSnapshot::default(), Style::PLAIN), None);
    }

    #[test]
    fn brand_line_colour_and_ascii_fallbacks() {
        let colour = Style {
            color: true,
            unicode: true,
        };
        let line = brand_line(&snap(5_000, 0, 0), colour).unwrap();
        assert!(line.starts_with("\x1b[1;38;2;17;22;38;48;2;102;140;255m ◆ LeanCTX \x1b[0m"));
        assert!(line.ends_with("\x1b[0m"));
        let ascii = Style {
            color: false,
            unicode: false,
        };
        let line = brand_line(&snap(312_000, 2, 1), ascii).unwrap();
        assert!(line.is_ascii(), "{line}");
        assert_eq!(
            line,
            "* LeanCTX | CONTROL sec 1 enforced | TOKENS -312.0K kept out | 50% leaner | 2 cached"
        );
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
            ..SecurityCounts::ZERO
        };
        let text = security_phrases(&counts).join(", ");
        assert_eq!(
            text,
            "1 secret kept out of context, 2 risky commands blocked, \
             1 path outside the project blocked, 3 prompt-injection patterns flagged"
        );
        assert!(!text.contains("neutraliz"));
    }

    #[test]
    fn gateway_phrases_say_what_happened_without_overclaiming() {
        let counts = SecurityCounts {
            pii_redacted: 2,
            content_withheld: 1,
            coverage_incomplete: 1,
            ..SecurityCounts::ZERO
        };
        assert_eq!(
            security_phrases(&counts).join(", "),
            "2 personal data values kept out of context, 1 source withheld, \
             1 source not fully inspected"
        );
    }
}
