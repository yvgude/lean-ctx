//! Append-only semantic-shadow comparison and accuracy reporting.

use std::io::Write;

use lean_ctx_time::UtcDateTime;
use serde::{Deserialize, Serialize};

use super::{ProfileHypothesis, TaskAnalysisInput};

const SHADOW_FILE: &str = "triage_shadow.jsonl";
const ACCURACY_FILE: &str = "triage_accuracy.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ComparisonRecord {
    task_query: String,
    rules_result: ProfileHypothesis,
    semantic_result: ProfileHypothesis,
    agreed: bool,
    timestamp: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PerFieldF1 {
    pub intent: f64,
    pub complexity: f64,
    pub scope: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AccuracyReport {
    pub agreement_rate: f64,
    pub per_field_f1: PerFieldF1,
    pub total_tasks_compared: usize,
    pub ready_for_promotion: bool,
}

/// Persists semantic-shadow observations and derives their accuracy report.
#[derive(Debug, Default)]
pub struct AccuracyTracker;

impl AccuracyTracker {
    pub fn record(
        input: &TaskAnalysisInput,
        rules_result: &ProfileHypothesis,
        semantic_result: &ProfileHypothesis,
    ) -> Result<(), String> {
        record_comparison(input, rules_result, semantic_result)
    }

    pub fn report() -> AccuracyReport {
        report()
    }
}

pub fn record_comparison(
    input: &TaskAnalysisInput,
    rules_result: &ProfileHypothesis,
    semantic_result: &ProfileHypothesis,
) -> Result<(), String> {
    let record = ComparisonRecord {
        task_query: input.query.clone(),
        rules_result: rules_result.clone(),
        semantic_result: semantic_result.clone(),
        agreed: rules_result.profile.intent == semantic_result.profile.intent
            && rules_result.profile.complexity == semantic_result.profile.complexity
            && rules_result.profile.scope == semantic_result.profile.scope,
        timestamp: UtcDateTime::now().to_rfc3339(),
    };
    append(SHADOW_FILE, &record)?;
    append(ACCURACY_FILE, &record)
}

pub fn report() -> AccuracyReport {
    let records = read_records();
    let total = records.len();
    if total == 0 {
        return AccuracyReport::default();
    }
    let agreement_rate =
        records.iter().filter(|record| record.agreed).count() as f64 / total as f64;
    let report = AccuracyReport {
        agreement_rate,
        per_field_f1: PerFieldF1 {
            intent: macro_f1(&records, |record| {
                (
                    record.rules_result.profile.intent.clone(),
                    record.semantic_result.profile.intent.clone(),
                )
            }),
            complexity: macro_f1(&records, |record| {
                (
                    record.rules_result.profile.complexity.clone(),
                    record.semantic_result.profile.complexity.clone(),
                )
            }),
            scope: macro_f1(&records, |record| {
                (
                    format!("{:?}", record.rules_result.profile.scope),
                    format!("{:?}", record.semantic_result.profile.scope),
                )
            }),
        },
        total_tasks_compared: total,
        ready_for_promotion: agreement_rate == 1.0,
    };
    if report.ready_for_promotion {
        tracing::info!(
            total,
            "semantic triage is ready for promotion: 100% agreement"
        );
    }
    report
}

fn append<T: Serialize>(filename: &str, record: &T) -> Result<(), String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?;
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("create triage data directory: {error}"))?;
    let mut line = serde_json::to_string(record)
        .map_err(|error| format!("serialize triage record: {error}"))?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(filename))
        .map_err(|error| format!("open triage record: {error}"))?;
    file.write_all(line.as_bytes())
        .map_err(|error| format!("write triage record: {error}"))
}

fn read_records() -> Vec<ComparisonRecord> {
    let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return Vec::new();
    };
    let Ok(contents) = std::fs::read_to_string(dir.join(ACCURACY_FILE)) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn macro_f1(
    records: &[ComparisonRecord],
    labels: impl Fn(&ComparisonRecord) -> (String, String),
) -> f64 {
    let mut values = std::collections::BTreeSet::new();
    for record in records {
        let (expected, actual) = labels(record);
        values.insert(expected);
        values.insert(actual);
    }
    values
        .iter()
        .map(|label| {
            let (mut true_positive, mut false_positive, mut false_negative) = (0_u64, 0_u64, 0_u64);
            for record in records {
                let (expected, actual) = labels(record);
                match (expected == *label, actual == *label) {
                    (true, true) => true_positive += 1,
                    (false, true) => false_positive += 1,
                    (true, false) => false_negative += 1,
                    (false, false) => {}
                }
            }
            let denominator = 2 * true_positive + false_positive + false_negative;
            if denominator == 0 {
                0.0
            } else {
                2.0 * true_positive as f64 / denominator as f64
            }
        })
        .sum::<f64>()
        / values.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::triage::{
        TriageBackendLocal,
        profile::{TaskProfileLocal, TaskScopeLocal},
    };

    fn hypothesis(intent: &str, complexity: &str, scope: TaskScopeLocal) -> ProfileHypothesis {
        ProfileHypothesis {
            profile: TaskProfileLocal {
                intent: intent.into(),
                complexity: complexity.into(),
                scope,
                ..Default::default()
            },
            confidence_milli: 500,
            backend: TriageBackendLocal::Rules,
        }
    }

    #[test]
    fn accuracy_tracker_computes_correct_agreement_rate() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let input = TaskAnalysisInput {
            query: "task".into(),
            ..Default::default()
        };
        record_comparison(
            &input,
            &hypothesis("fix", "low", TaskScopeLocal::SingleFile),
            &hypothesis("fix", "low", TaskScopeLocal::SingleFile),
        )
        .unwrap();
        record_comparison(
            &input,
            &hypothesis("fix", "low", TaskScopeLocal::SingleFile),
            &hypothesis("review", "high", TaskScopeLocal::MultiFile),
        )
        .unwrap();
        let report = report();
        assert_eq!(report.total_tasks_compared, 2);
        assert_eq!(report.agreement_rate, 0.5);
    }

    #[test]
    fn full_agreement_is_ready_for_promotion() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let input = TaskAnalysisInput {
            query: "task".into(),
            ..Default::default()
        };
        let result = hypothesis("fix", "low", TaskScopeLocal::SingleFile);
        record_comparison(&input, &result, &result).unwrap();
        assert!(report().ready_for_promotion);
    }
}
