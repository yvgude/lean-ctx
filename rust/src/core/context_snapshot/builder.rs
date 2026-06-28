//! Builds a [`ContextSnapshotV1`] from the live context stores (GL #1024).
//!
//! The builder is split into **pure projections** (`*_from_*` / `*_slice`),
//! which map a loaded store into a bounded snapshot slice and are unit-tested
//! with in-memory fixtures, and **impure anchors** (`git_anchor`,
//! `project_slice`), which read git / the filesystem. [`build`] orchestrates
//! them and finalizes the id (signing it when requested); [`create`] additionally
//! persists the snapshot and appends it to the append-only timeline.

use std::path::Path;
use std::time::Duration;

use crate::core::context_field::ContextState;
use crate::core::context_ir::{ContextIrSourceKindV1, ContextIrTotalsV1, ContextIrV1};
use crate::core::context_ledger::ContextLedger;
use crate::core::session::SessionState;

use super::digest::finalize_id;
use super::signing::sign_snapshot;
use super::types::{
    ContextSnapshotV1, GitAnchorV1, MAX_SNAPSHOT_LEDGER_ITEMS, MAX_SNAPSHOT_LINEAGE_ITEMS,
    MAX_SNAPSHOT_SESSION_LIST, SnapshotLedgerItemV1, SnapshotLedgerV1, SnapshotLineageItemV1,
    SnapshotLineageV1, SnapshotProjectV1, SnapshotRoiV1, SnapshotSessionV1,
};

/// Inputs for building a snapshot.
pub struct SnapshotOptions {
    /// Project root the snapshot is anchored to.
    pub project_root: String,
    /// Sign the snapshot with the publisher keypair (else just finalize the id).
    pub sign: bool,
}

/// Build an in-memory snapshot from the current store state, finalizing its id
/// (and signing it when `opts.sign`). Does not persist anything.
pub fn build(opts: &SnapshotOptions) -> Result<ContextSnapshotV1, String> {
    let mut snap = ContextSnapshotV1::new(
        chrono::Utc::now().to_rfc3339(),
        env!("CARGO_PKG_VERSION").to_string(),
    );
    snap.git = git_anchor(&opts.project_root);
    snap.project = project_slice(&opts.project_root);

    let ir = ContextIrV1::load();
    snap.roi = roi_from_totals(&ir.totals);
    snap.lineage = lineage_from_ir(&ir);

    snap.ledger = ledger_slice(&ContextLedger::load());
    snap.session =
        SessionState::load_latest_for_project_root(&opts.project_root).map(|s| session_slice(&s));

    snap.parent_id = super::timeline::head_id(&opts.project_root);

    if opts.sign {
        let (key, _newly_created) = crate::core::context_package::keys::load_or_create()?;
        sign_snapshot(&mut snap, &key)?;
    } else {
        finalize_id(&mut snap)?;
    }
    Ok(snap)
}

/// Build, persist, and append the snapshot to the project's timeline.
pub fn create(opts: &SnapshotOptions) -> Result<ContextSnapshotV1, String> {
    let snap = build(opts)?;
    super::timeline::write_snapshot(&opts.project_root, &snap)?;
    Ok(snap)
}

// --- pure projections -------------------------------------------------------

fn roi_from_totals(t: &ContextIrTotalsV1) -> SnapshotRoiV1 {
    let denom = t.input_tokens + t.tokens_saved;
    let compression_rate = if denom == 0 {
        0.0
    } else {
        t.tokens_saved as f64 / denom as f64
    };
    SnapshotRoiV1 {
        input_tokens: t.input_tokens,
        output_tokens: t.output_tokens,
        tokens_saved: t.tokens_saved,
        compression_rate,
    }
}

fn lineage_from_ir(ir: &ContextIrV1) -> SnapshotLineageV1 {
    let start = ir.items.len().saturating_sub(MAX_SNAPSHOT_LINEAGE_ITEMS);
    let items = ir.items[start..]
        .iter()
        .map(|it| SnapshotLineageItemV1 {
            seq: it.seq,
            kind: kind_str(&it.source.kind).to_string(),
            tool: it.source.tool.clone(),
            path: it.source.path.clone(),
            input_tokens: it.input_tokens as u64,
            output_tokens: it.output_tokens as u64,
            compression_ratio: it.compression_ratio,
            content_hash: it.verification.content_md5.clone(),
        })
        .collect();
    SnapshotLineageV1 {
        items_recorded: ir.totals.items_recorded,
        items,
    }
}

