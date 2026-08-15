use crate::core::triage::{TaskAnalysisInput, TriageEngine};
use serde_json::Value;

const TRUNCATION_SCORE: f64 = 0.3;
const MAX_PRUNED_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreOptimizeResult {
    pub task_class: String,
    pub complexity: String,
    pub tokens_pruned: usize,
    pub original_token_estimate: usize,
}

/// Classify the latest user request and trim sufficiently old, low-relevance
/// message text before the main prompt optimizer runs.
pub fn pre_optimize(body: &mut Value) -> Option<PreOptimizeResult> {
    let messages = body.get_mut("messages")?.as_array_mut()?;
    let last_user = messages
        .iter()
        .rposition(|message| message_role(message) == Some("user"))?;
    let triage = TriageEngine::default();
    let (task_class, complexity) =
        classify_and_complexity_with_triage(message_text(&messages[last_user]), &triage);
    #[cfg(feature = "enterprise")]
    crate::core::context_prefetch::plan_after_triage(&task_class);
    let original_token_estimate = estimate_tokens(messages.iter().map(message_char_count).sum());

    for (index, message) in messages.iter_mut().enumerate() {
        if relevance_score(message_role(message), index, last_user) < TRUNCATION_SCORE {
            truncate_message(message);
        }
    }

    let optimized_token_estimate = estimate_tokens(messages.iter().map(message_char_count).sum());
    Some(PreOptimizeResult {
        complexity,
        task_class: task_class.clone(),
        tokens_pruned: original_token_estimate.saturating_sub(optimized_token_estimate),
        original_token_estimate,
    })
}

pub fn classify_task(message: Option<&str>) -> &'static str {
    let text = message.unwrap_or_default().to_ascii_lowercase();

    if contains_any(&text, &["refactor", "cleanup", "clean up", "restructure"]) {
        "refactor"
    } else if contains_any(&text, &["debug", "diagnose", "trace", "investigate"]) {
        "debug"
    } else if contains_any(
        &text,
        &["fix", "bug", "broken", "regression", "repair", "error"],
    ) {
        "coding_fix"
    } else if contains_any(
        &text,
        &["implement", "add", "create", "build", "feature", "write"],
    ) {
        "coding_new"
    } else {
        "question"
    }
}

fn classify_with_triage(message: Option<&str>, engine: &TriageEngine) -> String {
    engine
        .analyze(&TaskAnalysisInput {
            query: message.unwrap_or_default().to_owned(),
            ..TaskAnalysisInput::default()
        })
        .map(|analysis| {
            task_class_for_policy(&analysis.profile.task_class, &analysis.profile.intent)
        })
        .unwrap_or_else(|_| classify_task(message).to_owned())
}

fn classify_and_complexity_with_triage(
    message: Option<&str>,
    engine: &TriageEngine,
) -> (String, String) {
    engine
        .analyze(&TaskAnalysisInput {
            query: message.unwrap_or_default().to_owned(),
            ..TaskAnalysisInput::default()
        })
        .map(|analysis| {
            (
                task_class_for_policy(&analysis.profile.task_class, &analysis.profile.intent),
                analysis.profile.complexity,
            )
        })
        .unwrap_or_else(|_| (classify_task(message).to_owned(), "low".to_owned()))
}

fn task_class_for_policy(profile_task_class: &str, intent: &str) -> String {
    match intent {
        "simple" => "simple".to_owned(),
        "coding_fix" | "coding_new" | "refactor" => intent.to_owned(),
        "debug" | "debugging" => "debugging".to_owned(),
        "explore" | "exploration" | "research" => "exploration".to_owned(),
        _ => match profile_task_class {
            "coding" | "coding_fix" | "coding_new" | "refactor" | "debugging" | "exploration"
            | "research" => profile_task_class.to_owned(),
            _ => "coding".to_owned(),
        },
    }
}

fn contains_any(text: &str, keywords: &[&str]) -> bool {
    keywords.iter().any(|keyword| text.contains(keyword))
}

fn relevance_score(role: Option<&str>, index: usize, last_user: usize) -> f64 {
    if role == Some("system") || index == last_user {
        return 1.0;
    }

    let base = if role == Some("assistant") { 0.8 } else { 1.0 };
    let age = last_user.saturating_sub(index) as i32;
    base * 0.9_f64.powi(age)
}

fn message_role(message: &Value) -> Option<&str> {
    message.get("role")?.as_str()
}

fn message_text(message: &Value) -> Option<&str> {
    message.get("content")?.as_str()
}

fn message_char_count(message: &Value) -> usize {
    message_text(message).map_or(0, |content| content.chars().count())
}

fn estimate_tokens(characters: usize) -> usize {
    characters.div_ceil(4)
}

