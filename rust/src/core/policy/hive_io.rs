// SPDX-License-Identifier: Apache-2.0
//! The I/O members of the Hive effective policy (ent2-3):
//! `security.readable_roots` and `security.never_send`.
//!
//! - `readable_roots` (path globs, `{project}` and a leading `~` expanded,
//!   `x-merge: intersect`): LeanCTX tools and hooked native reads may read a
//!   path only when it, or one of its ancestors, matches a root. An empty
//!   list allows nothing.
//! - `never_send` (gitignore globs, `x-merge: union`): a matching path is
//!   never read into context, never indexed and never sent to a model or to
//!   Hive. Patterns are matched relative to the project; a path outside it is
//!   matched by its components, so an unanchored pattern (`*.pem`, `.env*`,
//!   `secrets/`) applies everywhere.
//!
//! Both are read from the installed floor ([`super::hive_floor`]) for the
//! vault linked to the project. Fail-closed: a damaged, missing-but-required
//! or older floor, or a pattern that does not compile, refuses every read
//! instead of dropping the rules. Neither the role (`allow_secret_paths`)
//! nor the warn boundary mode loosens them: the Hive policy is a floor a
//! local setting may only tighten.
//!
//! Monolith: it needs the installed floor, the vault map and the data
//! directory, all engine state (`core::policy::hive_floor`).

use std::path::{Component, Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::hive_floor;

/// The I/O rules of the Hive policy in force for one project.
#[derive(Debug)]
pub struct IoRules {
    project: PathBuf,
    /// `None` = the policy sets no readable roots (everything the other
    /// gates allow is readable); `Some(empty)` = nothing is readable.
    roots: Option<Vec<glob::Pattern>>,
    never_send: Option<Gitignore>,
}

/// Why a path is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The path lies outside every readable root.
    OutsideRoots,
    /// The path matches a `never_send` pattern.
    NeverSend(String),
}

/// The I/O rules for `project` from the floor installed in this data
/// directory. `Ok(None)` when no Hive policy is installed or it sets neither
/// member; `Err` (fail closed) when the installed policy cannot be trusted.
pub fn rules_for(project: &Path) -> Result<Option<IoRules>, String> {
    let Some(dir) = hive_floor::floor_dir() else {
        return Ok(None);
    };
    rules_in(&dir, project)
}

/// [`rules_for`] with the floor directory given (`<hive data>/policy/effective`).
pub fn rules_in(floor_dir: &Path, project: &Path) -> Result<Option<IoRules>, String> {
    let (floor, required) = hive_floor::read(floor_dir)?;
    let Some(checked) = hive_floor::check(floor.as_deref(), required.as_deref())? else {
        return Ok(None);
    };
    let project = canonical_or_given(project);
    let vault = floor.as_deref().and_then(|text| {
        let hive_data = floor_dir.parent()?.parent()?;
        hive_floor::vault_for(text, hive_data, &project)
    });
    let security = &checked.policy.for_vault(vault.as_deref()).security;
    compile(
        &project,
        security.readable_roots.as_deref(),
        security.never_send.as_deref(),
    )
}

