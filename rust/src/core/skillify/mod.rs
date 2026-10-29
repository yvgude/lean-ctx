//! Skillify (#290): distill a project's recurring session patterns into versioned,
//! git-committable `.cursor/rules/skillify-*.mdc` rule files.
//!
//! Pipeline: [`candidate::mine_candidates`] (read diary + knowledge) →
//! [`gate::judge`] (precision-biased KEEP/SKIP) → [`rule_file::plan`]
//! (create / merge / unchanged). Runs on demand only; nothing is written unless
//! the miner is invoked, and re-runs are idempotent.
//!
//! A rule file is a derived copy of the facts it was distilled from: it is
//! bound to their sources when it is written and asked again before lean-ctx
//! lists or promotes it ([`provenance`]).

pub(crate) mod candidate;
pub(crate) mod gate;
mod provenance;
pub(crate) mod rule_file;

use std::path::{Path, PathBuf};

use gate::Verdict;
use rule_file::WriteOutcome;

/// Summary of one mining run, for human + machine reporting.
#[derive(Debug, Default)]
pub(crate) struct MineReport {
    pub created: Vec<String>,
    pub merged: Vec<String>,
    pub unchanged: Vec<String>,
    /// `(title, reason)` for every rejected candidate.
    pub skipped: Vec<(String, String)>,
    pub candidates_seen: usize,
    pub output_dir: String,
    /// Rule files the run found and must not touch, sorted by path.
    pub notices: Vec<RuleNotice>,
}

/// One generated rule on disk.
#[derive(Debug, Clone)]
pub(crate) struct RuleSummary {
    pub slug: String,
    pub version: u32,
    pub title: String,
}

/// A rule file that needs the user's attention. Never a rule: the path, a
/// reason code and a fixed sentence — nothing of the file's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuleNotice {
    pub path: String,
    pub code: &'static str,
    pub reason: &'static str,
}

/// What `list` returns: the rules lean-ctx may show now, and — apart from
/// them — the files it reports instead. Both sorted (#498).
#[derive(Debug, Default)]
pub(crate) struct RuleListing {
    pub rules: Vec<RuleSummary>,
    pub notices: Vec<RuleNotice>,
}

/// The walk every operation runs first (`provenance::review`), as notices.
fn reviewed(output_root: &Path, project_root: &str) -> Vec<RuleNotice> {
    provenance::review(output_root, project_root)
        .into_iter()
        .map(|(path, notice)| RuleNotice {
            path: path.display().to_string(),
            code: notice.code(),
            reason: notice.sentence(),
        })
        .collect()
}

fn config() -> crate::core::config::SkillifyConfig {
    crate::core::config::Config::load().skillify
}

/// Resolve the output root from the configured scope.
fn output_root(project_root: &str, scope: &str) -> PathBuf {
    if scope.eq_ignore_ascii_case("global")
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    PathBuf::from(project_root)
}

/// Run the miner end-to-end. Returns an error only when skillify is disabled or
/// a write fails; an empty project simply yields an empty report.
pub(crate) fn mine(project_root: &str) -> Result<MineReport, String> {
    let cfg = config();
    if !cfg.enabled {
        return Err("skillify is disabled — set `[skillify] enabled = true`".to_string());
    }
    let root = output_root(project_root, &cfg.scope);
    let now = chrono::Utc::now().to_rfc3339();
    let candidates = candidate::mine_candidates(project_root);

    let mut report = MineReport {
        candidates_seen: candidates.len(),
        output_dir: rule_file::rules_dir(&root).display().to_string(),
        // Rules whose sources were withdrawn leave the host's directory
        // first; a rule that is derived again below is written anew through
        // admission.
        notices: reviewed(&root, project_root),
        ..Default::default()
    };

    for c in &candidates {
        match gate::judge(c, cfg.min_confidence, cfg.min_recurrence) {
            Verdict::Skip(reason) => report.skipped.push((c.title.clone(), reason)),
            Verdict::Keep => {
                let full = rule_file::full_slug(&c.slug);
                match write_rule(&root, project_root, c, &now) {
                    Ok(WriteOutcome::Created) => report.created.push(full),
                    Ok(WriteOutcome::Merged) => report.merged.push(full),
                    Ok(WriteOutcome::Unchanged) => report.unchanged.push(full),
                    Err(Refused::Withheld(reason)) => report.skipped.push((full, reason)),
                    Err(Refused::Failed(error)) => return Err(error),
                }
            }
        }
    }
    Ok(report)
}

/// Why a kept candidate did not become a rule file.
enum Refused {
    /// Not permitted now; the reason is typed and content-free.
    Withheld(String),
    /// The file system refused the write.
    Failed(String),
}