fn ledger_slice(led: &ContextLedger) -> SnapshotLedgerV1 {
    let start = led.entries.len().saturating_sub(MAX_SNAPSHOT_LEDGER_ITEMS);
    let items = led.entries[start..]
        .iter()
        .map(|e| SnapshotLedgerItemV1 {
            path: e.path.clone(),
            state: state_str(e.state.unwrap_or(ContextState::Candidate)).to_string(),
            phi: e.phi,
            sent_tokens: e.sent_tokens,
            original_tokens: e.original_tokens,
        })
        .collect();
    SnapshotLedgerV1 {
        window_size: led.window_size,
        total_tokens_sent: led.total_tokens_sent,
        total_tokens_saved: led.total_tokens_saved,
        items,
    }
}

fn session_slice(s: &SessionState) -> SnapshotSessionV1 {
    SnapshotSessionV1 {
        session_id: Some(s.id.clone()),
        task: s.task.as_ref().map(|t| t.description.clone()),
        decisions: s
            .decisions
            .iter()
            .take(MAX_SNAPSHOT_SESSION_LIST)
            .map(|d| d.summary.clone())
            .collect(),
        files_touched: s
            .files_touched
            .iter()
            .take(MAX_SNAPSHOT_SESSION_LIST)
            .map(|f| f.path.clone())
            .collect(),
        progress_pct: s.task.as_ref().and_then(|t| t.progress_pct),
    }
}

// --- impure anchors ---------------------------------------------------------

fn project_slice(project_root: &str) -> SnapshotProjectV1 {
    SnapshotProjectV1 {
        root_hash: Some(crate::core::project_hash::hash_project_root(project_root)),
        identity_hash: crate::core::project_hash::project_identity(project_root)
            .map(|id| crate::core::hasher::hash_str(&id)),
    }
}

