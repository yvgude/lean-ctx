//! Active Inference Preload — predictive context loading.
//!
//! Uses the agent's task description and recent interactions to predict
//! which providers and resources will be needed next, then preloads them
//! into the session cache before the agent asks.
//!
//! Scientific basis: Active Inference (Friston 2010; Parr, Pezzulo & Friston 2022).
//! The system acts to reduce expected surprise by preloading context that
//! minimizes the predicted free energy of future queries.
//!
//! Strategy:
//!   1. Parse task keywords → predict relevant provider actions
//!   2. Score predictions using the provider bandit
//!   3. Preload top-k predictions into session cache

use crate::core::provider_bandit::ProviderBandit;

/// A predicted preload action.
#[derive(Debug, Clone)]
pub struct PreloadPrediction {
    pub provider_id: String,
    pub action: String,
    pub confidence: f64,
    pub reason: String,
}

/// Keyword → provider action mappings.
static KEYWORD_MAPPINGS: &[(&[&str], &str, &str)] = &[
    (
        &["bug", "error", "crash", "fix", "broken", "issue", "defect"],
        "github",
        "issues",
    ),
    (
        &["bug", "error", "crash", "fix", "broken", "issue", "defect"],
        "jira",
        "issues",
    ),
    (
        &["pr", "pull", "merge", "review", "branch"],
        "github",
        "pull_requests",
    ),
    (
        &[
            "database",
            "table",
            "schema",
            "column",
            "migration",
            "sql",
            "db",
        ],
        "postgres",
        "schemas",
    ),
    (
        &["sprint", "story", "epic", "velocity", "backlog"],
        "jira",
        "sprints",
    ),
    (
        &["wiki", "doc", "documentation", "guide", "howto"],
        "github",
        "issues",
    ),
];

/// Predict which provider actions should be preloaded based on the task.
pub fn predict_preloads(
    task_description: &str,
    available_providers: &[String],
    bandit: &mut ProviderBandit,
    max_predictions: usize,
) -> Vec<PreloadPrediction> {
    let task_lower = task_description.to_lowercase();
    let task_words: Vec<&str> = task_lower.split_whitespace().collect();

    let mut predictions: Vec<PreloadPrediction> = Vec::new();

    for &(keywords, provider, action) in KEYWORD_MAPPINGS {
        if !available_providers.iter().any(|p| p == provider) {
            continue;
        }

        let matching_keywords: Vec<&&str> = keywords
            .iter()
            .filter(|kw| task_words.iter().any(|tw| tw.contains(*kw)))
            .collect();

        if matching_keywords.is_empty() {
            continue;
        }

        let keyword_confidence = matching_keywords.len() as f64 / keywords.len() as f64;

        let task_type = infer_task_type(&task_lower);
        let bandit_score = bandit.estimated_probability(&task_type, provider);

        let combined = 0.6 * keyword_confidence + 0.4 * bandit_score;

        if !predictions
            .iter()
            .any(|p| p.provider_id == provider && p.action == action)
        {
            predictions.push(PreloadPrediction {
                provider_id: provider.to_string(),
                action: action.to_string(),
                confidence: combined,
                reason: format!(
                    "keywords: {}",
                    matching_keywords
                        .iter()
                        .map(|k| **k)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
    }

    predictions.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    predictions.truncate(max_predictions);
    predictions
}

/// Simple task type inference from keywords. Public so the preload feedback loop
/// can bucket outcomes by the same task type the prediction was scored under.
#[must_use]
pub fn infer_task_type(task: &str) -> String {
    if task.contains("bug")
        || task.contains("fix")
        || task.contains("error")
        || task.contains("crash")
    {
        "bugfix".into()
    } else if task.contains("feature") || task.contains("add") || task.contains("implement") {
        "feature".into()
    } else if task.contains("refactor") || task.contains("clean") || task.contains("improve") {
        "refactor".into()
    } else {
        "general".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predict_bug_fix_suggests_issues() {
        let mut bandit = ProviderBandit::new();
        let providers = vec!["github".into(), "jira".into()];

        let predictions = predict_preloads(
            "Fix the authentication bug in the login flow",
            &providers,
            &mut bandit,
            5,
        );

        assert!(!predictions.is_empty());
        assert!(
            predictions
                .iter()
                .any(|p| p.provider_id == "github" && p.action == "issues")
        );
    }

    #[test]
    fn predict_db_task_suggests_schemas() {
        let mut bandit = ProviderBandit::new();
        let providers = vec!["postgres".into()];

        let predictions = predict_preloads(
            "Add a new column to the users database table",
            &providers,
            &mut bandit,
            5,
        );

        assert!(
            predictions
                .iter()
                .any(|p| p.provider_id == "postgres" && p.action == "schemas")
        );
    }

    #[test]
    fn predict_pr_review_suggests_pull_requests() {
        let mut bandit = ProviderBandit::new();
        let providers = vec!["github".into()];

        let predictions = predict_preloads(
            "Review the open pull requests and merge the approved ones",
            &providers,
            &mut bandit,
            5,
        );

        assert!(
            predictions
                .iter()
                .any(|p| p.provider_id == "github" && p.action == "pull_requests")
        );
    }

    #[test]
    fn predict_empty_task_returns_empty() {
        let mut bandit = ProviderBandit::new();
        let predictions = predict_preloads("", &["github".into()], &mut bandit, 5);
        assert!(predictions.is_empty());
    }

    #[test]
    fn predict_unavailable_provider_skipped() {
        let mut bandit = ProviderBandit::new();
        let predictions = predict_preloads(
            "Fix the database schema migration",
            &["github".into()], // postgres not available
            &mut bandit,
            5,
        );

        assert!(!predictions.iter().any(|p| p.provider_id == "postgres"));
    }

    #[test]
    fn predict_respects_max_predictions() {
        let mut bandit = ProviderBandit::new();
        let providers = vec!["github".into(), "jira".into(), "postgres".into()];

        let predictions = predict_preloads(
            "Fix the bug in database schema and review pull requests",
            &providers,
            &mut bandit,
            2,
        );

        assert!(predictions.len() <= 2);
    }

    #[test]
    fn predict_bandit_trained_boosts_confidence() {
        let mut bandit = ProviderBandit::new();
        for _ in 0..20 {
            bandit.update("bugfix", "github", true);
            bandit.update("bugfix", "jira", false);
        }

        let providers = vec!["github".into(), "jira".into()];
        let predictions = predict_preloads(
            "Fix the crash bug in authentication",
            &providers,
            &mut bandit,
            5,
        );

        let gh = predictions
            .iter()
            .find(|p| p.provider_id == "github" && p.action == "issues");
        let jira = predictions
            .iter()
            .find(|p| p.provider_id == "jira" && p.action == "issues");

        if let (Some(gh), Some(jira)) = (gh, jira) {
            assert!(
                gh.confidence > jira.confidence,
                "Trained bandit should boost github over jira"
            );
        }
    }

    #[test]
    fn infer_task_type_correctness() {
        assert_eq!(infer_task_type("fix the crash bug"), "bugfix");
        assert_eq!(infer_task_type("add new feature"), "feature");
        assert_eq!(infer_task_type("refactor the auth module"), "refactor");
        assert_eq!(infer_task_type("update documentation"), "general");
    }
}
