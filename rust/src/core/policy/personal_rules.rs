// SPDX-License-Identifier: Apache-2.0
//! Personal mask/block rule compiler. It turns a bounded declarative rule set
//! into ordinary redaction and blocked-pattern entries of the project policy,
//! locally and without an account, licence or runtime. Field selectors match
//! literal labels in single-line JSON, key=value or colon text; they are not a
//! semantic decoder for escaped keys, XML or multiline values.
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// Largest number of rules one personal rule set may contain.
pub const MAX_RULES: usize = 32;
const MAX_INPUT_BYTES: usize = 128 * 1024;
const MAX_ID_BYTES: usize = 48;
const MAX_SOURCE_PATTERN_BYTES: usize = 2048;
const MAX_PATTERN_BYTES: usize = 4096;
const MAX_FIELD_NAMES: usize = 8;
const MAX_FIELD_NAME_BYTES: usize = 64;
const REGEX_SIZE_LIMIT: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    schema_version: u32,
    rules: Vec<Rule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    id: String,
    action: Action,
    matcher: Matcher,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Mask,
    Block,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Matcher {
    Pattern { pattern: String },
    Field { names: Vec<String> },
}

/// A compiled personal rule set: rule id to bounded, non-empty-matching regex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    pub rule_count: usize,
    pub redaction: BTreeMap<String, String>,
    pub blocked_patterns: BTreeMap<String, String>,
}

/// The only refusal. It never echoes input: rule ids, field names and
/// patterns may themselves contain sensitive customer information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid personal protection rules")]
pub struct Invalid;

/// Compile a personal rule set (`{"schema_version":1,"rules":[...]}`).
pub fn compile(raw: &str) -> Result<Compiled, Invalid> {
    if raw.len() > MAX_INPUT_BYTES {
        return Err(Invalid);
    }
    // Never surface serde's contextual errors (see `Invalid`).
    let input: Input = serde_json::from_str(raw).map_err(|_| Invalid)?;
    if input.schema_version != 1 || input.rules.is_empty() || input.rules.len() > MAX_RULES {
        return Err(Invalid);
    }
    let mut compiled = Compiled {
        rule_count: input.rules.len(),
        redaction: BTreeMap::new(),
        blocked_patterns: BTreeMap::new(),
    };
    let mut ids = BTreeSet::new();
    for rule in input.rules {
        if !valid_id(&rule.id) || !ids.insert(rule.id.clone()) {
            return Err(Invalid);
        }
        let pattern = match rule.matcher {
            Matcher::Pattern { pattern } if pattern.len() <= MAX_SOURCE_PATTERN_BYTES => pattern,
            Matcher::Pattern { .. } => return Err(Invalid),
            Matcher::Field { names } => field_pattern(&names)?,
        };
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err(Invalid);
        }
        let regex = regex::RegexBuilder::new(&pattern)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map_err(|_| Invalid)?;
        if regex.is_match("") {
            return Err(Invalid);
        }
        match rule.action {
            Action::Mask => compiled.redaction.insert(rule.id, pattern),
            Action::Block => compiled.blocked_patterns.insert(rule.id, pattern),
        };
    }
    Ok(compiled)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
}

