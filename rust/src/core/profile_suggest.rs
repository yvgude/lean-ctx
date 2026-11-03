//! Repo-stack-aware profile recommendation (#851).
//!
//! Powers `lean-ctx profile suggest`: scan the current repo for deterministic
//! signals (languages, source-file count, monorepo layout, build/CI markers,
//! configured LLM providers) and recommend a context profile plus a few key
//! settings (`history_mode`, output density, `effort`).
//!
//! Strictly **read-only and local-only**: it prints a suggestion and the exact
//! commands to apply it, and never writes config. All signals come from the
//! filesystem + local config/env, so the output is a deterministic function of
//! the repo and environment (no network, no telemetry).
//!
//! Reuses existing detectors rather than reinventing them:
//! [`language_for_ext`] for language classification and
//! [`crate::core::pathutil::has_multi_repo_children`] for the monorepo check.
//! The pure mapping ([`suggest`]) is separated from the I/O scan ([`analyze`])
//! so the heuristic is unit-tested without touching disk.

use std::collections::BTreeMap;
use std::path::Path;

use crate::core::config::Config;
use crate::core::language_capabilities::language_for_ext;

/// A repo is "large" past this many indexed source files (favors broad context).
const LARGE_REPO_FILES: usize = 2000;
/// A repo is "small" at or below this many source files (a focused default fits).
const SMALL_REPO_FILES: usize = 60;
/// This many distinct languages counts as polyglot (favors broad context).
const POLYGLOT_LANGS: usize = 4;
/// Hard cap on files visited during the scan, so `suggest` stays fast on huge trees.
const MAX_WALK_FILES: usize = 50_000;

/// Files counted for one language. Serialized for `--json`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LanguageCount {
    pub language: String,
    pub files: usize,
}

/// Deterministic, locally-detected signals about the repo + environment.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RepoSignals {
    pub root: String,
    pub source_files: usize,
    pub languages: Vec<LanguageCount>,
    pub monorepo: bool,
    pub workspace_markers: Vec<String>,
    pub build_markers: Vec<String>,
    pub ci: bool,
    pub providers: Vec<String>,
    pub proxy_enabled: bool,
}

/// Key settings the suggestion recommends alongside the profile.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RecommendedSettings {
    /// `proxy.history_mode` — `None` ⇒ leave the default untouched.
    pub history_mode: Option<String>,
    /// `output_density`.
    pub output_density: String,
    /// `proxy.effort` — `None` ⇒ leave off (opt-in; never inferred from a repo).
    pub effort: Option<String>,
}

/// A task-oriented profile the user can switch to, with the situation it fits.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProfileAlternative {
    pub profile: String,
    pub when: String,
}

/// The full recommendation: a primary profile, why, the settings, and
/// task-oriented alternatives.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Suggestion {
    pub profile: String,
    pub rationale: Vec<String>,
    pub settings: RecommendedSettings,
    pub alternatives: Vec<ProfileAlternative>,
}

