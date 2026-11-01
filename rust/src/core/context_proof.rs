use chrono::TimeZone;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static PROOFS_COLLECTED: AtomicU64 = AtomicU64::new(0);
static PROOFS_WRITTEN: AtomicU64 = AtomicU64::new(0);
static LAST_WRITTEN_UNIX_MS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProofStatsSnapshot {
    pub collected: u64,
    pub written: u64,
    pub last_written_at: Option<String>,
}

pub(crate) fn proof_stats_snapshot() -> ProofStatsSnapshot {
    let collected = PROOFS_COLLECTED.load(Ordering::Relaxed);
    let written = PROOFS_WRITTEN.load(Ordering::Relaxed);
    let last_ms = LAST_WRITTEN_UNIX_MS.load(Ordering::Relaxed);
    let last_written_at = if last_ms == 0 {
        None
    } else {
        chrono::Utc
            .timestamp_millis_opt(last_ms as i64)
            .single()
            .map(|t| t.to_rfc3339())
    };
    ProofStatsSnapshot {
        collected,
        written,
        last_written_at,
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ContextProofV1 {
    pub schema_version: u32,
    pub created_at: String,
    pub lean_ctx_version: String,
    pub session_id: Option<String>,
    pub project: ProjectIdentity,
    pub role: RoleIdentity,
    pub profile: ProfileIdentity,
    /// Kept for the proof's wire shape. Per-layer metrics are not collected,
    /// so this is `runs: 0` unless a legacy `pipeline_stats.json` exists.
    pub pipeline: crate::core::pipeline::PipelineStats,
    pub verification: crate::core::output_verification::VerificationSnapshot,
    pub ledger: LedgerSummary,
    pub evidence: Vec<EvidenceReceipt>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectIdentity {
    pub project_root_hash: Option<String>,
    pub project_identity_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RoleIdentity {
    pub name: String,
    /// BLAKE3 hex; the serialized name keeps its historical `_md5` suffix.
    #[serde(rename = "policy_md5")]
    pub policy_blake3: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProfileIdentity {
    pub name: String,
    /// BLAKE3 hex; the serialized name keeps its historical `_md5` suffix.
    #[serde(rename = "policy_md5")]
    pub policy_blake3: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LedgerSummary {
    pub window_size: usize,
    pub entries: usize,
    pub total_tokens_sent: usize,
    pub total_tokens_saved: usize,
    pub compression_ratio: f64,
    pub top_files_by_sent_tokens: Vec<LedgerFileSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LedgerFileSummary {
    pub path: String,
    pub mode: String,
    pub sent_tokens: usize,
    pub original_tokens: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EvidenceReceipt {
    pub tool: Option<String>,
    pub input_md5: Option<String>,
    pub output_md5: Option<String>,
    pub agent_id: Option<String>,
    pub client_name: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ProofOptions {
    pub max_evidence: usize,
    pub max_ledger_files: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ProofSources {
    pub project_root: Option<String>,
    pub session: Option<crate::core::session::SessionState>,
    pub pipeline: Option<crate::core::pipeline::PipelineStats>,
    pub ledger: Option<crate::core::context_ledger::ContextLedger>,
}

pub(crate) fn collect_v1(sources: ProofSources, opts: ProofOptions) -> ContextProofV1 {
    PROOFS_COLLECTED.fetch_add(1, Ordering::Relaxed);
    let created_at = chrono::Utc::now().to_rfc3339();

    let role_name = crate::core::roles::active_role_name();
    let role = crate::core::roles::active_role();

    let profile_name = crate::core::profiles::active_profile_name();
    let profile = crate::core::profiles::active_profile();

    let role_policy_blake3 =
        crate::core::hasher::hash_str(&serde_json::to_string(&role).unwrap_or_default());
    let profile_policy_blake3 =
        crate::core::hasher::hash_str(&serde_json::to_string(&profile).unwrap_or_default());

    let project_root = sources.project_root.clone().or_else(|| {
        sources
            .session
            .as_ref()
            .and_then(|s| s.project_root.clone())
    });

    let (project_root_hash, project_identity_hash) = if let Some(ref root) = project_root {
        let root_hash = crate::core::project_hash::hash_project_root(root);
        let identity = crate::core::project_hash::project_identity(root);
        let identity_hash = identity.as_deref().map(crate::core::hasher::hash_str);
        (Some(root_hash), identity_hash)
    } else {
        (None, None)
    };

    let verification = crate::core::output_verification::stats_snapshot();

    let pipeline = sources
        .pipeline
        .unwrap_or_else(crate::core::pipeline::PipelineStats::load);

    let ledger = sources
        .ledger
        .unwrap_or_else(crate::core::context_ledger::ContextLedger::load);

    let top_files = ledger
        .files_by_token_cost()
        .into_iter()
        .take(opts.max_ledger_files.max(1))
        .filter_map(|(path, _)| ledger.entries.iter().find(|e| e.path == path).cloned())
        .map(|e| LedgerFileSummary {
            path: e.path,
            mode: e.mode,
            sent_tokens: e.sent_tokens,
            original_tokens: e.original_tokens,
        })
        .collect::<Vec<_>>();

    let ledger_summary = LedgerSummary {
        window_size: ledger.window_size,
        entries: ledger.entries.len(),
        total_tokens_sent: ledger.total_tokens_sent,
        total_tokens_saved: ledger.total_tokens_saved,
        compression_ratio: ledger.compression_ratio(),
        top_files_by_sent_tokens: top_files,
    };

    let session = sources.session.or_else(|| {
        project_root
            .as_deref()
            .and_then(crate::core::session::SessionState::load_latest_for_project_root)
            .or_else(crate::core::session::SessionState::load_latest)
    });

    let (session_id, evidence) = if let Some(s) = session {
        let mut receipts = s
            .evidence
            .into_iter()
            .filter(|e| matches!(e.kind, crate::core::session::EvidenceKind::ToolCall))
            .rev()
            .take(opts.max_evidence.max(1))
            .map(|e| EvidenceReceipt {
                tool: e.tool,
                input_md5: e.input_md5,
                output_md5: e.output_md5,
                agent_id: e.agent_id,
                client_name: e.client_name,
                timestamp: e.timestamp.to_rfc3339(),
            })
            .collect::<Vec<_>>();
        receipts.reverse();
        (Some(s.id), receipts)
    } else {
        (None, Vec::new())
    };

    ContextProofV1 {
        schema_version: crate::core::contracts::CONTEXT_PROOF_V1_SCHEMA_VERSION,
        created_at,
        lean_ctx_version: env!("CARGO_PKG_VERSION").to_string(),
        session_id,
        project: ProjectIdentity {
            project_root_hash,
            project_identity_hash,
        },
        role: RoleIdentity {
            name: role_name,
            policy_blake3: role_policy_blake3,
        },
        profile: ProfileIdentity {
            name: profile_name,
            policy_blake3: profile_policy_blake3,
        },
        pipeline,
        verification,
        ledger: ledger_summary,
        evidence,
    }
}

pub(crate) fn write_project_proof(
    project_root: &Path,
    proof: &ContextProofV1,
    filename: Option<&str>,
) -> Result<PathBuf, String> {
    let proofs_dir = crate::core::pathutil::safe_project_data_dir(project_root)?.join("proofs");
    std::fs::create_dir_all(&proofs_dir).map_err(|e| e.to_string())?;

    let ts = chrono::Utc::now().format("%Y-%m-%d_%H%M%S");
    let name = match filename {
        None => format!("context-proof-v1_{ts}.json"),
        // `filename` comes from the ctx_proof caller. Only a bare file name may
        // be joined onto proofs/: `Path::join` drops the base for an absolute
        // path, and `..` would walk out of it.
        Some(f) => {
            let mut components = Path::new(f).components();
            match (components.next(), components.next()) {
                (Some(std::path::Component::Normal(_)), None) => f.to_string(),
                _ => return Err(format!("filename must be a plain file name: {f}")),
            }
        }
    };
    let path = proofs_dir.join(name);

    let json = serde_json::to_string_pretty(proof).map_err(|e| e.to_string())?;
    // Proof artifacts may be attached to CI logs; always redact (even for admin).
    let json = crate::core::redaction::redact_text(&json);
    crate::config_io::write_atomic(&path, &json)?;

    PROOFS_WRITTEN.fetch_add(1, Ordering::Relaxed);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    LAST_WRITTEN_UNIX_MS.store(ms, Ordering::Relaxed);

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_has_required_fields() {
        let proof = collect_v1(
            ProofSources {
                project_root: Some(".".to_string()),
                ..Default::default()
            },
            ProofOptions {
                max_evidence: 5,
                max_ledger_files: 3,
            },
        );
        let v = serde_json::to_value(&proof).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert!(v["created_at"].as_str().unwrap_or_default().contains('T'));
        assert!(v["lean_ctx_version"].as_str().unwrap_or_default().len() >= 3);
        // The BLAKE3 policy hashes keep their historical wire name.
        assert!(v["role"]["policy_md5"].is_string(), "{v}");
        assert!(v["profile"]["policy_md5"].is_string(), "{v}");
    }

    #[test]
    fn write_project_proof_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let proof = collect_v1(
            ProofSources {
                project_root: Some(dir.path().to_string_lossy().to_string()),
                ..Default::default()
            },
            ProofOptions {
                max_evidence: 3,
                max_ledger_files: 2,
            },
        );
        let path = write_project_proof(dir.path(), &proof, None).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("\"schema_version\": 1"));
    }

    #[test]
    fn write_project_proof_rejects_filenames_outside_proofs_dir() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let proof = collect_v1(
            ProofSources {
                project_root: Some(project.to_string_lossy().to_string()),
                ..Default::default()
            },
            ProofOptions {
                max_evidence: 1,
                max_ledger_files: 1,
            },
        );
        let outside = dir.path().join("outside.json");
        let outside_abs = outside.to_string_lossy().to_string();
        for name in [
            "../../../outside.json",
            outside_abs.as_str(),
            "sub/dir.json",
            "..",
            ".",
            "",
        ] {
            assert!(
                write_project_proof(&project, &proof, Some(name)).is_err(),
                "filename {name:?} must be rejected"
            );
        }
        assert!(!outside.exists(), "nothing may be written outside proofs/");

        let path = write_project_proof(&project, &proof, Some("named.json")).unwrap();
        assert_eq!(path.file_name().unwrap(), "named.json");
        assert!(path.parent().unwrap().ends_with("proofs"));
    }
}
