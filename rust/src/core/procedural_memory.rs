//! Procedural Memory — recurring workflow detection and template storage.
//!
//! Detects repeated tool-call sequences in Episodic Memory and stores them
//! as reusable Procedures with activation/termination conditions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use super::episodic_memory::{Episode, Outcome};

use crate::core::context_admission::stores::StoreAdmission;
use crate::core::memory_policy::ProceduralPolicy;
use crate::core::policy::runtime::PublicationAuthority;

fn procedural_lock(project_hash: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.entry(project_hash.to_string()).or_default().clone()
}

fn acquire_procedural_file_lock(path: &Path) -> Result<std::fs::File, String> {
    use fs2::FileExt;

    let parent = path
        .parent()
        .ok_or_else(|| "procedural store path has no parent".to_string())?;
    let name = path
        .file_name()
        .ok_or_else(|| "procedural store path has no filename".to_string())?
        .to_string_lossy();
    let lock_path = parent.join(format!(".{name}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|error| format!("procedural store lock unavailable: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600));
    }
    file.try_lock_exclusive()
        .map_err(|error| format!("procedural store lock unavailable: {error}"))?;
    Ok(file)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProceduralStore {
    pub project_hash: String,
    pub procedures: Vec<Procedure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Procedure {
    pub id: String,
    pub name: String,
    pub description: String,
    pub steps: Vec<ProcedureStep>,
    pub activation_keywords: Vec<String>,
    pub confidence: f32,
    pub times_used: u32,
    pub times_succeeded: u32,
    pub last_used: DateTime<Utc>,
    pub project_specific: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ProcedureStep {
    pub tool: String,
    pub description: String,
    pub optional: bool,
}

/// Retention value of a procedure (higher = keep): confidence-weighted, with a
/// success-rate and usage component. The single ranking used by both the
/// per-write reclaim ([`ProceduralStore::add_procedure`]) and the consolidation
/// capacity pass, so procedure eviction is consistent everywhere (#995).
pub fn retention_score(p: &Procedure) -> f32 {
    let use_score = (p.times_used.min(20) as f32) / 20.0;
    p.confidence * 0.5 + p.success_rate() * 0.3 + use_score * 0.2
}

/// Order procedures best-kept first (descending retention), deterministically.
/// Pass to `memory_capacity::reclaim_store`, which archives the
/// lowest-ranked tail.
pub fn retention_cmp(a: &Procedure, b: &Procedure) -> std::cmp::Ordering {
    retention_score(b)
        .total_cmp(&retention_score(a))
        .then_with(|| b.last_used.cmp(&a.last_used))
        .then_with(|| b.created_at.cmp(&a.created_at))
        .then_with(|| a.name.cmp(&b.name))
        .then_with(|| a.id.cmp(&b.id))
}

impl Procedure {
    pub fn success_rate(&self) -> f32 {
        if self.times_used == 0 {
            return 0.0;
        }
        self.times_succeeded as f32 / self.times_used as f32
    }

    pub fn matches_context(&self, task: &str) -> bool {
        let task_lower = task.to_lowercase();
        self.activation_keywords
            .iter()
            .any(|kw| task_lower.contains(&kw.to_lowercase()))
    }
}

impl ProceduralStore {
    pub fn new(project_hash: &str) -> Self {
        Self {
            project_hash: project_hash.to_string(),
            procedures: Vec::new(),
        }
    }

    pub fn suggest(&self, task: &str) -> Vec<&Procedure> {
        let mut matches: Vec<(&Procedure, f32)> = self
            .procedures
            .iter()
            .filter(|p| p.matches_context(task) && p.confidence >= 0.3)
            .map(|p| {
                let score = p.confidence * 0.5 + p.success_rate() * 0.3 + usage_recency(p) * 0.2;
                (p, score)
            })
            .collect();

        matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        matches.into_iter().map(|(p, _)| p).collect()
    }

    pub fn record_usage(&mut self, procedure_id: &str, success: bool) {
        if let Some(proc) = self.procedures.iter_mut().find(|p| p.id == procedure_id) {
            proc.times_used += 1;
            if success {
                proc.times_succeeded += 1;
            }
            proc.last_used = Utc::now();
            proc.confidence =
                (proc.confidence * 0.8 + if success { 0.2 } else { -0.1 }).clamp(0.0, 1.0);
        }
    }

    pub fn add_procedure(&mut self, procedure: Procedure, policy: &ProceduralPolicy) {
        if let Some(existing) = self
            .procedures
            .iter_mut()
            .find(|p| p.name == procedure.name)
        {
            existing.confidence = existing.confidence.midpoint(procedure.confidence);
            existing.steps = procedure.steps;
            existing.activation_keywords = procedure.activation_keywords;
        } else {
            self.procedures.push(procedure);
        }

        // Lossless capacity reclaim (#995): keep the highest-value procedures and
        // archive the rest instead of truncating them away. `add_procedure` only
        // sees `ProceduralPolicy`, so it uses the standard headroom default and is
        // always lossless (procedure eviction was already unconditional — it just
        // used to lose the dropped procedures). Same `retention_cmp` as the
        // consolidation capacity pass, so ranking is identical everywhere.
        if let Err(error) = crate::core::memory_capacity::reclaim_store(
            crate::core::memory_archive::MemoryStore::Procedures,
            Some(&self.project_hash),
            &mut self.procedures,
            policy.max_procedures,
            crate::core::memory_lifecycle::DEFAULT_RECLAIM_HEADROOM_PCT,
            true,
            retention_cmp,
        ) {
            tracing::warn!(%error, "procedure capacity reclaim failed");
        }
    }

    pub(crate) fn add_procedure_admitted(
        &mut self,
        mut procedure: Procedure,
        policy: &ProceduralPolicy,
        admission: &StoreAdmission,
        authority: &PublicationAuthority,
    ) -> Result<(), String> {
        let mut admit =
            |text: &str| crate::core::session::admission::admit_store_text(admission, text);
        if !sanitize_procedure(&mut procedure, &mut admit) {
            return Err("procedure content withheld by current policy".into());
        }
        if let Some(existing) = self
            .procedures
            .iter_mut()
            .find(|existing| existing.name == procedure.name)
        {
            existing.confidence = existing.confidence.midpoint(procedure.confidence);
            existing.steps = procedure.steps;
            existing.activation_keywords = procedure.activation_keywords;
        } else {
            self.procedures.push(procedure);
        }

        crate::core::memory_capacity::reclaim_store_with_authority(
            crate::core::memory_archive::MemoryStore::Procedures,
            Some(&self.project_hash),
            &mut self.procedures,
            policy.max_procedures,
            crate::core::memory_lifecycle::DEFAULT_RECLAIM_HEADROOM_PCT,
            true,
            (admission, authority),
            retention_cmp,
        )?;
        Ok(())
    }

    pub fn detect_patterns(&mut self, episodes: &[Episode], policy: &ProceduralPolicy) {
        let sequences = extract_tool_sequences(episodes);
        let patterns = find_repeated_sequences(&sequences, policy);

        for (steps, count, keywords) in patterns {
            if count < policy.min_repetitions || steps.len() < policy.min_sequence_len {
                continue;
            }

            let name = generate_procedure_name(&steps);
            let already_exists = self.procedures.iter().any(|p| p.name == name);
            if already_exists {
                continue;
            }

            let success_count = episodes
                .iter()
                .filter(|ep| matches!(ep.outcome, Outcome::Success { .. }))
                .count();
            let confidence = success_count as f32 / episodes.len().max(1) as f32;

            self.add_procedure(
                Procedure {
                    id: format!("proc-{}", md5_short(&name)),
                    name,
                    description: format!("Detected workflow ({count} repetitions)"),
                    steps,
                    activation_keywords: keywords,
                    confidence,
                    times_used: count as u32,
                    times_succeeded: success_count as u32,
                    last_used: Utc::now(),
                    project_specific: true,
                    created_at: Utc::now(),
                },
                policy,
            );
        }
    }

    /// Derive procedures from admitted episodes, sanitize each candidate, then
    /// route capacity eviction through the authority-checked archive sink.
    pub(crate) fn detect_patterns_admitted(
        &mut self,
        episodes: &[Episode],
        policy: &ProceduralPolicy,
        admission: &StoreAdmission,
        authority: &PublicationAuthority,
    ) -> Result<(), String> {
        let sequences = extract_tool_sequences(episodes);
        let patterns = find_repeated_sequences(&sequences, policy);
        let success_count = episodes
            .iter()
            .filter(|ep| matches!(ep.outcome, Outcome::Success { .. }))
            .count();
        let confidence = success_count as f32 / episodes.len().max(1) as f32;
        let mut candidate_names = std::collections::HashSet::new();
        let mut candidates = Vec::new();

        for (steps, count, keywords) in patterns {
            if count < policy.min_repetitions || steps.len() < policy.min_sequence_len {
                continue;
            }

            let name = generate_procedure_name(&steps);
            if self
                .procedures
                .iter()
                .any(|procedure| procedure.name == name)
                || !candidate_names.insert(name.clone())
            {
                continue;
            }
            let mut procedure = Procedure {
                id: format!("proc-{}", md5_short(&name)),
                name,
                description: format!("Detected workflow ({count} repetitions)"),
                steps,
                activation_keywords: keywords,
                confidence,
                times_used: count as u32,
                times_succeeded: success_count as u32,
                last_used: Utc::now(),
                project_specific: true,
                created_at: Utc::now(),
            };
            let mut admit =
                |text: &str| crate::core::session::admission::admit_store_text(admission, text);
            if sanitize_procedure(&mut procedure, &mut admit) {
                candidates.push(procedure);
            }
        }

        for procedure in candidates {
            self.add_procedure_admitted(procedure, policy, admission, authority)?;
        }
        Ok(())
    }

    fn store_path(project_hash: &str) -> Option<PathBuf> {
        let dir = crate::core::data_dir::lean_ctx_data_dir()
            .ok()?
            .join("memory")
            .join("procedures");
        Some(dir.join(format!("{project_hash}.json")))
    }

    pub fn load(project_hash: &str) -> Option<Self> {
        let path = Self::store_path(project_hash)?;
        let data = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    /// Load procedures only after every content-bearing field is admitted for
    /// the trusted project's storage policy.
    pub(crate) fn load_admitted(
        project_hash: &str,
        admission: &StoreAdmission,
    ) -> Result<Self, String> {
        let mut store = match Self::store_path(project_hash) {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(data) => {
                    serde_json::from_str::<Self>(&data).map_err(|error| error.to_string())?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Self::new(project_hash)
                }
                Err(error) => return Err(error.to_string()),
            },
            None => return Err("Cannot determine data directory".into()),
        };
        if store.project_hash != project_hash {
            return Err("procedural store project scope mismatch".into());
        }
        let mut admit =
            |text: &str| crate::core::session::admission::admit_store_text(admission, text);
        store
            .procedures
            .retain_mut(|procedure| sanitize_procedure(procedure, &mut admit));
        if !admission.is_current() {
            return Err("session admission policy changed; retry".into());
        }
        Ok(store)
    }

    pub fn load_or_create(project_hash: &str) -> Self {
        Self::load(project_hash).unwrap_or_else(|| Self::new(project_hash))
    }

    /// Reload, sanitize, derive and atomically persist under one per-project
    /// lock so concurrent session completions cannot overwrite each other.
    pub(crate) fn mutate_locked_admitted<T>(
        project_hash: &str,
        admission: &StoreAdmission,
        mutate: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<(Self, T), String> {
        Self::mutate_locked_admitted_with_authority(project_hash, admission, |store, _| {
            mutate(store)
        })
    }

    /// Admitted mutation variant for derived writes that may archive evictions.
    /// The scoped authority is supplied to the closure so each archive sink can
    /// revalidate immediately before publishing its recovery artifact.
    pub(crate) fn mutate_locked_admitted_with_authority<T>(
        project_hash: &str,
        admission: &StoreAdmission,
        mutate: impl FnOnce(&mut Self, &PublicationAuthority) -> Result<T, String>,
    ) -> Result<(Self, T), String> {
        let authority = PublicationAuthority::capture()
            .map_err(|_| "source authority unavailable".to_string())?;
        let lock = procedural_lock(project_hash);
        let _guard = lock
            .try_lock()
            .map_err(|_| "procedural store is busy; retry".to_string())?;
        let path = Self::store_path(project_hash)
            .ok_or_else(|| "Cannot determine data directory".to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        let _file_lock = acquire_procedural_file_lock(&path)?;
        let mut store = match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str::<Self>(&data).map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::new(project_hash),
            Err(error) => return Err(error.to_string()),
        };
        if store.project_hash != project_hash {
            return Err("procedural store project scope mismatch".into());
        }
        let mut admit =
            |text: &str| crate::core::session::admission::admit_store_text(admission, text);
        store
            .procedures
            .retain_mut(|procedure| sanitize_procedure(procedure, &mut admit));
        let result = mutate(&mut store, &authority)?;
        store
            .procedures
            .retain_mut(|procedure| sanitize_procedure(procedure, &mut admit));
        let json = serde_json::to_string_pretty(&store).map_err(|error| error.to_string())?;
        admission.authorize_publication(&authority)?;
        crate::config_io::write_atomic(&path, &json)?;
        Ok((store, result))
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::store_path(&self.project_hash)
            .ok_or_else(|| "Cannot determine data directory".to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{e}"))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("{e}"))?;
        std::fs::write(path, json).map_err(|e| format!("{e}"))
    }
}

/// Compatibility entry point for callers already running inside a trusted
/// project source view. Calls without that authority fail closed. New callers
/// should pass the project root explicitly to avoid relying on ambient state.
pub fn auto_detect_from_episodes(project_hash: &str, policy: &ProceduralPolicy) -> Option<usize> {
    let _authority = PublicationAuthority::capture().ok()?;
    let project_root = crate::core::policy::runtime::detached_project_root(Path::new("")).ok()?;
    auto_detect_from_episodes_for_project(&project_root, project_hash, policy)
}

/// Explicit-project variant for trusted callers outside the session tool.
pub fn auto_detect_from_episodes_for_project(
    project_root: &Path,
    project_hash: &str,
    policy: &ProceduralPolicy,
) -> Option<usize> {
    let project_root_text = project_root.to_str()?;
    if crate::core::project_hash::hash_project_root(project_root_text) != project_hash {
        return None;
    }

    crate::core::policy::runtime::with_explicit_project_source_view(
        project_root,
        |authority| -> Result<Option<usize>, String> {
            let admission = StoreAdmission::current();
            let episodes =
                super::episodic_memory::EpisodicStore::load_admitted(project_hash, &admission)?;
            if episodes.episodes.is_empty() {
                return Ok(None);
            }
            let (procedures, ()) = ProceduralStore::mutate_locked_admitted_with_authority(
                project_hash,
                &admission,
                |store, store_authority| {
                    store.detect_patterns_admitted(
                        &episodes.episodes,
                        policy,
                        &admission,
                        store_authority,
                    )
                },
            )?;
            admission.authorize_publication(authority)?;
            Ok(Some(procedures.procedures.len()))
        },
    )
    .ok()?
    .ok()?
}

fn sanitize_procedure(
    procedure: &mut Procedure,
    admit: &mut impl FnMut(&str) -> Option<String>,
) -> bool {
    fn field(value: &mut String, admit: &mut impl FnMut(&str) -> Option<String>) -> bool {
        if value.is_empty() {
            return true;
        }
        let Some(projected) = admit(value) else {
            return false;
        };
        *value = projected;
        true
    }

    field(&mut procedure.name, admit)
        && field(&mut procedure.description, admit)
        && procedure
            .activation_keywords
            .iter_mut()
            .all(|keyword| field(keyword, admit))
        && procedure
            .steps
            .iter_mut()
            .all(|step| field(&mut step.tool, admit) && field(&mut step.description, admit))
}

fn extract_tool_sequences(episodes: &[Episode]) -> Vec<Vec<String>> {
    episodes
        .iter()
        .map(|ep| ep.actions.iter().map(|a| a.tool.clone()).collect())
        .collect()
}

fn find_repeated_sequences(
    sequences: &[Vec<String>],
    policy: &ProceduralPolicy,
) -> Vec<(Vec<ProcedureStep>, usize, Vec<String>)> {
    let mut ngram_counts: HashMap<Vec<String>, usize> = HashMap::new();

    for seq in sequences {
        if seq.len() < policy.min_sequence_len {
            continue;
        }
        let max_win = seq.len().min(policy.max_window_size);
        for window_size in policy.min_sequence_len..=max_win {
            for window in seq.windows(window_size) {
                let key: Vec<String> = window.to_vec();
                *ngram_counts.entry(key).or_insert(0) += 1;
            }
        }
    }

    let mut results: Vec<(Vec<ProcedureStep>, usize, Vec<String>)> = Vec::new();

    let mut sorted: Vec<_> = ngram_counts.into_iter().collect();
    sorted.sort_by(|a, b| {
        let score_a = a.1 * a.0.len();
        let score_b = b.1 * b.0.len();
        score_b.cmp(&score_a)
    });

    let mut seen_prefixes: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (tools, count) in sorted {
        if count < policy.min_repetitions {
            continue;
        }

        let prefix = tools.join("->");
        let is_substring = seen_prefixes.iter().any(|s| s.contains(&prefix));
        if is_substring {
            continue;
        }

        seen_prefixes.insert(prefix);

        let steps: Vec<ProcedureStep> = tools
            .iter()
            .map(|t| ProcedureStep {
                tool: t.clone(),
                description: String::new(),
                optional: false,
            })
            .collect();

        let keywords: Vec<String> = tools
            .iter()
            .filter(|t| !t.starts_with("ctx_"))
            .cloned()
            .collect();

        results.push((steps, count, keywords));
    }

    results
}

fn generate_procedure_name(steps: &[ProcedureStep]) -> String {
    let tools: Vec<&str> = steps.iter().map(|s| s.tool.as_str()).collect();
    let short: Vec<&str> = tools
        .iter()
        .map(|t| t.strip_prefix("ctx_").unwrap_or(t))
        .collect();
    format!("workflow-{}", short.join("-"))
}

fn md5_short(input: &str) -> String {
    use md5::{Digest, Md5};
    let result = Md5::digest(input.as_bytes());
    crate::core::agent_identity::hex_encode(&result)[..8].to_string()
}

fn usage_recency(proc: &Procedure) -> f32 {
    let days_old = Utc::now().signed_duration_since(proc.last_used).num_days() as f32;
    (1.0 - days_old / 30.0).max(0.0)
}

pub fn format_suggestion(proc: &Procedure) -> String {
    let mut output = format!(
        "Suggested workflow: {} (confidence: {:.0}%, used {}x, success rate: {:.0}%)\n",
        proc.name,
        proc.confidence * 100.0,
        proc.times_used,
        proc.success_rate() * 100.0
    );
    for (i, step) in proc.steps.iter().enumerate() {
        let opt = if step.optional { " (optional)" } else { "" };
        output.push_str(&format!("  {}. {}{opt}\n", i + 1, step.tool));
    }
    output
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::core::episodic_memory::{Action, Episode, Outcome};

    fn make_episode_with_tools(tools: &[&str]) -> Episode {
        Episode {
            id: "ep-1".to_string(),
            session_id: "s-1".to_string(),
            timestamp: Utc::now(),
            task_description: "test task".to_string(),
            actions: tools
                .iter()
                .map(|t| Action {
                    tool: t.to_string(),
                    description: String::new(),
                    timestamp: Utc::now(),
                    duration_ms: 100,
                    success: true,
                })
                .collect(),
            outcome: Outcome::Success { tests_passed: true },
            affected_files: vec![],
            summary: String::new(),
            duration_secs: 60,
            tokens_used: 1000,
            agent_id: None,
        }
    }

    #[test]
    fn admitted_procedure_rejects_restricted_candidate_before_archive() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();

        let project = tempfile::tempdir().expect("project root");
        let project_path = project.path().to_str().expect("UTF-8 project path");
        crate::core::policy::runtime::with_project_source_view(project_path, || {
            let authority = PublicationAuthority::capture().expect("project authority");
            let admission = StoreAdmission::current();
            let policy = ProceduralPolicy {
                max_procedures: 1,
                ..Default::default()
            };
            let mut store = ProceduralStore::new("procedure-admission-test");
            let candidate = Procedure {
                id: "restricted".into(),
                name: "TOP SECRET\ncandidate".into(),
                description: "safe description".into(),
                steps: vec![],
                activation_keywords: vec![],
                confidence: 0.5,
                times_used: 1,
                times_succeeded: 1,
                last_used: Utc::now(),
                project_specific: true,
                created_at: Utc::now(),
            };

            assert!(
                store
                    .add_procedure_admitted(candidate, &policy, &admission, &authority)
                    .is_err()
            );
            assert!(store.procedures.is_empty());
            assert!(
                crate::core::memory_archive::list_archives(
                    crate::core::memory_archive::MemoryStore::Procedures,
                    Some("procedure-admission-test"),
                )
                .is_empty()
            );
        })
        .expect("source view");
    }

    #[test]
    fn admission_scrubs_every_procedure_field_or_drops_the_record() {
        let mut procedure = Procedure {
            id: "proc-test".into(),
            name: "NAME PRIVATE".into(),
            description: "DESCRIPTION PRIVATE".into(),
            steps: vec![ProcedureStep {
                tool: "TOOL PRIVATE".into(),
                description: "STEP PRIVATE".into(),
                optional: false,
            }],
            activation_keywords: vec!["KEYWORD PRIVATE".into()],
            confidence: 0.8,
            times_used: 2,
            times_succeeded: 1,
            last_used: Utc::now(),
            project_specific: true,
            created_at: Utc::now(),
        };
        assert!(sanitize_procedure(&mut procedure, &mut |text| {
            Some(text.replace("PRIVATE", "redacted"))
        }));
        assert_eq!(procedure.name, "NAME redacted");
        assert_eq!(procedure.description, "DESCRIPTION redacted");
        assert_eq!(procedure.steps[0].tool, "TOOL redacted");
        assert_eq!(procedure.steps[0].description, "STEP redacted");
        assert_eq!(procedure.activation_keywords[0], "KEYWORD redacted");

        assert!(!sanitize_procedure(&mut procedure, &mut |text| {
            (text != "NAME redacted").then(|| text.to_string())
        }));
    }

    #[test]
    fn detect_patterns_from_episodes() {
        let policy = ProceduralPolicy::default();
        let episodes: Vec<Episode> = (0..5)
            .map(|_| make_episode_with_tools(&["ctx_read", "ctx_shell", "ctx_read"]))
            .collect();

        let mut store = ProceduralStore::new("test");
        store.detect_patterns(&episodes, &policy);

        assert!(
            !store.procedures.is_empty(),
            "Should detect at least one pattern"
        );
    }

    #[test]
    fn suggest_matching_procedure() {
        let policy = ProceduralPolicy::default();
        let mut store = ProceduralStore::new("test");
        store.add_procedure(
            Procedure {
                id: "proc-1".to_string(),
                name: "deploy-workflow".to_string(),
                description: "Deploy".to_string(),
                steps: vec![ProcedureStep {
                    tool: "ctx_shell".to_string(),
                    description: "cargo build".to_string(),
                    optional: false,
                }],
                activation_keywords: vec!["deploy".to_string(), "release".to_string()],
                confidence: 0.8,
                times_used: 5,
                times_succeeded: 4,
                last_used: Utc::now(),
                project_specific: true,
                created_at: Utc::now(),
            },
            &policy,
        );

        let suggestions = store.suggest("deploy the new version");
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].name, "deploy-workflow");

        let none = store.suggest("refactor the database layer");
        assert!(none.is_empty());
    }

    #[test]
    fn record_usage_updates_confidence() {
        let policy = ProceduralPolicy::default();
        let mut store = ProceduralStore::new("test");
        store.add_procedure(
            Procedure {
                id: "proc-1".to_string(),
                name: "test-workflow".to_string(),
                description: "Test".to_string(),
                steps: vec![],
                activation_keywords: vec![],
                confidence: 0.5,
                times_used: 0,
                times_succeeded: 0,
                last_used: Utc::now(),
                project_specific: false,
                created_at: Utc::now(),
            },
            &policy,
        );

        store.record_usage("proc-1", true);
        let proc = &store.procedures[0];
        assert_eq!(proc.times_used, 1);
        assert_eq!(proc.times_succeeded, 1);
        assert!(proc.confidence > 0.5);
    }

    #[test]
    fn success_rate_calculation() {
        let proc = Procedure {
            id: "p".to_string(),
            name: "n".to_string(),
            description: String::new(),
            steps: vec![],
            activation_keywords: vec![],
            confidence: 0.5,
            times_used: 10,
            times_succeeded: 7,
            last_used: Utc::now(),
            project_specific: false,
            created_at: Utc::now(),
        };
        assert!((proc.success_rate() - 0.7).abs() < 0.01);
    }

    #[test]
    fn max_procedures_enforced() {
        // Eviction archives the dropped procedures and keeps them all when the
        // archive write fails (lossless, #995); in the shared test sandbox that
        // write competed with every parallel memory test and lost.
        let _data = crate::core::data_dir::isolated_data_dir();
        let policy = ProceduralPolicy::default();
        let mut store = ProceduralStore::new("test");
        for i in 0..110 {
            store.add_procedure(
                Procedure {
                    id: format!("p-{i}"),
                    name: format!("workflow-{i}"),
                    description: String::new(),
                    steps: vec![],
                    activation_keywords: vec![],
                    confidence: i as f32 / 110.0,
                    times_used: 0,
                    times_succeeded: 0,
                    last_used: Utc::now(),
                    project_specific: false,
                    created_at: Utc::now(),
                },
                &policy,
            );
        }
        assert!(store.procedures.len() <= policy.max_procedures);
    }

    #[test]
    fn auto_detect_learns_from_recorded_episodes() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project root");
        let root = project.path().to_str().expect("UTF-8 project root");
        let hash = crate::core::project_hash::hash_project_root(root);
        let policy = crate::core::memory_policy::MemoryPolicy::default();
        let mut episodes = crate::core::episodic_memory::EpisodicStore::new(&hash);
        for _ in 0..5 {
            episodes.record_episode(
                make_episode_with_tools(&["ctx_read", "ctx_shell", "ctx_read"]),
                &policy.episodic,
            );
        }
        episodes.save().expect("episodic save");

        assert!(
            auto_detect_from_episodes(&hash, &policy.procedural).is_none(),
            "ambient compatibility entry point must fail closed"
        );
        let learned =
            auto_detect_from_episodes_for_project(project.path(), &hash, &policy.procedural);
        assert!(
            learned.is_some_and(|count| count > 0),
            "admitted auto-detect should learn a workflow, got {learned:?}"
        );
        let reloaded = ProceduralStore::load(&hash).expect("procedural store persisted");
        assert!(!reloaded.procedures.is_empty());
    }

    #[test]
    fn format_suggestion_output() {
        let proc = Procedure {
            id: "p".to_string(),
            name: "deploy-workflow".to_string(),
            description: String::new(),
            steps: vec![
                ProcedureStep {
                    tool: "ctx_shell".to_string(),
                    description: "test".to_string(),
                    optional: false,
                },
                ProcedureStep {
                    tool: "ctx_shell".to_string(),
                    description: "build".to_string(),
                    optional: true,
                },
            ],
            activation_keywords: vec![],
            confidence: 0.85,
            times_used: 10,
            times_succeeded: 8,
            last_used: Utc::now(),
            project_specific: false,
            created_at: Utc::now(),
        };
        let output = format_suggestion(&proc);
        assert!(output.contains("deploy-workflow"));
        assert!(output.contains("85%"));
        assert!(output.contains("(optional)"));
    }
}