/// Scans `root` and collects [`RepoSignals`]. Respects `.gitignore` (so vendored
/// / build dirs don't skew the language mix) and is bounded by `MAX_WALK_FILES`.
#[must_use]
pub fn analyze(root: &str) -> RepoSignals {
    let root_path = Path::new(root);

    let mut lang_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut source_files = 0usize;

    for entry in ignore::WalkBuilder::new(root_path)
        .standard_filters(true)
        .build()
        .flatten()
    {
        if source_files >= MAX_WALK_FILES {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if let Some(lang) = language_for_ext(ext) {
            *lang_counts.entry(lang.id_str()).or_insert(0) += 1;
            source_files += 1;
        }
    }

    let mut languages: Vec<LanguageCount> = lang_counts
        .into_iter()
        .map(|(language, files)| LanguageCount {
            language: language.to_string(),
            files,
        })
        .collect();
    // Stable order: most files first, ties broken by name.
    languages.sort_by(|a, b| {
        b.files
            .cmp(&a.files)
            .then_with(|| a.language.cmp(&b.language))
    });

    let workspace_markers = detect_workspace_markers(root_path);
    let monorepo =
        !workspace_markers.is_empty() || crate::core::pathutil::has_multi_repo_children(root_path);

    let build_markers = detect_build_markers(root_path);
    let ci = root_path.join(".github/workflows").is_dir()
        || root_path.join(".gitlab-ci.yml").is_file()
        || root_path.join(".circleci").is_dir()
        || root_path.join("azure-pipelines.yml").is_file();

    let cfg = Config::load();
    let providers = detect_providers(&cfg);
    let proxy_enabled = cfg.proxy_enabled.unwrap_or(false);

    RepoSignals {
        root: root.to_string(),
        source_files,
        languages,
        monorepo,
        workspace_markers,
        build_markers,
        ci,
        providers,
        proxy_enabled,
    }
}

/// Maps signals to a recommendation. Pure and deterministic (no I/O), so the
/// heuristic is fully unit-tested.
#[must_use]
pub fn suggest(signals: &RepoSignals) -> Suggestion {
    let polyglot = signals.languages.len() >= POLYGLOT_LANGS;
    let large = signals.source_files >= LARGE_REPO_FILES;
    let small = signals.source_files <= SMALL_REPO_FILES;

    let mut rationale = Vec::new();

    let profile = if signals.monorepo {
        rationale.push("monorepo layout → broad cross-package context".to_string());
        "exploration"
    } else if large {
        rationale.push(format!(
            "large repo ({} source files) → wider, map-first context",
            signals.source_files
        ));
        "exploration"
    } else if polyglot {
        rationale.push(format!(
            "polyglot ({} languages) → wider context to span stacks",
            signals.languages.len()
        ));
        "exploration"
    } else if small {
        rationale.push(format!(
            "small repo ({} source files) → a focused default is enough",
            signals.source_files
        ));
        "coder"
    } else {
        rationale.push(format!(
            "typical project size ({} source files) → balanced default",
            signals.source_files
        ));
        "coder"
    };

    let broad = signals.monorepo || large || polyglot;
    let output_density = if broad { "terse" } else { "normal" };
    if broad {
        rationale.push("dense output (terse) to fit the larger surface in budget".to_string());
    }

    let history_mode = if signals.proxy_enabled || !signals.providers.is_empty() {
        rationale
            .push("provider proxy active → cache-aware history (cache-stable pruning)".to_string());
        Some("cache-aware".to_string())
    } else {
        None
    };

    // `effort` is a cost/latency knob with no repo signal — never inferred here.
    let effort = None;

    let mut alternatives = Vec::new();
    if signals.ci {
        alternatives.push(ProfileAlternative {
            profile: "ci-debug".to_string(),
            when: "iterating on CI / shell failures".to_string(),
        });
    }
    alternatives.push(ProfileAlternative {
        profile: "hotfix".to_string(),
        when: "urgent one-file fix — minimal context".to_string(),
    });
    alternatives.push(ProfileAlternative {
        profile: "bugfix".to_string(),
        when: "debugging a specific issue".to_string(),
    });
    alternatives.push(ProfileAlternative {
        profile: "review".to_string(),
        when: "read-only code review".to_string(),
    });

    Suggestion {
        profile: profile.to_string(),
        rationale,
        settings: RecommendedSettings {
            history_mode,
            output_density: output_density.to_string(),
            effort,
        },
        alternatives,
    }
}

/// Monorepo / workspace marker files at the repo root (deterministic file probes).
fn detect_workspace_markers(root: &Path) -> Vec<String> {
    const MARKERS: &[&str] = &[
        "pnpm-workspace.yaml",
        "lerna.json",
        "nx.json",
        "turbo.json",
        "rush.json",
        "go.work",
    ];
    let mut found: Vec<String> = MARKERS
        .iter()
        .filter(|m| root.join(m).exists())
        .map(|m| (*m).to_string())
        .collect();
    // A Cargo workspace is expressed inside Cargo.toml rather than its own file.
    if std::fs::read_to_string(root.join("Cargo.toml"))
        .is_ok_and(|s| s.lines().any(|l| l.trim_start().starts_with("[workspace]")))
    {
        found.push("Cargo.toml [workspace]".to_string());
    }
    found
}

/// Build-tool markers at the repo root, de-duplicated by tool name.
fn detect_build_markers(root: &Path) -> Vec<String> {
    const MARKERS: &[(&str, &str)] = &[
        ("Cargo.toml", "cargo"),
        ("package.json", "npm"),
        ("go.mod", "go"),
        ("pyproject.toml", "python"),
        ("requirements.txt", "python"),
        ("setup.py", "python"),
        ("pom.xml", "maven"),
        ("build.gradle", "gradle"),
        ("Gemfile", "bundler"),
        ("composer.json", "composer"),
        ("Makefile", "make"),
        ("Dockerfile", "docker"),
    ];
    let mut found: Vec<String> = Vec::new();
    for (file, name) in MARKERS {
        if root.join(file).exists() && !found.iter().any(|f| f == name) {
            found.push((*name).to_string());
        }
    }
    found
}

/// LLM providers in use, from local config upstreams + environment API keys.
fn detect_providers(cfg: &Config) -> Vec<String> {
    let mut providers: Vec<String> = Vec::new();

    if env_set("ANTHROPIC_API_KEY") || env_set("ANTHROPIC_AUTH_TOKEN") {
        push_unique(&mut providers, "anthropic");
    }
    if env_set("OPENAI_API_KEY") {
        push_unique(&mut providers, "openai");
    }
    if env_set("GEMINI_API_KEY") || env_set("GOOGLE_API_KEY") {
        push_unique(&mut providers, "gemini");
    }
    if cfg.proxy.anthropic_upstream.is_some() {
        push_unique(&mut providers, "anthropic");
    }
    if cfg.proxy.openai_upstream.is_some() {
        push_unique(&mut providers, "openai");
    }
    if cfg.proxy.chatgpt_upstream.is_some() {
        push_unique(&mut providers, "chatgpt");
    }
    if cfg.proxy.gemini_upstream.is_some() {
        push_unique(&mut providers, "gemini");
    }

    providers.sort();
    providers
}

fn push_unique(v: &mut Vec<String>, name: &str) {
    if !v.iter().any(|p| p == name) {
        v.push(name.to_string());
    }
}

fn env_set(key: &str) -> bool {
    std::env::var(key).is_ok_and(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals(source_files: usize, langs: &[&str], monorepo: bool) -> RepoSignals {
        RepoSignals {
            root: "/tmp/x".to_string(),
            source_files,
            languages: langs
                .iter()
                .map(|l| LanguageCount {
                    language: (*l).to_string(),
                    files: 1,
                })
                .collect(),
            monorepo,
            workspace_markers: vec![],
            build_markers: vec![],
            ci: false,
            providers: vec![],
            proxy_enabled: false,
        }
    }

    #[test]
    fn monorepo_suggests_exploration() {
        let s = suggest(&signals(300, &["rust", "typescript"], true));
        assert_eq!(s.profile, "exploration");
        assert_eq!(s.settings.output_density, "terse");
    }

    #[test]
    fn large_repo_suggests_exploration() {
        let s = suggest(&signals(5000, &["rust"], false));
        assert_eq!(s.profile, "exploration");
        assert_eq!(s.settings.output_density, "terse");
    }

    #[test]
    fn polyglot_suggests_exploration() {
        let s = suggest(&signals(
            300,
            &["rust", "go", "python", "typescript"],
            false,
        ));
        assert_eq!(s.profile, "exploration");
    }

    #[test]
    fn small_repo_suggests_coder_normal_density() {
        let s = suggest(&signals(20, &["rust"], false));
        assert_eq!(s.profile, "coder");
        assert_eq!(s.settings.output_density, "normal");
    }

    #[test]
    fn typical_repo_suggests_coder() {
        let s = suggest(&signals(400, &["rust", "typescript"], false));
        assert_eq!(s.profile, "coder");
        assert_eq!(s.settings.output_density, "normal");
    }

    #[test]
    fn effort_is_never_inferred() {
        let s = suggest(&signals(5000, &["rust", "go", "python", "ts"], true));
        assert!(s.settings.effort.is_none());
    }

    #[test]
    fn history_mode_recommended_only_with_providers() {
        let mut s = signals(400, &["rust"], false);
        assert!(suggest(&s).settings.history_mode.is_none());
        s.providers = vec!["anthropic".to_string()];
        assert_eq!(
            suggest(&s).settings.history_mode.as_deref(),
            Some("cache-aware")
        );
    }

    #[test]
    fn ci_adds_ci_debug_alternative_first() {
        let mut s = signals(400, &["rust"], false);
        s.ci = true;
        let out = suggest(&s);
        assert_eq!(out.alternatives.first().unwrap().profile, "ci-debug");
    }

    #[test]
    fn suggestion_is_deterministic() {
        let s = signals(5000, &["rust", "go"], true);
        let a = suggest(&s);
        let b = suggest(&s);
        assert_eq!(a.profile, b.profile);
        assert_eq!(a.rationale, b.rationale);
    }
}
