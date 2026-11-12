// SPDX-License-Identifier: Apache-2.0
//! Strict parser for one `.leanctx/decisions/<id>.toml` record.
//!
//! Unknown keys are errors (a misspelt `scope.path` must not silently widen a
//! decision to the whole repo), and so are invalid globs and regexes: a record
//! that cannot be checked as written is reported, never half-applied.

use serde::Deserialize;

use super::{Check, CheckKind, Decision, Status};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDecision {
    id: String,
    title: String,
    statement: String,
    #[serde(default)]
    rationale: String,
    status: RawStatus,
    #[serde(default)]
    supersedes: Vec<String>,
    decided_by: RawDecidedBy,
    decided_at: RawDate,
    #[serde(default)]
    source: Option<RawSource>,
    #[serde(default)]
    scope: RawScope,
    #[serde(default, rename = "check")]
    checks: Vec<RawCheck>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum RawStatus {
    Active,
    Superseded,
    Revoked,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum RawDecidedBy {
    Owner,
    Team,
    Review,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawDate {
    Text(String),
    Toml(toml::value::Datetime),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    kind: RawSourceKind,
    #[serde(rename = "ref")]
    reference: String,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "kebab-case")]
enum RawSourceKind {
    OwnerAnswer,
    Review,
    Mr,
    Session,
    Doc,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawScope {
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    symbols: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCheck {
    kind: RawCheckKind,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum RawCheckKind {
    ForbidPath,
    ForbidRegex,
    RequireRegex,
}

/// Upper bound for a compiled check regex (bytes of program); a record is
/// data from the repository and must not make every edit slow.
const REGEX_SIZE_LIMIT: usize = 1 << 20;

fn valid_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("dec-") else {
        return false;
    };
    !rest.is_empty()
        && id.len() <= 80
        && rest.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn valid_date(date: &str) -> bool {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok() && date.len() == 10
}

fn compile_globs(field: &str, globs: &[String]) -> Result<Vec<glob::Pattern>, String> {
    globs
        .iter()
        .enumerate()
        .map(|(i, g)| {
            if g.trim().is_empty() {
                return Err(format!("{field}[{i}]: empty glob"));
            }
            if g.starts_with('/') || g.split('/').any(|c| c == "..") {
                return Err(format!(
                    "{field}[{i}] \"{g}\": globs are relative to the project root (no leading `/`, no `..`)"
                ));
            }
            glob::Pattern::new(g).map_err(|e| format!("{field}[{i}] \"{g}\": invalid glob: {e}"))
        })
        .collect()
}

fn nonempty_list(field: &str, values: &[String]) -> Result<(), String> {
    match values.iter().position(|v| v.trim().is_empty()) {
        Some(i) => Err(format!("{field}[{i}]: empty value")),
        None => Ok(()),
    }
}

fn compile_check(i: usize, raw: RawCheck) -> Result<Check, String> {
    let kind = match raw.kind {
        RawCheckKind::ForbidPath => CheckKind::ForbidPath,
        RawCheckKind::ForbidRegex => CheckKind::ForbidRegex,
        RawCheckKind::RequireRegex => CheckKind::RequireRegex,
    };
    let globs = compile_globs(&format!("check[{i}].paths"), &raw.paths)?;
    let regex = match (kind, raw.pattern.as_deref()) {
        (CheckKind::ForbidPath, Some(_)) => {
            return Err(format!(
                "check[{i}]: `pattern` is not used by kind forbid_path (it forbids the paths themselves)"
            ));
        }
        (CheckKind::ForbidPath, None) => None,
        (_, None) => {
            return Err(format!(
                "check[{i}]: kind {} requires `pattern`",
                kind.as_str()
            ));
        }
        (_, Some(p)) => Some(
            regex::RegexBuilder::new(p)
                .size_limit(REGEX_SIZE_LIMIT)
                .build()
                .map_err(|e| format!("check[{i}].pattern: invalid regex: {e}"))?,
        ),
    };
    if raw.message.as_deref().is_some_and(|m| m.trim().is_empty()) {
        return Err(format!(
            "check[{i}].message: empty (omit it to use the statement)"
        ));
    }
    Ok(Check {
        kind,
        paths: raw.paths,
        globs,
        pattern: raw.pattern,
        regex,
        message: raw.message,
    })
}

/// Parse and validate one record. `file_stem` must equal the record's `id`;
/// `rel_file` is kept for reporting. Errors name the offending key.
pub fn parse_decision(text: &str, file_stem: &str, rel_file: &str) -> Result<Decision, String> {
    let raw: RawDecision = toml::from_str(text).map_err(|e| {
        // toml's message already names the key and position; keep it on one line.
        e.to_string()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    })?;
    if !valid_id(&raw.id) {
        return Err(format!(
            "id `{}`: must be kebab-case with prefix `dec-` (e.g. dec-no-macos-gate)",
            raw.id
        ));
    }
    if raw.id != file_stem {
        return Err(format!(
            "id `{}` does not match the file name (expected {}.toml)",
            raw.id, raw.id
        ));
    }
    for (field, value) in [("title", &raw.title), ("statement", &raw.statement)] {
        if value.trim().is_empty() {
            return Err(format!("{field}: must not be empty"));
        }
    }
    if let Some(bad) = raw.supersedes.iter().find(|s| !valid_id(s)) {
        return Err(format!("supersedes: `{bad}` is not a decision id"));
    }
    if raw.supersedes.iter().any(|s| s == &raw.id) {
        return Err("supersedes: a decision cannot supersede itself".to_string());
    }
    let decided_at = match raw.decided_at {
        RawDate::Text(s) => s,
        RawDate::Toml(d) => d.to_string(),
    };
    if !valid_date(&decided_at) {
        return Err(format!("decided_at `{decided_at}`: expected YYYY-MM-DD"));
    }
    let (source_kind, source_ref) = match raw.source {
        Some(s) => {
            if s.reference.trim().is_empty() {
                return Err("source.ref: must not be empty".to_string());
            }
            let kind = match s.kind {
                RawSourceKind::OwnerAnswer => "owner-answer",
                RawSourceKind::Review => "review",
                RawSourceKind::Mr => "mr",
                RawSourceKind::Session => "session",
                RawSourceKind::Doc => "doc",
            };
            (Some(kind.to_string()), Some(s.reference))
        }
        None => (None, None),
    };
    nonempty_list("scope.repos", &raw.scope.repos)?;
    nonempty_list("scope.symbols", &raw.scope.symbols)?;
    let path_globs = compile_globs("scope.paths", &raw.scope.paths)?;
    let checks = raw
        .checks
        .into_iter()
        .enumerate()
        .map(|(i, c)| compile_check(i, c))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Decision {
        id: raw.id,
        title: raw.title,
        statement: raw.statement,
        rationale: raw.rationale,
        status: match raw.status {
            RawStatus::Active => Status::Active,
            RawStatus::Superseded => Status::Superseded,
            RawStatus::Revoked => Status::Revoked,
        },
        supersedes: raw.supersedes,
        decided_by: match raw.decided_by {
            RawDecidedBy::Owner => "owner",
            RawDecidedBy::Team => "team",
            RawDecidedBy::Review => "review",
        }
        .to_string(),
        decided_at,
        source_kind,
        source_ref,
        repos: raw.scope.repos,
        paths: raw.scope.paths,
        path_globs,
        symbols: raw.scope.symbols,
        checks,
        file: rel_file.to_string(),
    })
}

/// A fresh record for `lean-ctx decisions new` that parses as written.
pub fn scaffold(id: &str, title: &str, today: &str) -> String {
    let title = title.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "id = \"{id}\"\n\
         title = \"{title}\"\n\
         statement = \"{title}.\"\n\
         rationale = \"\"\n\
         status = \"active\"                 # active | superseded | revoked\n\
         supersedes = []                   # ids\n\
         decided_by = \"team\"              # owner | team | review\n\
         decided_at = \"{today}\"\n\
         \n\
         [source]                          # where it was decided\n\
         kind = \"doc\"                      # owner-answer | review | mr | session | doc\n\
         ref = \".leanctx/decisions/{id}.toml\"\n\
         \n\
         [scope]\n\
         repos = []                        # empty = all\n\
         paths = []                        # globs; empty = repo-wide\n\
         symbols = []\n\
         \n\
         # Optional, machine-checkable; may be several.\n\
         # [[check]]\n\
         # kind = \"forbid_regex\"           # forbid_path | forbid_regex | require_regex\n\
         # paths = [\"src/**\"]\n\
         # pattern = \"(?m)^.*TODO\"\n\
         # message = \"why this is not allowed\"\n"
    )
}

/// `dec-<slug>` from free text: lower-case ASCII words joined by `-`.
pub fn id_from_slug(slug: &str) -> Option<String> {
    let slug = slug.trim().strip_prefix("dec-").unwrap_or(slug.trim());
    let words: Vec<String> = slug
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let id = format!("dec-{}", words.join("-"));
    valid_id(&id).then_some(id)
}
