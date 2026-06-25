use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::core::session::{
    Decision, EvidenceRecord, FileTouched, Finding, ProgressEntry, SessionState, SessionStats,
    TaskInfo, TestSnapshot,
};

const MAX_BUNDLE_BYTES: usize = 250_000;
const MAX_NEXT_STEPS: usize = 25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundlePrivacyV1 {
    Redacted,
    Full,
}

impl BundlePrivacyV1 {
    #[must_use]
    pub fn parse(s: Option<&str>) -> Self {
        match s.unwrap_or("redacted").trim().to_lowercase().as_str() {
            "full" => Self::Full,
            _ => Self::Redacted,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CcpSessionBundleV1 {
    pub schema_version: u32,
    pub exported_at: DateTime<Utc>,
    pub project: ProjectIdentityV1,
    pub role: PolicyIdentityV1,
    pub profile: PolicyIdentityV1,
    pub session: SessionExcerptV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectIdentityV1 {
    pub project_root_hash: Option<String>,
    pub project_identity_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyIdentityV1 {
    pub name: String,
    pub policy_md5: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionExcerptV1 {
    pub id: String,
    pub version: u32,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub project_root: Option<String>,
    pub shell_cwd: Option<String>,
    pub task: Option<TaskInfo>,
    pub findings: Vec<Finding>,
    pub decisions: Vec<Decision>,
    pub files_touched: Vec<FileTouched>,
    pub test_results: Option<TestSnapshot>,
    pub progress: Vec<ProgressEntry>,
    pub next_steps: Vec<String>,
    pub evidence: Vec<EvidenceRecord>,
    pub stats: SessionStats,
    #[serde(default)]
    pub terse_mode: bool,
    #[serde(default)]
    pub compression_level: String,
}

#[must_use]
pub fn build_bundle_v1(session: &SessionState, privacy: BundlePrivacyV1) -> CcpSessionBundleV1 {
    let role_name = crate::core::roles::active_role_name();
    let role = crate::core::roles::active_role();
    let profile_name = crate::core::profiles::active_profile_name();
    let profile = crate::core::profiles::active_profile();

    let role_policy_md5 =
        crate::core::hasher::hash_str(&serde_json::to_string(&role).unwrap_or_default());
    let profile_policy_md5 =
        crate::core::hasher::hash_str(&serde_json::to_string(&profile).unwrap_or_default());

    let (project_root_hash, project_identity_hash) =
        session
            .project_root
            .as_deref()
            .map_or((None, None), |root| {
                let root_hash = crate::core::project_hash::hash_project_root(root);
                let identity = crate::core::project_hash::project_identity(root);
                let identity_hash = identity.as_deref().map(crate::core::hasher::hash_str);
                (Some(root_hash), identity_hash)
            });

    let mut excerpt = SessionExcerptV1 {
        id: session.id.clone(),
        version: session.version,
        started_at: session.started_at,
        updated_at: session.updated_at,
        project_root: session.project_root.clone(),
        shell_cwd: session.shell_cwd.clone(),
        task: session.task.clone(),
        findings: session.findings.clone(),
        decisions: session.decisions.clone(),
        files_touched: session.files_touched.clone(),
        test_results: session.test_results.clone(),
        progress: session.progress.clone(),
        next_steps: session
            .next_steps
            .iter()
            .take(MAX_NEXT_STEPS)
            .cloned()
            .collect(),
        evidence: session.evidence.clone(),
        stats: session.stats.clone(),
        terse_mode: session.terse_mode,
        compression_level: session.compression_level.clone(),
    };

    // Path minimization: prefer relative paths when project_root is known.
    let root = excerpt.project_root.clone().unwrap_or_default();
    if !root.is_empty() {
        for f in &mut excerpt.files_touched {
            if let Some(rel) = strip_root_prefix(&root, &f.path) {
                f.path = rel;
            }
        }
        for finding in &mut excerpt.findings {
            if let Some(ref file) = finding.file.clone()
                && let Some(rel) = strip_root_prefix(&root, file)
            {
                finding.file = Some(rel);
            }
        }
    }

    match privacy {
        BundlePrivacyV1::Full => {
            // Full export is allowed only for admin role; otherwise force redaction.
            if role_name != "admin" {
                redact_excerpt_in_place(&mut excerpt);
            } else if crate::core::redaction::redaction_enabled_for_active_role() {
                // Admin opted into redaction — keep it consistent.
                redact_excerpt_in_place(&mut excerpt);
            }
        }
        BundlePrivacyV1::Redacted => {
            redact_excerpt_in_place(&mut excerpt);
        }
    }

    CcpSessionBundleV1 {
        schema_version: crate::core::contracts::CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION,
        exported_at: Utc::now(),
        project: ProjectIdentityV1 {
            project_root_hash,
            project_identity_hash,
        },
        role: PolicyIdentityV1 {
            name: role_name,
            policy_md5: role_policy_md5,
        },
        profile: PolicyIdentityV1 {
            name: profile_name,
            policy_md5: profile_policy_md5,
        },
        session: excerpt,
    }
}

pub fn serialize_bundle_v1_pretty(bundle: &CcpSessionBundleV1) -> Result<String, String> {
    let json = serde_json::to_string_pretty(bundle).map_err(|e| e.to_string())?;
    if json.len() > MAX_BUNDLE_BYTES {
        return Err(format!(
            "ERROR: bundle too large ({} bytes > max {}). Use privacy=redacted and/or reduce session evidence.",
            json.len(),
            MAX_BUNDLE_BYTES
        ));
    }
    Ok(json)
}

pub fn parse_bundle_v1(json: &str) -> Result<CcpSessionBundleV1, String> {
    let b: CcpSessionBundleV1 = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if b.schema_version != crate::core::contracts::CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION {
        return Err(format!(
            "ERROR: unsupported schema_version {} (expected {})",
            b.schema_version,
            crate::core::contracts::CCP_SESSION_BUNDLE_V1_SCHEMA_VERSION
        ));
    }
    Ok(b)
}

pub fn write_bundle_v1(path: &Path, json: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "ERROR: invalid path".to_string())?;
    if !parent.exists() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("bundle")
    ));
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn read_bundle_v1(path: &Path) -> Result<CcpSessionBundleV1, String> {
    let json = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if json.len() > MAX_BUNDLE_BYTES {
        return Err(format!(
            "ERROR: bundle file too large ({} bytes > max {})",
            json.len(),
            MAX_BUNDLE_BYTES
        ));
    }
    parse_bundle_v1(&json)
}

pub fn import_bundle_v1_into_session(
    session: &mut SessionState,
    bundle: &CcpSessionBundleV1,
    current_project_root: Option<&str>,
) -> ImportReportV1 {
    let mut imported = bundle.session.clone();

    // Prefer current project root when provided (replay safety).
    if let Some(root) = current_project_root {
        imported.project_root = Some(root.to_string());
    }

    // Mark stale file paths if missing or outside jail.
    let jail_root = imported.project_root.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string())
    });
    let jail_root_path = PathBuf::from(&jail_root);

    let mut stale = 0u32;
    for f in &mut imported.files_touched {
        let candidate = candidate_path(&jail_root_path, &f.path);
        if let Ok((jailed, _warning)) = crate::core::io_boundary::jail_and_check_path(
            "ctx_session.import",
            candidate.as_path(),
            jail_root_path.as_path(),
        ) {
            if jailed.exists() {
                f.stale = false;
            } else {
                f.stale = true;
                stale += 1;
            }
        } else {
            f.stale = true;
            stale += 1;
        }
    }

    *session = SessionState {
        id: imported.id.clone(),
        version: imported.version,
        started_at: imported.started_at,
        updated_at: imported.updated_at,
        project_root: imported.project_root.clone(),
        shell_cwd: imported.shell_cwd.clone(),
        task: imported.task.clone(),
        findings: imported.findings.clone(),
        decisions: imported.decisions.clone(),
        files_touched: imported.files_touched.clone(),
        test_results: imported.test_results.clone(),
        progress: imported.progress.clone(),
        next_steps: imported.next_steps.clone(),
        evidence: imported.evidence.clone(),
        intents: Vec::new(),
        active_structured_intent: None,
        stats: imported.stats.clone(),
        terse_mode: imported.terse_mode,
        compression_level: imported.compression_level.clone(),
        last_consolidate_ts: None,
        extra_roots: Vec::new(),
        wakeup_manifest: Vec::new(),
        playbook: crate::core::session::Playbook::default(),
        last_semantic_query: None,
    };

    ImportReportV1 {
        session_id: session.id.clone(),
        version: session.version,
        files_touched: session.files_touched.len() as u32,
        stale_files: stale,
    }
}

#[derive(Debug, Clone)]
pub struct ImportReportV1 {
    pub session_id: String,
    pub version: u32,
    pub files_touched: u32,
    pub stale_files: u32,
}

fn redact_excerpt_in_place(ex: &mut SessionExcerptV1) {
    ex.shell_cwd = None;
    // project_root is represented as hashes at bundle level; avoid exporting raw paths.
    ex.project_root = None;

    if let Some(ref mut t) = ex.task {
        t.description = crate::core::redaction::redact_text(&t.description);
        if let Some(ref mut intent) = t.intent {
            *intent = crate::core::redaction::redact_text(intent);
        }
    }
    for f in &mut ex.findings {
        f.summary = crate::core::redaction::redact_text(&f.summary);
        if let Some(ref mut file) = f.file {
            *file = crate::core::redaction::redact_text(file);
        }
    }
    for d in &mut ex.decisions {
        d.summary = crate::core::redaction::redact_text(&d.summary);
        if let Some(ref mut r) = d.rationale {
            *r = crate::core::redaction::redact_text(r);
        }
    }
    for p in &mut ex.progress {
        p.action = crate::core::redaction::redact_text(&p.action);
        if let Some(ref mut detail) = p.detail {
            *detail = crate::core::redaction::redact_text(detail);
        }
    }
    for s in &mut ex.next_steps {
        *s = crate::core::redaction::redact_text(s);
    }
    for ev in &mut ex.evidence {
        ev.value = None;
    }
}

fn strip_root_prefix(root: &str, path: &str) -> Option<String> {
    let root = root.trim_end_matches(std::path::MAIN_SEPARATOR);
    let root_prefix = format!("{root}{}", std::path::MAIN_SEPARATOR);
    if path.starts_with(&root_prefix) {
        Some(path.trim_start_matches(&root_prefix).to_string())
    } else {
        None
    }
}

fn candidate_path(jail_root: &Path, stored_path: &str) -> PathBuf {
    let p = PathBuf::from(stored_path);
    if p.is_absolute() {
        p
    } else {
        jail_root.join(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_export_drops_evidence_values() {
        let mut s = SessionState::new();
        s.record_manual_evidence("k", Some("secret=abcdef0123456789abcdef0123456789"));
        let b = build_bundle_v1(&s, BundlePrivacyV1::Redacted);
        assert!(b.session.evidence.iter().all(|e| e.value.is_none()));
    }

    #[test]
    fn serialize_respects_size_cap() {
        let s = SessionState::new();
        let b = build_bundle_v1(&s, BundlePrivacyV1::Redacted);
        let json = serialize_bundle_v1_pretty(&b).expect("json");
        assert!(json.len() < MAX_BUNDLE_BYTES);
        let parsed = parse_bundle_v1(&json).expect("parse");
        assert_eq!(parsed.schema_version, b.schema_version);
    }

    #[test]
    fn import_marks_missing_files_stale() {
        let mut s = SessionState::new();
        s.project_root = Some(
            std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .to_string(),
        );
        s.touch_file("does-not-exist-xyz.txt", None, "full", 10);
        let b = build_bundle_v1(&s, BundlePrivacyV1::Redacted);

        let root = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let mut target = SessionState::new();
        let report = import_bundle_v1_into_session(&mut target, &b, Some(&root));
        assert_eq!(report.files_touched, 1);
        assert_eq!(report.stale_files, 1);
        assert!(target.files_touched[0].stale);
    }
}