fn git_anchor(project_root: &str) -> GitAnchorV1 {
    if !crate::core::git::git_available() {
        return GitAnchorV1::default();
    }
    let root = Path::new(project_root);
    let commit = git_str(root, &["rev-parse", "HEAD"]);
    let branch = git_str(root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let dirty = crate::core::git::run_git(
        &["status", "--porcelain"],
        root,
        Duration::from_secs(5),
        &[],
    )
    .is_ok_and(|o| o.success && !o.stdout.trim().is_empty());
    GitAnchorV1 {
        commit,
        branch,
        dirty,
    }
}

fn git_str(root: &Path, args: &[&str]) -> Option<String> {
    crate::core::git::run_git(args, root, Duration::from_secs(5), &[])
        .ok()
        .filter(|o| o.success)
        .map(|o| o.stdout.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn kind_str(k: &ContextIrSourceKindV1) -> &'static str {
    match k {
        ContextIrSourceKindV1::Read => "read",
        ContextIrSourceKindV1::Shell => "shell",
        ContextIrSourceKindV1::Search => "search",
        ContextIrSourceKindV1::Provider => "provider",
        ContextIrSourceKindV1::Other => "other",
    }
}

fn state_str(s: ContextState) -> &'static str {
    match s {
        ContextState::Candidate => "candidate",
        ContextState::Included => "included",
        ContextState::Excluded => "excluded",
        ContextState::Pinned => "pinned",
        ContextState::Stale => "stale",
        ContextState::Shadowed => "shadowed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_ir::{
        ContextIrItemV1, ContextIrSafetyV1, ContextIrSourceV1, ContextIrVerificationV1,
    };
    use crate::core::session::{Decision, FileTouched, TaskInfo};

    fn ir_item(seq: u64, kind: ContextIrSourceKindV1, path: &str) -> ContextIrItemV1 {
        ContextIrItemV1 {
            seq,
            created_at: "2026-06-28T00:00:00Z".into(),
            source: ContextIrSourceV1 {
                kind,
                tool: "ctx_read".into(),
                path: Some(path.into()),
                ..Default::default()
            },
            input_tokens: 500,
            output_tokens: 13,
            duration_us: 0,
            compression_ratio: 0.974,
            content_excerpt: String::new(),
            truncated: false,
            safety: ContextIrSafetyV1::default(),
            verification: ContextIrVerificationV1 {
                content_md5: Some("c".repeat(64)),
            },
        }
    }

    #[test]
    fn roi_handles_zero_and_normal() {
        let zero = roi_from_totals(&ContextIrTotalsV1::default());
        assert_eq!(zero.compression_rate, 0.0);

        let t = ContextIrTotalsV1 {
            items_recorded: 3,
            input_tokens: 200,
            output_tokens: 50,
            tokens_saved: 600,
        };
        let roi = roi_from_totals(&t);
        assert_eq!(roi.tokens_saved, 600);
        assert!(
            (roi.compression_rate - 0.75).abs() < 1e-9,
            "600/(200+600)=0.75"
        );
    }

    #[test]
    fn lineage_maps_fields_and_bounds_to_cap() {
        let mut ir = ContextIrV1::new();
        ir.totals.items_recorded = 999;
        for seq in 0..(MAX_SNAPSHOT_LINEAGE_ITEMS as u64 + 10) {
            ir.items
                .push(ir_item(seq, ContextIrSourceKindV1::Read, "src/a.rs"));
        }
        let slice = lineage_from_ir(&ir);
        assert_eq!(slice.items_recorded, 999);
        assert_eq!(slice.items.len(), MAX_SNAPSHOT_LINEAGE_ITEMS);
        // The cap keeps the most recent items (tail of the IR ring).
        assert_eq!(
            slice.items.last().unwrap().seq,
            MAX_SNAPSHOT_LINEAGE_ITEMS as u64 + 9
        );
        let first = &slice.items[0];
        assert_eq!(first.kind, "read");
        assert_eq!(first.tool, "ctx_read");
        assert_eq!(first.path.as_deref(), Some("src/a.rs"));
    }

    #[test]
    fn ledger_slice_maps_state_and_phi() {
        let mut led = ContextLedger::new();
        led.record("src/a.rs", "full", 500, 13);
        led.update_phi("src/a.rs", 0.9);
        led.set_state("src/a.rs", ContextState::Pinned);
        led.record("src/b.rs", "signatures", 800, 40);

        let slice = ledger_slice(&led);
        assert_eq!(slice.items.len(), 2);
        let a = slice.items.iter().find(|i| i.path == "src/a.rs").unwrap();
        assert_eq!(a.state, "pinned");
        assert_eq!(a.phi, Some(0.9));
        // record() marks a fresh entry as included (it just entered the window);
        // the candidate fallback only applies to entries with no recorded state.
        let b = slice.items.iter().find(|i| i.path == "src/b.rs").unwrap();
        assert_eq!(b.state, "included");
    }

    #[test]
    fn session_slice_projects_task_and_lists() {
        let mut s = SessionState::new();
        s.task = Some(TaskInfo {
            description: "Implement timeline".into(),
            intent: None,
            progress_pct: Some(40),
        });
        s.decisions.push(Decision {
            summary: "JSONL append-only index".into(),
            rationale: None,
            timestamp: chrono::Utc::now(),
        });
        s.files_touched.push(FileTouched {
            path: "rust/src/core/context_snapshot/timeline.rs".into(),
            file_ref: None,
            read_count: 1,
            modified: true,
            last_mode: "full".into(),
            tokens: 100,
            stale: false,
            context_item_id: None,
            summary: None,
        });

        let slice = session_slice(&s);
        assert_eq!(slice.task.as_deref(), Some("Implement timeline"));
        assert_eq!(slice.progress_pct, Some(40));
        assert_eq!(slice.decisions, vec!["JSONL append-only index".to_string()]);
        assert_eq!(
            slice.files_touched,
            vec!["rust/src/core/context_snapshot/timeline.rs".to_string()]
        );
    }

    #[test]
    fn kind_and_state_strings_are_snake_case() {
        assert_eq!(kind_str(&ContextIrSourceKindV1::Provider), "provider");
        assert_eq!(state_str(ContextState::Shadowed), "shadowed");
    }
}