/// Compile both members for `project` (absolute). `Ok(None)` when neither is set.
pub fn compile(
    project: &Path,
    roots: Option<&[String]>,
    never_send: Option<&[String]>,
) -> Result<Option<IoRules>, String> {
    if roots.is_none() && never_send.is_none() {
        return Ok(None);
    }
    let roots = roots
        .map(|roots| {
            roots
                .iter()
                .map(|root| root_pattern(project, root))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let never_send = never_send
        .map(|patterns| {
            let mut builder = GitignoreBuilder::new(project);
            for pattern in patterns {
                builder.add_line(None, pattern).map_err(|_| {
                    format!("the Hive policy never_send pattern {pattern:?} does not compile")
                })?;
            }
            builder
                .build()
                .map_err(|_| "the Hive policy never_send patterns do not compile".to_string())
        })
        .transpose()?;
    Ok(Some(IoRules {
        project: project.to_path_buf(),
        roots,
        never_send,
    }))
}

/// A readable root as an absolute glob: `{project}` becomes the project
/// directory, a leading `~` the home directory, a relative root is relative
/// to the project. Literal parts are escaped, so a `[` in a directory name
/// stays literal.
fn root_pattern(project: &Path, root: &str) -> Result<glob::Pattern, String> {
    let escape = |p: &Path| glob::Pattern::escape(&p.to_string_lossy());
    let mut expanded = root.replace("{project}", &escape(project));
    if expanded == "~" || expanded.starts_with("~/") {
        let home = dirs::home_dir()
            .ok_or_else(|| format!("the Hive policy readable root {root:?} needs a home"))?;
        expanded = format!("{}{}", escape(&canonical_or_given(&home)), &expanded[1..]);
    }
    if !Path::new(&expanded).is_absolute() {
        expanded = format!("{}/{expanded}", escape(project));
    }
    let trimmed = expanded.trim_end_matches('/');
    let expanded = if trimmed.is_empty() { "/" } else { trimmed };
    glob::Pattern::new(expanded)
        .map_err(|_| format!("the Hive policy readable root {root:?} does not compile"))
}

impl IoRules {
    /// Whether the policy restricts readable roots at all.
    pub fn has_roots(&self) -> bool {
        self.roots.is_some()
    }

    /// Whether the policy sets `never_send` patterns.
    pub fn has_never_send(&self) -> bool {
        self.never_send.is_some()
    }

    /// The refusal for reading `path`, or `None` when it may be read.
    /// A relative `path` is taken relative to the project.
    pub fn refusal(&self, path: &Path) -> Option<Refusal> {
        let path = self.absolute(path);
        if let Some(pattern) = self.never_send_hit(&path) {
            return Some(Refusal::NeverSend(pattern));
        }
        (!self.inside_roots(&path)).then_some(Refusal::OutsideRoots)
    }

    /// The `never_send` pattern `path` matches (only that member).
    pub fn never_send_hit(&self, path: &Path) -> Option<String> {
        let gitignore = self.never_send.as_ref()?;
        let path = self.absolute(path);
        let relative = match path.strip_prefix(&self.project) {
            Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
            Ok(_) => return None,
            // Outside the project: match by components (unanchored patterns).
            Err(_) => path
                .components()
                .filter(|c| matches!(c, Component::Normal(_)))
                .collect(),
        };
        if relative.as_os_str().is_empty() {
            return None;
        }
        let matched = gitignore.matched_path_or_any_parents(&relative, path.is_dir());
        matched
            .is_ignore()
            .then(|| matched.inner().map(|g| g.original().to_string()))
            .flatten()
    }

    /// Whether `path` (or an ancestor) matches a readable root.
    pub fn inside_roots(&self, path: &Path) -> bool {
        let Some(roots) = &self.roots else {
            return true;
        };
        let path = self.absolute(path);
        let options = glob::MatchOptions {
            case_sensitive: !cfg!(windows),
            require_literal_separator: true,
            require_literal_leading_dot: false,
        };
        path.ancestors().any(|candidate| {
            roots
                .iter()
                .any(|root| root.matches_path_with(candidate, options))
        })
    }

    fn absolute(&self, path: &Path) -> PathBuf {
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.project.join(path)
        };
        lexical(&joined)
    }
}

impl Refusal {
    /// One line for the refusal message.
    pub fn describe(&self) -> String {
        match self {
            Self::OutsideRoots => "outside the readable roots of the Hive policy".into(),
            Self::NeverSend(pattern) => {
                format!("matches the Hive policy never_send pattern {pattern:?}")
            }
        }
    }
}

/// `..` and `.` resolved without touching the file system.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn canonical_or_given(path: &Path) -> PathBuf {
    crate::core::pathutil::canonicalize_secure(path).unwrap_or_else(|_| lexical(path))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn readable_roots_admit_inside_and_refuse_outside() {
        let project = Path::new("/work/app");
        let rules = compile(
            project,
            Some(&strings(&["{project}", "/opt/shared/*/docs"])),
            None,
        )
        .unwrap()
        .unwrap();
        for inside in [
            "/work/app",
            "/work/app/src/main.rs",
            "src/lib.rs",
            "/opt/shared/team/docs/a.md",
        ] {
            assert_eq!(rules.refusal(Path::new(inside)), None, "{inside}");
        }
        for outside in [
            "/work/other/x.rs",
            "/work/app-evil/x.rs",
            "/work/app/../other/x.rs",
            "../../etc/passwd",
            "/opt/shared/team/src/a.rs",
        ] {
            assert_eq!(
                rules.refusal(Path::new(outside)),
                Some(Refusal::OutsideRoots),
                "{outside}"
            );
        }
        // An empty list allows nothing.
        let none = compile(project, Some(&[]), None).unwrap().unwrap();
        assert_eq!(
            none.refusal(Path::new("/work/app/a.rs")),
            Some(Refusal::OutsideRoots)
        );
    }

    #[test]
    fn never_send_refuses_inside_and_outside_the_project() {
        let project = Path::new("/work/app");
        let rules = compile(
            project,
            None,
            Some(&strings(&[
                "*.pem",
                ".env*",
                "secrets/",
                "/config/prod.toml",
            ])),
        )
        .unwrap()
        .unwrap();
        for (path, pattern) in [
            ("/work/app/tls/server.pem", "*.pem"),
            ("/work/app/.env.local", ".env*"),
            ("/work/app/secrets/token.txt", "secrets/"),
            ("/work/app/config/prod.toml", "/config/prod.toml"),
            ("/home/u/other/key.pem", "*.pem"),
        ] {
            assert_eq!(
                rules.refusal(Path::new(path)),
                Some(Refusal::NeverSend(pattern.into())),
                "{path}"
            );
        }
        for path in [
            "/work/app/src/main.rs",
            "/work/app/sub/config/prod.toml",
            "/work/app",
        ] {
            assert_eq!(rules.refusal(Path::new(path)), None, "{path}");
        }
    }

    #[test]
    fn unset_policy_has_no_rules_and_a_damaged_floor_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let floor_dir = dir.path().join("hive").join("policy").join("effective");
        std::fs::create_dir_all(&floor_dir).unwrap();
        let project = dir.path();
        // Nothing installed: no rules.
        assert!(rules_in(&floor_dir, project).unwrap().is_none());
        let floor = |security: serde_json::Value| {
            json!({
                "schema": hive_floor::FLOOR_SCHEMA, "version": 2,
                "digest": format!("sha256:{}", "a".repeat(64)),
                "not_after": "2099-01-01T00:00:00Z",
                "policy": {"effective": {
                    "schema": "leanctx.hive.policy/v1",
                    "security": security, "context": {}, "sharing": {}
                }, "vaults": []}
            })
            .to_string()
        };
        std::fs::write(
            floor_dir.join(hive_floor::REQUIRED_FILE),
            r#"{"version":2}"#,
        )
        .unwrap();
        // A policy without the members: no rules.
        std::fs::write(
            floor_dir.join(hive_floor::FLOOR_FILE),
            floor(json!({"pii": "redact"})),
        )
        .unwrap();
        assert!(rules_in(&floor_dir, project).unwrap().is_none());
        // With them: the project's own root applies.
        std::fs::write(
            floor_dir.join(hive_floor::FLOOR_FILE),
            floor(json!({"readable_roots": ["{project}"], "never_send": ["*.pem"]})),
        )
        .unwrap();
        let rules = rules_in(&floor_dir, project).unwrap().unwrap();
        assert!(rules.inside_roots(&canonical_or_given(project).join("a.rs")));
        assert!(!rules.inside_roots(Path::new("/elsewhere/a.rs")));
        // Damaged or missing while required: fail closed.
        std::fs::write(floor_dir.join(hive_floor::FLOOR_FILE), "{").unwrap();
        assert!(rules_in(&floor_dir, project).is_err());
        std::fs::remove_file(floor_dir.join(hive_floor::FLOOR_FILE)).unwrap();
        assert!(rules_in(&floor_dir, project).is_err());
    }
}
