//! Index-time file filters (#735): one shared corpus-membership decision for
//! every index builder.
//!
//! Source-control intent (`.gitignore`) and retrieval policy (what belongs in
//! the index corpus) are different axes: a file can be versioned and important
//! while still being bad context for code search (CSV seed data, fixtures,
//! lockfiles, vendored blobs). This module carries the declared retrieval
//! policy — `[index] include/exclude/respect_gitignore` from config, plus an
//! optional per-run CLI overlay (`lean-ctx index build --exclude …`) — so the
//! BM25, graph, and watch builders can never disagree about corpus membership.
//! The semantic (embedding) index chunks the BM25 corpus, so it inherits the
//! same universe by construction.
//!
//! Excluded files never enter the pipeline: no chunks, no graph nodes, no
//! embeddings. Globs are evaluated against the root-relative path with forward
//! slashes (same semantics as `extra_ignore_patterns`, which remains honored
//! as the legacy exclude list). Precedence: exclude wins over include; a
//! non-empty include list turns the corpus into "matching files only".

use std::sync::RwLock;

/// Per-run overlay set by the `lean-ctx index` CLI before builders run.
/// Process-local by design: one-off experiment flags must not leak into the
/// persisted config, and the short-lived CLI process is the only consumer.
struct CliOverlay {
    include: Vec<String>,
    exclude: Vec<String>,
    respect_gitignore: Option<bool>,
}

static CLI_OVERLAY: RwLock<Option<CliOverlay>> = RwLock::new(None);

/// Install the per-run CLI filter overlay (repeatable `--include`/`--exclude`
/// globs, `--no-gitignore`/`--respect-gitignore`). Called once by the `index`
/// CLI dispatch before any build starts.
pub fn set_cli_overlay(
    include: Vec<String>,
    exclude: Vec<String>,
    respect_gitignore: Option<bool>,
) {
    let mut guard = CLI_OVERLAY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = Some(CliOverlay {
        include,
        exclude,
        respect_gitignore,
    });
}

/// True when a CLI overlay is installed. The orchestrator uses this to skip
/// delegating the build to the daemon: the daemon would build with *its*
/// config and could overwrite the one-off filtered result.
pub fn cli_overlay_active() -> bool {
    CLI_OVERLAY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some()
}

/// The resolved corpus-membership filter for one indexing run.
#[derive(Debug)]
pub struct IndexFileFilter {
    include: Vec<glob::Pattern>,
    exclude: Vec<glob::Pattern>,
    /// Whether walkers should honor `.gitignore`/global/exclude files.
    pub respect_gitignore: bool,
}

impl IndexFileFilter {
    /// Resolve the effective filter: `[index]` config, extended by the CLI
    /// overlay. CLI `--exclude` appends to the config excludes; CLI
    /// `--include` replaces the config include set for this run (a one-off
    /// experiment declares its corpus outright); an explicit gitignore flag
    /// overrides the config boolean.
    pub fn effective() -> Self {
        let cfg = crate::core::config::Config::load();
        Self::resolve(&cfg)
    }

    /// [`Self::effective`] with an already-loaded config (builders that hold
    /// one avoid a second load).
    pub fn resolve(cfg: &crate::core::config::Config) -> Self {
        let mut include: Vec<String> = cfg.index.include.clone();
        let mut exclude: Vec<String> = cfg.index.exclude.clone();
        let mut respect_gitignore = cfg.index.respect_gitignore;

        let guard = CLI_OVERLAY
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(overlay) = guard.as_ref() {
            if !overlay.include.is_empty() {
                include.clone_from(&overlay.include);
            }
            exclude.extend(overlay.exclude.iter().cloned());
            if let Some(flag) = overlay.respect_gitignore {
                respect_gitignore = flag;
            }
        }

        Self::from_lists(&include, &exclude, respect_gitignore)
    }

    /// Build a filter from raw glob lists. Invalid globs are dropped with a
    /// warning rather than aborting the build — matching the long-standing
    /// `extra_ignore_patterns` behavior.
    pub fn from_lists(include: &[String], exclude: &[String], respect_gitignore: bool) -> Self {
        let compile = |patterns: &[String], role: &str| -> Vec<glob::Pattern> {
            patterns
                .iter()
                .filter_map(|p| match glob::Pattern::new(p) {
                    Ok(pat) => Some(pat),
                    Err(e) => {
                        tracing::warn!("[index_filter] ignoring invalid {role} glob {p:?}: {e}");
                        None
                    }
                })
                .collect()
        };
        Self {
            include: compile(include, "include"),
            exclude: compile(exclude, "exclude"),
            respect_gitignore,
        }
    }

