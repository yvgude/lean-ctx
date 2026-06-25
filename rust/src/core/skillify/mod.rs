//! Skillify (#290): distill a project's recurring session patterns into versioned,
//! git-committable `.cursor/rules/skillify-*.mdc` rule files.
//!
//! Pipeline: [`candidate::mine_candidates`] (read diary + knowledge) →
//! [`gate::judge`] (precision-biased KEEP/SKIP) → [`rule_file::write_candidate`]
//! (create / merge / unchanged). Runs on demand only; nothing is written unless
//! the miner is invoked, and re-runs are idempotent.

pub mod candidate;
pub mod gate;
pub mod rule_file;

use std::path::PathBuf;

use gate::Verdict;
use rule_file::WriteOutcome;

/// Summary of one mining run, for human + machine reporting.
#[derive(Debug, Default)]
pub struct MineReport {
    pub created: Vec<String>,
    pub merged: Vec<String>,
    pub unchanged: Vec<String>,
    /// `(title, reason)` for every rejected candidate.
    pub skipped: Vec<(String, String)>,
    pub candidates_seen: usize,
    pub output_dir: String,
}

/// One generated rule on disk.
#[derive(Debug, Clone)]
pub struct RuleSummary {
    pub slug: String,
    pub version: u32,
    pub title: String,
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
pub fn mine(project_root: &str) -> Result<MineReport, String> {
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
        ..Default::default()
    };

    for c in &candidates {
        match gate::judge(c, cfg.min_confidence, cfg.min_recurrence) {
            Verdict::Skip(reason) => report.skipped.push((c.title.clone(), reason)),
            Verdict::Keep => {
                let full = rule_file::full_slug(&c.slug);
                match rule_file::write_candidate(&root, c, &now)? {
                    WriteOutcome::Created => report.created.push(full),
                    WriteOutcome::Merged => report.merged.push(full),
                    WriteOutcome::Unchanged => report.unchanged.push(full),
                }
            }
        }
    }
    Ok(report)
}

/// List the generated rules currently on disk for the configured scope.
#[must_use]
pub fn list_rules(project_root: &str) -> Vec<RuleSummary> {
    let cfg = config();
    let root = output_root(project_root, &cfg.scope);
    let dir = rule_file::rules_dir(&root);
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
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
        if let Ok(content) = std::fs::read_to_string(&path) {
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
    out
}

/// Copy a project-scoped generated rule into the global `~/.cursor/rules` so it
/// applies to every project. Returns the destination path on success.
pub fn promote(project_root: &str, slug: &str) -> Result<String, String> {
    let full = if slug.starts_with(rule_file::SLUG_PREFIX) {
        slug.to_string()
    } else {
        rule_file::full_slug(slug)
    };
    let src = rule_file::rule_path(&PathBuf::from(project_root), &full);
    if !src.exists() {
        return Err(format!("no generated rule `{full}` in this project"));
    }
    let home = dirs::home_dir().ok_or("cannot resolve home directory")?;
    let dst = rule_file::rule_path(&home, &full);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let content = std::fs::read_to_string(&src).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(&dst, &content)?;
    Ok(dst.display().to_string())
}

/// Current skillify configuration, for `status`.
#[must_use]
pub fn current_config() -> crate::core::config::SkillifyConfig {
    config()
}
