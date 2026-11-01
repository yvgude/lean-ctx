// SPDX-License-Identifier: Apache-2.0
//! The one mandatory path into the Hive outbox for content.
//!
//! A text that carries a credential, personal data (email, phone, card,
//! IBAN, national ids), anything the redaction rules would mask, a pattern
//! the active egress policy forbids, or a local home path is dropped, never
//! rewritten and sent. Every drop is counted for `engine_screening`.

use lean_ctx_protocol::hive::knowledge::EngineScreeningV1;

/// Bumped whenever the rules below change meaning.
pub const SCANNER_VERSION: u32 = 1;

/// Why a text was dropped. Never carries the text itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    Secret(String),
    Pii(String),
    Redaction,
    Forbidden,
    LocalPath,
    /// A forbidden pattern of this installation does not compile: nothing
    /// passes until it is fixed (fails closed, never silently unscreened).
    BrokenRules(usize),
}

impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Secret(kind) => write!(f, "credential ({kind})"),
            Self::Pii(kind) => write!(f, "personal data ({kind})"),
            Self::Redaction => f.write_str("matches a redaction rule"),
            Self::Forbidden => f.write_str("matches a forbidden egress pattern or org filter"),
            Self::LocalPath => f.write_str("contains a local home path"),
            Self::BrokenRules(n) => write!(
                f,
                "cannot be screened: {n} egress forbidden pattern(s) of this machine do not compile, so nothing goes to Hive until they are fixed (`lean-ctx policy` / `[egress] forbidden_patterns`)"
            ),
        }
    }
}

/// Screening rules with their configuration resolved once.
pub struct Screen {
    custom_secret_patterns: Vec<String>,
    redaction_excludes: std::sync::Arc<Vec<regex::Regex>>,
    forbidden: Vec<regex::Regex>,
    /// Forbidden patterns that did not compile: every text is refused.
    broken: usize,
    home: Option<String>,
    counts: EngineScreeningV1,
}

impl Screen {
    /// Rules of this installation (custom secret patterns, redaction
    /// excludes, the active pack's `[egress] forbidden_patterns`). A pattern
    /// that does not compile fails closed: the screen then refuses every
    /// text ([`Blocked::BrokenRules`], [`Screen::problem`]).
    pub fn for_installation() -> Self {
        let config = crate::core::config::Config::load();
        // The organization's Hive filter packs (redact and block rules
        // alike) keep a matching fact on this machine.
        let hive = crate::core::hive::policy::installed_filter_rules();
        let forbidden = crate::core::policy::runtime::active()
            .map(|policy| policy.resolved.egress.forbidden_patterns.clone())
            .unwrap_or_default()
            .into_iter()
            .chain(hive.redaction.into_iter().map(|(_, pattern)| pattern))
            .chain(hive.blocked.into_iter().map(|(_, pattern)| pattern))
            .map(|p| regex::Regex::new(&p))
            .collect();
        Self::with_compiled(
            config.secret_detection.custom_patterns.clone(),
            crate::core::redaction::config_exclude_patterns(),
            forbidden,
        )
    }

    /// [`Self::with_rules`] from compile results: an error counts as a
    /// broken rule, never as no rule.
    pub(crate) fn with_compiled(
        custom_secret_patterns: Vec<String>,
        redaction_excludes: std::sync::Arc<Vec<regex::Regex>>,
        compiled: Vec<Result<regex::Regex, regex::Error>>,
    ) -> Self {
        let total = compiled.len();
        let forbidden: Vec<regex::Regex> = compiled.into_iter().filter_map(Result::ok).collect();
        let broken = total - forbidden.len();
        if broken > 0 {
            tracing::warn!("hive screening: {broken} forbidden pattern(s) do not compile");
        }
        let mut screen = Self::with_rules(custom_secret_patterns, redaction_excludes, forbidden);
        screen.broken = broken;
        screen
    }

    /// Why this screen lets nothing pass (a broken rule), for status lines
    /// and the callers that stop all Hive content traffic on it.
    pub fn problem(&self) -> Option<String> {
        (self.broken > 0).then(|| {
            format!(
                "Hive content traffic is paused: {}",
                Blocked::BrokenRules(self.broken)
            )
        })
    }

    pub(crate) fn with_rules(
        custom_secret_patterns: Vec<String>,
        redaction_excludes: std::sync::Arc<Vec<regex::Regex>>,
        forbidden: Vec<regex::Regex>,
    ) -> Self {
        let home = dirs::home_dir()
            .map(|h| h.to_string_lossy().into_owned())
            .filter(|h| h.len() > 1);
        Self {
            custom_secret_patterns,
            redaction_excludes,
            forbidden,
            broken: 0,
            home,
            counts: EngineScreeningV1 {
                scanner_version: SCANNER_VERSION,
                secrets_removed: 0,
                pii_removed: 0,
                items_dropped: 0,
            },
        }
    }