fn truncate_message(message: &mut Value) {
    let Some(Value::String(content)) = message.get_mut("content") else {
        return;
    };
    let Some((byte_index, _)) = content.char_indices().nth(MAX_PRUNED_CHARS) else {
        return;
    };
    content.truncate(byte_index);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn returns_none_without_a_user_message() {
        let mut body = json!({"messages": [{"role": "assistant", "content": "hello"}]});

        assert_eq!(pre_optimize(&mut body), None);
    }

    #[test]
    fn classifies_task_kinds() {
        for (content, _expected_category) in [
            ("Please fix this broken endpoint", "coding_fix"),
            ("Implement a new endpoint", "coding"),
            ("Refactor the request pipeline", "refactor"),
            ("What does this endpoint do?", "exploration"),
            ("Debug this intermittent failure", "debug"),
        ] {
            let mut body = json!({"messages": [{"role": "user", "content": content}]});
            let result = pre_optimize(&mut body);
            assert!(result.is_some(), "should classify: {content}");
        }
    }

    #[derive(Debug)]
    struct FailingAnalyzer;

    impl crate::core::triage::TaskAnalyzer for FailingAnalyzer {
        fn analyze(
            &self,
            _: &crate::core::triage::TaskAnalysisInput,
        ) -> Result<crate::core::triage::ProfileHypothesis, crate::core::triage::TriageError>
        {
            Err(crate::core::triage::TriageError::InternalError(
                "forced failure".to_owned(),
            ))
        }

        fn name(&self) -> &'static str {
            "failing"
        }
    }

    #[test]
    fn uses_triage_intent_for_adaptive_policy_class() {
        let engine = TriageEngine::with_rules();

        assert_eq!(
            classify_with_triage(Some("Explain how this works"), &engine),
            "exploration"
        );
    }

    #[test]
    fn falls_back_to_heuristic_when_triage_fails() {
        let engine = TriageEngine::new(vec![Box::new(FailingAnalyzer)]);

        assert_eq!(
            classify_with_triage(Some("Implement a new endpoint"), &engine),
            "coding_new"
        );
    }

    #[test]
    fn classifies_the_last_user_message() {
        let mut body = json!({
            "messages": [
                {"role": "user", "content": "Implement an endpoint"},
                {"role": "assistant", "content": "Which endpoint?"},
                {"role": "user", "content": "Why does it return 404?"}
            ]
        });

        assert!(pre_optimize(&mut body).is_some());
    }

    #[test]
    fn prunes_old_low_relevance_messages_and_counts_tokens() {
        let old_content = "a".repeat(300);
        let mut messages: Vec<Value> = (0..12)
            .map(|_| json!({"role": "assistant", "content": old_content}))
            .collect();
        messages.push(json!({"role": "user", "content": "What changed?"}));
        let mut body = json!({"messages": messages});

        let result = pre_optimize(&mut body).unwrap();

        assert_eq!(
            body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            200
        );
        assert_eq!(
            body["messages"][4]["content"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            300
        );
        assert!(result.tokens_pruned > 0);
        assert!(result.original_token_estimate > result.tokens_pruned);
    }

    #[test]
    fn preserves_system_and_latest_user_messages() {
        let system = "s".repeat(400);
        let user = "u".repeat(400);
        let mut body = json!({
            "messages": [
                {"role": "system", "content": system},
                {"role": "assistant", "content": "a".repeat(400)},
                {"role": "user", "content": user}
            ]
        });

        pre_optimize(&mut body).unwrap();

        assert_eq!(
            body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            400
        );
        assert_eq!(
            body["messages"][2]["content"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            400
        );
    }

    #[test]
    #[cfg(feature = "enterprise")]
    fn triage_creates_a_prefetch_plan_from_live_read_trajectory() {
        let first = "src/r4_prefetch_trajectory_first.rs";
        let predicted = "src/r4_prefetch_trajectory_predicted.rs";
        for path in [first, predicted, first] {
            crate::core::context_prefetch::record_file_read(path);
        }

        let mut body = json!({
            "messages": [{"role": "user", "content": "Please fix this endpoint"}]
        });

        assert!(pre_optimize(&mut body).is_some());
        assert!(crate::core::context_prefetch::is_prefetch_prediction(
            predicted
        ));
    }

    #[test]
    fn benchmark_pre_optimize() {
        use std::time::Instant;

        let messages: Vec<Value> = (0..20)
            .map(|index| {
                let role = if index == 19 { "user" } else { "assistant" };
                let content = if index == 19 {
                    "Please fix this broken endpoint".to_owned()
                } else {
                    "a".repeat(400)
                };
                json!({"role": role, "content": content})
            })
            .collect();
        let body = json!({"messages": messages});
        let mut bodies = vec![body; 1_000];

        let started = Instant::now();
        for body in &mut bodies {
            assert!(pre_optimize(body).is_some());
        }
        let average = started.elapsed() / 1_000;
        println!("benchmark_pre_optimize: {average:?} per call");
    }
}