    /// Corpus-membership decision for one file, by root-relative path with
    /// forward slashes. Exclude wins over include; a non-empty include list
    /// admits matching files only; the empty filter admits everything
    /// (byte-for-byte today's behavior).
    pub fn is_excluded(&self, rel_path: &str) -> bool {
        if self.exclude.iter().any(|p| p.matches(rel_path)) {
            return true;
        }
        if !self.include.is_empty() && !self.include.iter().any(|p| p.matches(rel_path)) {
            return true;
        }
        false
    }

    /// True when this filter changes corpus membership relative to the
    /// unfiltered default (gitignore deviation counts: it changes the walk).
    pub fn is_active(&self) -> bool {
        !self.include.is_empty() || !self.exclude.is_empty() || !self.respect_gitignore
    }

    /// One-line human/JSON summary of the active filter for `index status`.
    /// `None` when the filter is the do-nothing default, so default output
    /// stays byte-identical.
    pub fn summary(&self) -> Option<String> {
        if !self.is_active() {
            return None;
        }
        let fmt = |patterns: &[glob::Pattern]| -> String {
            patterns
                .iter()
                .map(glob::Pattern::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut parts: Vec<String> = Vec::new();
        if !self.include.is_empty() {
            parts.push(format!("include=[{}]", fmt(&self.include)));
        }
        if !self.exclude.is_empty() {
            parts.push(format!("exclude=[{}]", fmt(&self.exclude)));
        }
        if !self.respect_gitignore {
            parts.push("gitignore=off".to_string());
        }
        Some(parts.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(include: &[&str], exclude: &[&str]) -> IndexFileFilter {
        let inc: Vec<String> = include.iter().map(ToString::to_string).collect();
        let exc: Vec<String> = exclude.iter().map(ToString::to_string).collect();
        IndexFileFilter::from_lists(&inc, &exc, true)
    }

    #[test]
    fn empty_filter_admits_everything() {
        let f = filter(&[], &[]);
        assert!(!f.is_excluded("src/main.rs"));
        assert!(!f.is_excluded("data/seed.csv"));
        assert!(!f.is_active());
        assert!(f.summary().is_none());
    }

    #[test]
    fn exclude_glob_drops_matching_files() {
        // The #735 motivating case: versioned Liquibase CSV seed data.
        let f = filter(&[], &["src/main/resources/liquibase/data/*.csv"]);
        assert!(f.is_excluded("src/main/resources/liquibase/data/users.csv"));
        assert!(!f.is_excluded("src/main/java/App.java"));
    }

    #[test]
    fn recursive_exclude_matches_at_any_depth() {
        let f = filter(&[], &["**/*.csv"]);
        assert!(f.is_excluded("data/seed.csv"));
        assert!(f.is_excluded("a/b/c/d.csv"));
        assert!(f.is_excluded("top.csv"));
        assert!(!f.is_excluded("src/lib.rs"));
    }

    #[test]
    fn include_list_admits_matching_files_only() {
        let f = filter(&["**/*.rs", "**/*.java"], &[]);
        assert!(!f.is_excluded("src/lib.rs"));
        assert!(!f.is_excluded("src/main/java/App.java"));
        assert!(f.is_excluded("README.md"));
        assert!(f.is_excluded("data/seed.csv"));
    }

    #[test]
    fn exclude_wins_over_include() {
        let f = filter(&["**/*.rs"], &["src/generated/**"]);
        assert!(!f.is_excluded("src/lib.rs"));
        assert!(f.is_excluded("src/generated/bindings.rs"));
    }

    #[test]
    fn invalid_glob_is_dropped_not_fatal() {
        let f = filter(&[], &["[unclosed", "**/*.csv"]);
        assert!(f.is_excluded("data/seed.csv"));
        assert!(!f.is_excluded("src/lib.rs"));
    }

    #[test]
    fn gitignore_deviation_marks_filter_active() {
        let f = IndexFileFilter::from_lists(&[], &[], false);
        assert!(f.is_active());
        assert_eq!(f.summary().as_deref(), Some("gitignore=off"));
    }

    #[test]
    fn summary_lists_both_axes() {
        let f = filter(&["**/*.rs"], &["**/*.csv", "**/*.jsonl"]);
        let s = f.summary().expect("active filter has a summary");
        assert!(s.contains("include=[**/*.rs]"));
        assert!(s.contains("exclude=[**/*.csv, **/*.jsonl]"));
    }
}