/// Write the rule file of one kept candidate under `output_root`, for a
/// request working in `project_root`.
///
/// A rule file is a persisted, git-committable artefact: only admitted text
/// is written into it (E3), and it is a derived copy of the facts it was
/// distilled from (G04) — its binding to their sources is decided before the
/// write and recorded after it. An unchanged rule is bound again, so a rule
/// written before bindings existed gets one on the next run.
fn write_rule(
    output_root: &Path,
    project_root: &str,
    candidate: &candidate::SkillCandidate,
    now: &str,
) -> Result<WriteOutcome, Refused> {
    let admitted = admitted_candidate(candidate).map_err(Refused::Withheld)?;
    let planned = rule_file::plan(output_root, &admitted, now);
    let binding = provenance::prepare(&planned.text, &candidate.derivation, project_root)
        .map_err(|withheld| Refused::Withheld(withheld.message("rule")))?;
    planned.write().map_err(Refused::Failed)?;
    if let Some(binding) = &binding {
        provenance::record(&planned.path, binding).map_err(Refused::Failed)?;
    }
    provenance::release(&planned.path);
    Ok(planned.outcome)
}

/// List the generated rules currently on disk for the configured scope.
pub(crate) fn list_rules(project_root: &str) -> RuleListing {
    let cfg = config();
    let root = output_root(project_root, &cfg.scope);
    let dir = rule_file::rules_dir(&root);
    // Under protection: a rule lean-ctx wrote whose sources were withdrawn
    // has left this directory now; one it must not touch is a notice, apart
    // from the rules.
    let mut listing = RuleListing {
        rules: Vec::new(),
        notices: reviewed(&root, project_root),
    };
    let out = &mut listing.rules;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return listing;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("mdc") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !stem.starts_with(rule_file::SLUG_PREFIX) {
            continue;
        }
        // A rule is a derived copy: it is listed only while the sources it is
        // bound to still admit it (`provenance::served`); a rule without a
        // binding follows the originless rule.
        let held = std::fs::read_to_string(&path)
            .is_ok_and(|raw| provenance::served(&path, &raw, project_root).is_ok());
        if !held {
            continue;
        }
        // The listed title is content of a file in the output root: it is
        // read through the common rooted admitted reader; a rule that is
        // withheld is not listed.
        if let Ok(content) = crate::tools::ctx_read::read_file_for_tool_rooted(
            &path.to_string_lossy(),
            &root.to_string_lossy(),
            "ctx_skillify",
        ) {
            let version = rule_file::parse_existing(&content).map_or(1, |r| r.version);
            let title =
                rule_file::extract_description(&content).unwrap_or_else(|| stem.to_string());
            out.push(RuleSummary {
                slug: stem.to_string(),
                version,
                title,
            });
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    listing
}

/// Copy a project-scoped generated rule into the global `~/.cursor/rules` so it
/// applies to every project. Returns the destination path on success.
pub(crate) fn promote(project_root: &str, slug: &str) -> Result<String, String> {
    let home = dirs::home_dir().ok_or("cannot resolve home directory")?;
    promote_into(project_root, slug, &home)
}

/// [`promote`] into the rules directory under `home`. Nothing under `home`
/// is created or written before every check has passed.
fn promote_into(project_root: &str, slug: &str, home: &Path) -> Result<String, String> {
    let full = if slug.starts_with(rule_file::SLUG_PREFIX) {
        slug.to_string()
    } else {
        rule_file::full_slug(slug)
    };
    // The walk every operation runs first: a rule whose sources were
    // withdrawn is no longer in the project to be promoted.
    provenance::review(Path::new(project_root), project_root);
    let src = rule_file::rule_path(&PathBuf::from(project_root), &full);
    if !src.exists() {
        return Err(format!("no generated rule `{full}` in this project"));
    }
    // Decided before anything outside the project is touched: the copy may
    // exist (`promotable`), and the rule is still admitted by the sources it
    // is bound to (`provenance::served`).
    let content = promotable(&src, project_root)?;
    provenance::served(&src, &content, project_root)
        .map_err(|withheld| withheld.message("rule"))?;
    let dst = rule_file::rule_path(home, &full);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    crate::config_io::write_atomic_with_backup(&dst, &content)?;
    // The copy keeps the binding of its original: it stays as revocable.
    provenance::carry_over(&src, &dst, &content)?;
    Ok(dst.display().to_string())
}

/// The bytes a promotion copies out of the project: the ORIGINAL rule file,
/// and only when the copy may exist. A promoted rule is a persisted copy
/// outside the project, so the source must be admitted, complete and storable
/// NOW (E3), and admission must leave it unchanged — a rule the current
/// policy would mask is not copied as it is, and never copied masked.
fn promotable(src: &std::path::Path, project_root: &str) -> Result<String, String> {
    let mut remaining = crate::core::limits::max_read_bytes();
    let read = crate::tools::ctx_read::read_file_for_tool_rooted_admitted(
        &src.to_string_lossy(),
        project_root,
        "ctx_skillify",
        &mut remaining,
    )
    .map_err(|error| error.to_string())?;
    let original = std::fs::read_to_string(src).map_err(|e| e.to_string())?;
    if !read.storable || !read.complete || read.content != original {
        return Err(
            "rule withheld by the context gateway: its content may not be copied out of the \
             project unchanged"
                .to_string(),
        );
    }
    Ok(original)
}

/// The candidate as it may be written to a rule file: title and body
/// admitted for a derived store under the current policy. A candidate is
/// distilled from knowledge facts and the session diary; a part that is
/// withheld or restricted refuses the whole rule, with a content-free reason.
fn admitted_candidate(
    candidate: &candidate::SkillCandidate,
) -> Result<candidate::SkillCandidate, String> {
    use crate::core::context_admission::carrier::admit_for_store;
    let admit = |text: &str| admit_for_store(text).map_err(|withheld| withheld.message("rule"));
    Ok(candidate::SkillCandidate {
        title: admit(&candidate.title)?,
        body: admit(&candidate.body)?,
        ..candidate.clone()
    })
}

/// Current skillify configuration, for `status`.
pub(crate) fn current_config() -> crate::core::config::SkillifyConfig {
    config()
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    fn candidate(body: &str) -> candidate::SkillCandidate {
        candidate::SkillCandidate {
            slug: "rotate-keys".into(),
            title: "Rotate keys".into(),
            body: body.into(),
            category: "decision".into(),
            recurrence: 3,
            confidence: 0.8,
            sources: vec!["sess1".into()],
            derivation: candidate::Derivation::default(),
        }
    }

    /// G04: a rule distilled from a fact that is bound to a file is bound to
    /// that file. lean-ctx lists and promotes it while the file is admitted
    /// and no longer once it is withdrawn; a rule distilled from a fact
    /// without a source on record is originless and withheld under
    /// protection. Refusals are typed and carry nothing of the rule.
    #[test]
    fn a_rule_is_used_only_while_the_sources_of_its_facts_admit_it() {
        use crate::core::context_admission::stored::lineage_support::{
            governed, project, scoped, two_facts, withdraw_source,
        };
        let _data = governed();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let output = Path::new(&root);
        two_facts(&root, "decision");
        // Under protection a rule is written only with a source on record:
        // the one distilled from the bound fact is, the one from the typed
        // fact is refused with a typed reason and leaves no file.
        scoped(&root, || {
            for mined in candidate::mine_candidates(&root) {
                match write_rule(output, &root, &mined, "2026-01-01T00:00:00Z") {
                    Ok(outcome) => {
                        assert_eq!(mined.slug, "token-check");
                        assert_eq!(outcome, WriteOutcome::Created);
                    }
                    Err(Refused::Withheld(reason)) => {
                        assert_eq!(mined.slug, "release-duty");
                        assert!(reason.contains("store.no_source_authority"), "{reason}");
                    }
                    Err(Refused::Failed(error)) => panic!("{error}"),
                }
            }
        });
        assert!(!rule_file::rule_path(output, "skillify-release-duty").exists());
        let created = rule_file::rule_path(output, "skillify-token-check");
        let text = std::fs::read_to_string(&created).expect("written rule");
        // A sibling lean-ctx wrote from the same sources, edited by hand
        // since, and a file that was there before bindings existed.
        let sibling = rule_file::rule_path(output, "skillify-edited");
        std::fs::write(&sibling, &text).expect("sibling");
        provenance::carry_over(&created, &sibling, &text).expect("binding of the sibling");
        let edited = format!("{text}\nA line added by hand.\n");
        std::fs::write(&sibling, &edited).expect("hand edit");
        let legacy = rule_file::rule_path(output, "skillify-legacy");
        std::fs::write(&legacy, "Run the linter before a push.\n").expect("legacy rule");

        let listed = || scoped(&root, || list_rules(&root));
        let slugs = |listing: &RuleListing| -> Vec<String> {
            listing.rules.iter().map(|rule| rule.slug.clone()).collect()
        };
        let notice = |path: &Path, code: &'static str| -> (String, &'static str) {
            (path.display().to_string(), code)
        };
        let codes = |listing: &RuleListing| -> Vec<(String, &'static str)> {
            listing
                .notices
                .iter()
                .map(|notice| (notice.path.clone(), notice.code))
                .collect()
        };
        // Served while its source is admitted. A notice is not a rule.
        let before = listed();
        assert_eq!(slugs(&before), ["skillify-token-check"]);
        assert_eq!(
            codes(&before),
            [notice(&legacy, "rule.no_source_on_record")]
        );

        // Withdrawn: the rule lean-ctx wrote leaves the host's directory — a
        // move, the bytes are kept — and the hand-edited one is left alone
        // and reported, with nothing of either rule in the report. The
        // quarantine is keyed by the canonical path of the rule while it
        // exists, as the move computes it: once moved, the path no longer
        // canonicalizes (on Windows the raw spelling then differs).
        let (copy, origin) = provenance::quarantine_paths(&created).expect("quarantine");
        withdraw_source(&root);
        let after = listed();
        assert!(slugs(&after).is_empty());
        assert!(!created.exists());
        assert_eq!(
            std::fs::read_to_string(copy).ok().as_deref(),
            Some(text.as_str())
        );
        assert!(origin.is_file());
        assert_eq!(
            std::fs::read_to_string(&sibling).ok().as_deref(),
            Some(edited.as_str())
        );
        assert_eq!(
            codes(&after),
            [
                notice(&sibling, "rule.edited_source_withdrawn"),
                notice(&legacy, "rule.no_source_on_record"),
            ]
        );
        assert!(
            after
                .notices
                .iter()
                .all(|notice| !notice.reason.contains("RS256"))
        );

        // After the move there is no rule to promote, and nothing is copied.
        let home = tempfile::tempdir().expect("home");
        let refused = scoped(&root, || promote_into(&root, "token-check", home.path()))
            .expect_err("a moved rule is not promoted");
        assert!(refused.contains("no generated rule"), "{refused}");
        assert!(!home.path().join(".cursor").exists());
    }

    /// E3: a generated rule file was written with whatever the miner
    /// distilled, and `promote` copied any rule file out of the project raw.
    #[test]
    fn rule_files_receive_admitted_text_and_only_unmasked_rules_are_promoted() {
        use crate::core::context_admission::stored::lineage_support::{
            bound_fact, project as canonical_project, scoped, withdraw_source,
        };
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().into_owned();
        let rule = |name: &str, content: &str| {
            let path = rule_file::rule_path(project.path(), name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            path
        };
        {
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            // Control: a clean candidate and a clean rule keep their bytes.
            let clean = admitted_candidate(&candidate("Run the linter before a push.")).unwrap();
            assert_eq!(clean.body, "Run the linter before a push.");
            let path = rule("skillify-clean", "Run the linter before a push.\n");
            assert_eq!(
                promotable(&path, &root).as_deref(),
                Ok("Run the linter before a push.\n")
            );

            let keyed = admitted_candidate(&candidate(&format!("Deploy with {AWS_KEY}.")));
            assert!(keyed.is_ok_and(|rule| !rule.body.contains(AWS_KEY)));
            let path = rule("skillify-keyed", &format!("Deploy with {AWS_KEY}.\n"));
            let refused = promotable(&path, &root).expect_err("a masked rule is not copied");
            assert!(!refused.contains(AWS_KEY), "{refused}");

            // G04, Community developer mode: a rule bound to a source that is
            // withdrawn is not moved (no protection claim) and is not copied
            // out of the project — refused with the typed reason, nothing of
            // the rule in it, nothing written under the home directory.
            let (_bound, bound_root) = canonical_project();
            let output = Path::new(&bound_root);
            bound_fact(&bound_root, "decision");
            scoped(&bound_root, || {
                for mined in candidate::mine_candidates(&bound_root) {
                    let written = write_rule(output, &bound_root, &mined, "2026-01-01T00:00:00Z");
                    assert!(
                        matches!(written, Ok(WriteOutcome::Created)),
                        "{}",
                        mined.slug
                    );
                }
            });
            let kept = rule_file::rule_path(output, "skillify-token-check");
            assert!(kept.is_file());
            withdraw_source(&bound_root);
            let home = tempfile::tempdir().unwrap();
            let refused = scoped(&bound_root, || {
                promote_into(&bound_root, "token-check", home.path())
            })
            .expect_err("a rule whose source is withdrawn is not copied");
            assert!(refused.contains("store.source_unavailable"), "{refused}");
            assert!(!refused.contains("RS256"), "{refused}");
            assert!(kept.is_file());
            assert!(!home.path().join(".cursor").exists());
        }
        let policy = crate::core::policy::load(
            "name = \"skillify-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let refused = admitted_candidate(&candidate("CONFIDENTIAL\nroadmap-marker"))
            .expect_err("a blocked candidate is not written");
        assert!(!refused.contains("roadmap-marker"), "{refused}");
        let path = rule("skillify-plan", "CONFIDENTIAL\nroadmap-marker\n");
        let refused = promotable(&path, &root).expect_err("a blocked rule is not copied");
        assert!(!refused.contains("roadmap-marker"), "{refused}");
    }
}