    /// `Ok(())` when `text` may leave the machine; otherwise the reason, and
    /// the drop is counted.
    pub fn check(&mut self, text: &str) -> Result<(), Blocked> {
        let verdict = self.verdict(text);
        if let Err(reason) = &verdict {
            self.counts.items_dropped += 1;
            match reason {
                Blocked::Secret(_) | Blocked::Redaction => self.counts.secrets_removed += 1,
                Blocked::Pii(_) => self.counts.pii_removed += 1,
                Blocked::Forbidden | Blocked::LocalPath | Blocked::BrokenRules(_) => {}
            }
        }
        verdict
    }

    fn verdict(&self, text: &str) -> Result<(), Blocked> {
        if self.broken > 0 {
            return Err(Blocked::BrokenRules(self.broken));
        }
        if text.contains("[REDACTED") {
            return Err(Blocked::Secret("masked in store".into()));
        }
        let findings = crate::core::secret_detection::detect_secrets_with_custom(
            text,
            &self.custom_secret_patterns,
        );
        if let Some(first) = findings.first() {
            return Err(Blocked::Secret(first.pattern_name.to_string()));
        }
        if crate::core::redaction::redact_text_with_excludes(text, &self.redaction_excludes) != text
        {
            return Err(Blocked::Redaction);
        }
        if let Some((class, _)) = crate::core::input_filters::pii::detect(text).first() {
            return Err(Blocked::Pii((*class).to_string()));
        }
        if crate::core::sensitivity::classify_content(text)
            >= crate::core::sensitivity::SensitivityLevel::Confidential
        {
            return Err(Blocked::Pii("card or account number".into()));
        }
        if self.forbidden.iter().any(|re| re.is_match(text)) {
            return Err(Blocked::Forbidden);
        }
        if let Some(home) = &self.home
            && text.contains(home.as_str())
        {
            return Err(Blocked::LocalPath);
        }
        Ok(())
    }

    /// The counts for `engine_screening`.
    pub fn counts(&self) -> EngineScreeningV1 {
        self.counts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Screen {
        Screen::with_rules(Vec::new(), std::sync::Arc::new(Vec::new()), Vec::new())
    }

    #[test]
    fn canary_secrets_and_personal_data_never_pass() {
        let mut s = screen();
        for canary in [
            concat!("deploy with AK", "IAIOSFODNN7EXAMPLE as key"),
            concat!("token gh", "p_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789"),
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEow",
            "mail jane.doe@example.com for access",
            "card 4111 1111 1111 1111",
        ] {
            assert!(s.check(canary).is_err(), "{canary}");
        }
        assert!(s.check("Hive never blocks a tool call.").is_ok());
        let counts = s.counts();
        assert_eq!(counts.items_dropped, 5);
        assert!(counts.secrets_removed >= 3 && counts.pii_removed >= 1);
    }

    #[test]
    fn forbidden_patterns_and_home_paths_are_dropped() {
        let mut s = Screen::with_rules(
            Vec::new(),
            std::sync::Arc::new(Vec::new()),
            vec![regex::Regex::new("project-falcon").unwrap()],
        );
        assert_eq!(s.check("codename project-falcon"), Err(Blocked::Forbidden));
        if let Some(home) = dirs::home_dir().filter(|h| h.to_string_lossy().len() > 1) {
            let text = format!("see {}/notes.txt", home.display());
            assert_eq!(s.check(&text), Err(Blocked::LocalPath));
        }
    }

    /// An invalid forbidden pattern never stops blocking silently: the
    /// screen then lets nothing pass and says why.
    #[test]
    #[allow(clippy::invalid_regex)] // the broken rule is the point of the test
    fn an_invalid_forbidden_pattern_fails_closed() {
        let mut s = Screen::with_compiled(
            Vec::new(),
            std::sync::Arc::new(Vec::new()),
            vec![regex::Regex::new("falcon"), regex::Regex::new("(unclosed")],
        );
        assert_eq!(
            s.check("Hive never blocks a tool call."),
            Err(Blocked::BrokenRules(1))
        );
        let problem = s.problem().expect("named");
        assert!(problem.contains("do not compile"), "{problem}");
        let fine = Screen::with_compiled(
            Vec::new(),
            std::sync::Arc::new(Vec::new()),
            vec![regex::Regex::new("falcon")],
        );
        assert_eq!(fine.problem(), None);
    }
}
