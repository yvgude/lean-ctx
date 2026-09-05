use super::{ValueAssessment, cpao};
use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

const MAX_ASSESSMENTS: usize = 100;
const MAX_DISK_ASSESSMENTS: usize = 10_000;
const MAX_LEDGER_BYTES: u64 = 1024 * 1024;
const LEDGER_FILE: &str = "value_assessments.jsonl";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValueAggregate {
    pub total: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub avg_cpao: Option<u64>,
    pub total_cost: u64,
}

#[derive(Debug, Clone)]
pub struct ValueGateStore {
    assessments: Arc<Mutex<VecDeque<ValueAssessment>>>,
}

impl Default for ValueGateStore {
    fn default() -> Self {
        let mut assessments: VecDeque<_> = Self::load_from_disk().into_iter().collect();
        while assessments.len() > MAX_ASSESSMENTS {
            assessments.pop_front();
        }
        Self {
            assessments: Arc::new(Mutex::new(assessments)),
        }
    }
}

impl ValueGateStore {
    pub fn record(&self, assessment: &ValueAssessment) {
        let mut entries = self
            .assessments
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if entries.len() >= MAX_ASSESSMENTS {
            entries.pop_front();
        }
        entries.push_back(assessment.clone());
        drop(entries);
        let _ = Self::append_to_disk(assessment);
    }

    pub fn recent(&self, n: usize) -> Vec<ValueAssessment> {
        self.assessments
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .rev()
            .take(n)
            .cloned()
            .collect()
    }

    pub fn aggregate(&self) -> ValueAggregate {
        let entries = self
            .assessments
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let costs: Vec<u64> = entries.iter().map(|a| a.cost_micros).collect();
        let accepted: Vec<bool> = entries.iter().map(|a| a.outcome_accepted).collect();
        let accepted_count = accepted.iter().filter(|&&ok| ok).count();
        ValueAggregate {
            total: entries.len(),
            accepted: accepted_count,
            rejected: entries.len() - accepted_count,
            avg_cpao: cpao::cost_per_accepted_outcome(&costs, &accepted),
            total_cost: costs.into_iter().fold(0, u64::saturating_add),
        }
    }

    pub fn persist_path() -> PathBuf {
        crate::core::paths::state_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("lean-ctx"))
            .join(LEDGER_FILE)
    }

    pub fn append_to_disk(assessment: &ValueAssessment) -> io::Result<()> {
        Self::append_to_path(&Self::persist_path(), assessment)
    }

    pub fn load_from_disk() -> Vec<ValueAssessment> {
        Self::load_from_path(&Self::persist_path())
    }

    pub(crate) fn append_to_path(path: &Path, assessment: &ValueAssessment) -> io::Result<()> {
        let json = serde_json::to_string(assessment)
            .map_err(|error| io::Error::other(format!("serialize assessment: {error}")))?;
        fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")))?;
        Self::rotate_if_needed(path, json.len() as u64 + 1)?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{json}")
    }

    pub(crate) fn load_from_path(path: &Path) -> Vec<ValueAssessment> {
        let mut assessments = Self::read_tail(&path.with_extension("jsonl.1"));
        assessments.extend(Self::read_tail(path));
        let keep_from = assessments.len().saturating_sub(MAX_DISK_ASSESSMENTS);
        assessments.drain(..keep_from);
        assessments
    }

    fn read_tail(path: &Path) -> Vec<ValueAssessment> {
        let mut assessments: Vec<_> = fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .rev()
            .take(MAX_DISK_ASSESSMENTS)
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        assessments.reverse();
        assessments
    }

    fn rotate_if_needed(path: &Path, incoming_bytes: u64) -> io::Result<()> {
        if fs::metadata(path)
            .is_ok_and(|meta| meta.len().saturating_add(incoming_bytes) > MAX_LEDGER_BYTES)
        {
            let backup = path.with_extension("jsonl.1");
            if backup.exists() {
                fs::remove_file(&backup)?;
            }
            fs::rename(path, backup)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assessment(id: usize) -> ValueAssessment {
        ValueAssessment {
            task_id: format!("task-{id}"),
            model: "test".into(),
            total_tokens: id as u64,
            cost_micros: 10,
            outcome_accepted: true,
            cpao_micros: Some(10),
            evidence: vec!["test".into()],
            timestamp: "2026-08-12T00:00:00Z".into(),
        }
    }

    fn test_path(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("lean-ctx-value-gate-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path.join(LEDGER_FILE)
    }

    #[test]
    fn test_persist_creates_file() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let store = ValueGateStore::default();
        let path = ValueGateStore::persist_path();
        store.record(&assessment(1));
        assert!(path.is_file());
    }

    #[test]
    fn test_persist_append() {
        let path = test_path("append");
        (0..3).for_each(|id| ValueGateStore::append_to_path(&path, &assessment(id)).unwrap());
        assert_eq!(fs::read_to_string(path).unwrap().lines().count(), 3);
    }

    #[test]
    fn test_load_roundtrip() {
        let path = test_path("roundtrip");
        let expected = vec![assessment(1), assessment(2)];
        for item in &expected {
            ValueGateStore::append_to_path(&path, item).unwrap();
        }
        assert_eq!(ValueGateStore::load_from_path(&path), expected);
    }

    #[test]
    fn test_load_corrupt_line() {
        let path = test_path("corrupt");
        ValueGateStore::append_to_path(&path, &assessment(1)).unwrap();
        fs::write(
            &path,
            format!(
                "{{not json}}\n{}",
                serde_json::to_string(&assessment(2)).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(ValueGateStore::load_from_path(&path), vec![assessment(2)]);
    }

    #[test]
    fn test_load_empty_file() {
        let path = test_path("empty");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "").unwrap();
        assert!(ValueGateStore::load_from_path(&path).is_empty());
    }
}
