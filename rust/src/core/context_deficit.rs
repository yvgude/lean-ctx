use super::context_ledger::{ContextLedger, PressureAction};
use super::intent_engine::{IntentScope, StructuredIntent, TaskType};

#[derive(Debug, Clone)]
pub struct ContextDeficit {
    pub missing_targets: Vec<String>,
    pub suggested_files: Vec<SuggestedFile>,
    pub pressure_action: PressureAction,
    pub budget_remaining: usize,
}

#[derive(Debug, Clone)]
pub struct SuggestedFile {
    pub path: String,
    pub reason: DeficitReason,
    pub estimated_tokens: usize,
    pub recommended_mode: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeficitReason {
    TargetNotLoaded,
    DependencyOfTarget,
    TestFileForTarget,
    ConfigForTask,
}

impl DeficitReason {
    fn priority(&self) -> u8 {
        match self {
            Self::TargetNotLoaded => 0,
            Self::DependencyOfTarget => 1,
            Self::TestFileForTarget => 2,
            Self::ConfigForTask => 3,
        }
    }
}

pub fn detect_deficit(
    ledger: &ContextLedger,
    intent: &StructuredIntent,
    known_files: &[String],
) -> ContextDeficit {
    let loaded_paths: Vec<&str> = ledger.entries.iter().map(|e| e.path.as_str()).collect();
    let pressure = ledger.pressure();

    let mut missing_targets = Vec::new();
    let mut suggestions = Vec::new();

    for target in &intent.targets {
        if target.contains('.') || target.contains('/') {
            let is_loaded = loaded_paths
                .iter()
                .any(|p| p.ends_with(target) || p.contains(target));
            if !is_loaded {
                missing_targets.push(target.clone());

                let matching: Vec<&String> = known_files
                    .iter()
                    .filter(|f| f.ends_with(target) || f.contains(target))
                    .collect();

                for file in matching {
                    let mode = mode_for_pressure(&pressure.recommendation, &intent.scope);
                    suggestions.push(SuggestedFile {
                        path: file.clone(),
                        reason: DeficitReason::TargetNotLoaded,
                        estimated_tokens: estimate_tokens_for_mode(&mode),
                        recommended_mode: mode,
                    });
                }
            }
        }
    }

    if intent.task_type == TaskType::FixBug || intent.task_type == TaskType::Test {
        for target in &intent.targets {
            if target.contains('.') || target.contains('/') {
                let test_patterns = derive_test_paths(target);
                for test_path in &test_patterns {
                    let matching: Vec<&String> = known_files
                        .iter()
                        .filter(|f| f.contains(test_path))
                        .collect();
                    for file in matching {
                        if !loaded_paths.contains(&file.as_str())
                            && !suggestions.iter().any(|s| s.path == *file)
                        {
                            let mode = mode_for_pressure(&pressure.recommendation, &intent.scope);
                            suggestions.push(SuggestedFile {
                                path: file.clone(),
                                reason: DeficitReason::TestFileForTarget,
                                estimated_tokens: estimate_tokens_for_mode(&mode),
                                recommended_mode: mode,
                            });
                        }
                    }
                }
            }
        }
    }

    if intent.task_type == TaskType::Config || intent.task_type == TaskType::Deploy {
        let config_patterns = [
            "Cargo.toml",
            "package.json",
            "tsconfig.json",
            "pyproject.toml",
            ".env",
            "Dockerfile",
        ];
        for pattern in &config_patterns {
            let matching: Vec<&String> = known_files
                .iter()
                .filter(|f| f.ends_with(pattern))
                .collect();
            for file in matching {
                if !loaded_paths.contains(&file.as_str())
                    && !suggestions.iter().any(|s| s.path == *file)
                {
                    suggestions.push(SuggestedFile {
                        path: file.clone(),
                        reason: DeficitReason::ConfigForTask,
                        estimated_tokens: estimate_tokens_for_mode("full"),
                        recommended_mode: "full".to_string(),
                    });
                }
            }
        }
    }

    suggestions.sort_by_key(|s| s.reason.priority());

    let budget_remaining = pressure.remaining_tokens;
    let mut cumulative = 0usize;
    suggestions.retain(|s| {
        cumulative += s.estimated_tokens;
        cumulative <= budget_remaining
    });

    ContextDeficit {
        missing_targets,
        suggested_files: suggestions,
        pressure_action: pressure.recommendation,
        budget_remaining,
    }
}

fn mode_for_pressure(action: &PressureAction, scope: &IntentScope) -> String {
    match action {
        PressureAction::EvictLeastRelevant => "reference".to_string(),
        PressureAction::ForceCompression => "signatures".to_string(),
        PressureAction::SuggestCompression => match scope {
            IntentScope::SingleFile => "full".to_string(),
            IntentScope::MultiFile => "signatures".to_string(),
            IntentScope::CrossModule | IntentScope::ProjectWide => "map".to_string(),
        },
        PressureAction::NoAction => match scope {
            IntentScope::SingleFile | IntentScope::MultiFile => "full".to_string(),
            IntentScope::CrossModule => "signatures".to_string(),
            IntentScope::ProjectWide => "map".to_string(),
        },
    }
}

fn estimate_tokens_for_mode(mode: &str) -> usize {
    match mode {
        "full" => 2000,
        "signatures" => 400,
        "map" => 200,
        "reference" => 50,
        "aggressive" => 800,
        _ => 1000,
    }
}

fn derive_test_paths(file_path: &str) -> Vec<String> {
    let stem = std::path::Path::new(file_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if stem.is_empty() {
        return Vec::new();
    }
    vec![
        format!("{stem}_test"),
        format!("test_{stem}"),
        format!("{stem}.test"),
        format!("{stem}.spec"),
        format!("{stem}_spec"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_missing_target() {
        let ledger = ContextLedger::new();
        let intent = StructuredIntent::from_query("fix bug in auth.rs");
        let known = vec!["src/auth.rs".to_string(), "src/db.rs".to_string()];
        let deficit = detect_deficit(&ledger, &intent, &known);
        assert!(!deficit.missing_targets.is_empty());
        assert!(deficit
            .suggested_files
            .iter()
            .any(|s| s.path.contains("auth")));
    }

    #[test]
    fn no_deficit_when_loaded() {
        let mut ledger = ContextLedger::new();
        ledger.record("src/auth.rs", "full", 500, 500);
        let intent = StructuredIntent::from_query("fix bug in auth.rs");
        let known = vec!["src/auth.rs".to_string()];
        let deficit = detect_deficit(&ledger, &intent, &known);
        assert!(deficit.missing_targets.is_empty());
    }

    #[test]
    fn suggests_test_files_for_fixbug() {
        let mut ledger = ContextLedger::new();
        ledger.record("src/auth.rs", "full", 500, 500);
        let intent = StructuredIntent::from_query("fix bug in auth.rs");
        let known = vec!["src/auth.rs".to_string(), "tests/auth_test.rs".to_string()];
        let deficit = detect_deficit(&ledger, &intent, &known);
        let test_suggestions: Vec<_> = deficit
            .suggested_files
            .iter()
            .filter(|s| s.reason == DeficitReason::TestFileForTarget)
            .collect();
        assert!(
            !test_suggestions.is_empty(),
            "should suggest test files for FixBug"
        );
    }

    #[test]
    fn respects_budget() {
        let mut ledger = ContextLedger::with_window_size(1000);
        ledger.record("existing.rs", "full", 900, 900);
        let intent = StructuredIntent::from_query("fix bug in big_file.rs");
        let known = vec!["src/big_file.rs".to_string()];
        let deficit = detect_deficit(&ledger, &intent, &known);
        assert!(
            deficit.suggested_files.is_empty() || deficit.budget_remaining < 200,
            "should respect budget constraints"
        );
    }

    #[test]
    fn config_task_suggests_config_files() {
        let ledger = ContextLedger::new();
        let intent = StructuredIntent::from_query("configure env settings for the project");
        let known = vec![
            "src/main.rs".to_string(),
            "Cargo.toml".to_string(),
            "package.json".to_string(),
        ];
        let deficit = detect_deficit(&ledger, &intent, &known);
        let config_suggestions: Vec<_> = deficit
            .suggested_files
            .iter()
            .filter(|s| s.reason == DeficitReason::ConfigForTask)
            .collect();
        assert!(!config_suggestions.is_empty());
    }

    #[test]
    fn mode_adapts_to_pressure() {
        let mode_low = mode_for_pressure(&PressureAction::NoAction, &IntentScope::SingleFile);
        let mode_high = mode_for_pressure(
            &PressureAction::EvictLeastRelevant,
            &IntentScope::SingleFile,
        );
        assert_eq!(mode_low, "full");
        assert_eq!(mode_high, "reference");
    }

    #[test]
    fn derive_test_paths_generates_variants() {
        let paths = derive_test_paths("src/auth.rs");
        assert!(paths.contains(&"auth_test".to_string()));
        assert!(paths.contains(&"test_auth".to_string()));
        assert!(paths.contains(&"auth.test".to_string()));
        assert!(paths.contains(&"auth.spec".to_string()));
    }
}
