//! Doctor check for the deprecation register (GL #394).
//!
//! `DEPRECATIONS.toml` (repo root) is compiled into the binary, so the check
//! reports exactly the deprecations that apply to the installed build — per
//! CONTRACTS.md every surface is announced at least 2 minor releases before
//! removal, and `lean-ctx doctor` is the user-facing warning channel.

use super::{BOLD, DIM, GREEN, Outcome, RST, YELLOW};

const REGISTER: &str = include_str!("../../data/DEPRECATIONS.toml");

#[derive(Debug, serde::Deserialize)]
struct Register {
    #[serde(default)]
    deprecation: Vec<Deprecation>,
}

#[derive(Debug, serde::Deserialize)]
struct Deprecation {
    id: String,
    surface: String,
    subject: String,
    announced_in: String,
    earliest_removal: String,
    #[serde(default)]
    replacement: String,
    #[serde(default)]
    #[allow(dead_code)]
    note: String,
}

fn parse_register() -> Result<Register, toml::de::Error> {
    toml::from_str(REGISTER)
}

/// One scored doctor line: green when the shipping build deprecates nothing,
/// yellow with the full list otherwise (each entry names its replacement).
pub(super) fn deprecations_outcome() -> Outcome {
    match parse_register() {
        Ok(reg) if reg.deprecation.is_empty() => Outcome {
            ok: true,
            line: format!(
                "{BOLD}Deprecations{RST}  {GREEN}none active{RST}  {DIM}(register: DEPRECATIONS.toml, policy: CONTRACTS.md){RST}"
            ),
        },
        Ok(reg) => {
            let mut line = format!(
                "{BOLD}Deprecations{RST}  {YELLOW}{} active in this build{RST}",
                reg.deprecation.len()
            );
            for d in &reg.deprecation {
                let replacement = if d.replacement.is_empty() {
                    String::from("no replacement")
                } else {
                    format!("use {}", d.replacement)
                };
                line.push_str(&format!(
                    "\n      {YELLOW}•{RST} [{}] {} {DIM}({}){RST} — announced {}, removal ≥ {} — {DIM}{replacement}{RST}",
                    d.surface, d.subject, d.id, d.announced_in, d.earliest_removal
                ));
            }
            Outcome { ok: false, line }
        }
        Err(e) => Outcome {
            ok: false,
            line: format!("{BOLD}Deprecations{RST}  {YELLOW}register unreadable: {e}{RST}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_register_parses() {
        let reg = parse_register().expect("DEPRECATIONS.toml must stay parseable");
        // Policy invariant: each entry announces a removal at least 2 minor
        // releases ahead and carries a stable id.
        for d in &reg.deprecation {
            assert!(!d.id.is_empty());
            assert!(!d.subject.is_empty());
            let minor = |v: &str| -> Option<(u64, u64)> {
                let mut parts = v.split('.');
                Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
            };
            let (a_major, a_minor) = minor(&d.announced_in).expect("announced_in is semver");
            let (r_major, r_minor) = minor(&d.earliest_removal).expect("earliest_removal semver");
            assert!(
                r_major > a_major || (r_major == a_major && r_minor >= a_minor + 2),
                "{}: earliest_removal must be >= 2 minor releases after announced_in",
                d.id
            );
        }
    }

    #[test]
    fn outcome_is_green_without_active_deprecations() {
        let reg = parse_register().expect("parseable");
        let outcome = deprecations_outcome();
        assert_eq!(outcome.ok, reg.deprecation.is_empty());
    }
}