fn field_pattern(names: &[String]) -> Result<String, Invalid> {
    if names.is_empty() || names.len() > MAX_FIELD_NAMES {
        return Err(Invalid);
    }
    let mut unique = BTreeSet::new();
    for name in names {
        if name.is_empty()
            || name.len() > MAX_FIELD_NAME_BYTES
            || name.trim() != name
            || name
                .chars()
                .any(|c| c.is_control() || "\"'\\:=,{}[];".contains(c))
            || !unique.insert(name.as_str())
        {
            return Err(Invalid);
        }
    }
    let alternatives = unique
        .iter()
        .map(|s| regex::escape(s))
        .collect::<Vec<_>>()
        .join("|");
    // Quoted values consume escaped quotes; malformed unterminated quoted values
    // conservatively consume the remainder of that line. Matching includes the
    // whole assignment and a possible separator, so output is context text, not
    // a promise that masked JSON remains syntactically valid.
    Ok(format!(
        r#"(?im)(?:^|[,{{;\n])[ \t]*(?:"(?:{alternatives})"|'(?:{alternatives})'|(?:{alternatives}))[ \t]*[:=][ \t]*(?:"(?:\\[^\r\n]|[^"\\\r\n])*"|'(?:\\[^\r\n]|[^'\\\r\n])*'|"[^\r\n]*|'[^\r\n]*|[^\r\n,;}}\]]*)"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    type Outcome = Result<(), Box<dyn std::error::Error>>;

    fn rule(id: &str, action: &str, matcher: serde_json::Value) -> serde_json::Value {
        let mut rule = json!({"id":id,"action":action});
        rule["matcher"] = matcher;
        rule
    }

    fn compile_rules(rules: &[serde_json::Value]) -> Result<Compiled, Invalid> {
        compile(&json!({"schema_version":1,"rules":rules}).to_string())
    }

    #[test]
    fn separate_actions_are_deterministic_and_preserve_patterns() -> Outcome {
        let rules = vec![
            rule(
                "customer",
                "mask",
                json!({"kind":"pattern","pattern":"K-[0-9]{6}"}),
            ),
            rule(
                "restricted",
                "block",
                json!({"kind":"pattern","pattern":"STOP-[0-9]+"}),
            ),
        ];
        let compiled = compile_rules(&rules)?;
        assert_eq!(compiled, compile_rules(&rules)?);
        assert_eq!(compiled.redaction["customer"], "K-[0-9]{6}");
        assert_eq!(compiled.blocked_patterns["restricted"], "STOP-[0-9]+");
        assert_eq!(compiled.rule_count, 2);
        Ok(())
    }

    #[test]
    fn fields_keep_boundaries_and_hide_escaped_and_malformed_quotes() -> Outcome {
        let compiled = compile_rules(&[rule(
            "account",
            "mask",
            json!({"kind":"field","names":["customer_number","Kundennummer"]}),
        )])?;
        let re = regex::Regex::new(&compiled.redaction["account"])?;
        for text in [
            r#"{"customer_number": "K-48\"2193", "visible": 7}"#,
            "Kundennummer = K-482193",
            "customer_number: \"K-482193,unterminated",
            r"customer_number='K-48\'2193'",
        ] {
            let masked = re.replace_all(text, "[REDACTED]");
            assert!(!masked.contains("K-48"), "{masked}");
            assert!(masked.contains("[REDACTED]"));
        }
        assert!(!re.is_match("other_customer_number=K-482193"));
        assert!(!re.is_match("customer_number_extra=K-482193"));
        assert!(
            !re.is_match(&format!(r#"{{"customer{}u005fnumber": "K-482193"}}"#, '\\')),
            "encoded JSON keys are explicitly outside literal-field matching"
        );
        Ok(())
    }

    #[test]
    fn bounds_duplicates_and_empty_matches_reject_without_echo() -> Outcome {
        let valid = rule(
            "customer",
            "mask",
            json!({"kind":"pattern","pattern":"K-[0-9]+"}),
        );
        let mut distinct: Vec<_> = (0..MAX_RULES)
            .map(|i| {
                rule(
                    &format!("r{i}"),
                    "mask",
                    json!({"kind":"pattern","pattern":"K-[0-9]+"}),
                )
            })
            .collect();
        assert_eq!(compile_rules(&distinct)?.rule_count, MAX_RULES);
        distinct.push(rule(
            "overflow",
            "mask",
            json!({"kind":"pattern","pattern":"K-[0-9]+"}),
        ));
        let mut unknown = valid.clone();
        unknown["sensitive-field"] = json!("do-not-echo");
        for rules in [
            distinct,
            vec![valid.clone(), valid.clone()],
            vec![rule(
                "customer",
                "mask",
                json!({"kind":"pattern","pattern":"["}),
            )],
            vec![rule(
                "customer",
                "mask",
                json!({"kind":"pattern","pattern":"x*"}),
            )],
            vec![rule(
                "customer",
                "mask",
                json!({"kind":"pattern","pattern":"X".repeat(2049)}),
            )],
            vec![rule(
                "customer",
                "mask",
                json!({"kind":"field","names":["id","id"]}),
            )],
            vec![rule(
                "Customer",
                "mask",
                json!({"kind":"pattern","pattern":"K-[0-9]+"}),
            )],
            vec![unknown],
            vec![],
        ] {
            let error = compile_rules(&rules)
                .err()
                .ok_or("invalid rules compiled")?;
            assert_eq!(error.to_string(), "invalid personal protection rules");
        }
        assert_eq!(compile(r#"{"schema_version":2,"rules":[]}"#), Err(Invalid));
        assert_eq!(compile(&"x".repeat(MAX_INPUT_BYTES + 1)), Err(Invalid));
        Ok(())
    }
}
