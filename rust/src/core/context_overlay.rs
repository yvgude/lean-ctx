//! Reversible context overlays — user/policy manipulations that modify
//! context items without changing source files ("synaptic modulation").

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

use super::context_field::{ContextItemId, ContextState, ViewKind};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OverlayId(pub String);

impl OverlayId {
    #[must_use]
    pub fn generate(target: &ContextItemId) -> Self {
        Self(format!(
            "ov_{}_{}",
            target.as_str(),
            Utc::now().timestamp_millis()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OverlayId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OverlayOp {
    Include,
    Exclude { reason: String },
    Pin { verbatim: bool },
    Unpin,
    Rewrite { content: String },
    SetView(ViewKind),
    SetPriority { set_priority: f64 },
    MarkOutdated,
    Expire { after_secs: u64 },
}

impl OverlayOp {
    fn discriminant(&self) -> &'static str {
        match self {
            Self::Include => "include",
            Self::Exclude { .. } => "exclude",
            Self::Pin { .. } => "pin",
            Self::Unpin => "unpin",
            Self::Rewrite { .. } => "rewrite",
            Self::SetView(_) => "set_view",
            Self::SetPriority { .. } => "set_priority",
            Self::MarkOutdated => "mark_outdated",
            Self::Expire { .. } => "expire",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayScope {
    Call,
    Session,
    Project,
    Agent(String),
    Global,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayAuthor {
    User,
    Policy(String),
    Agent(String),
}

// ---------------------------------------------------------------------------
// ContextOverlay
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextOverlay {
    pub id: OverlayId,
    pub target: ContextItemId,
    pub operation: OverlayOp,
    pub scope: OverlayScope,
    pub before_hash: String,
    pub author: OverlayAuthor,
    pub created_at: DateTime<Utc>,
    pub stale: bool,
}

impl ContextOverlay {
    #[must_use]
    pub fn new(
        target: ContextItemId,
        operation: OverlayOp,
        scope: OverlayScope,
        before_hash: String,
        author: OverlayAuthor,
    ) -> Self {
        Self {
            id: OverlayId::generate(&target),
            target,
            operation,
            scope,
            before_hash,
            author,
            created_at: Utc::now(),
            stale: false,
        }
    }

    fn is_expired(&self) -> bool {
        if let OverlayOp::Expire { after_secs } = &self.operation {
            let elapsed = Utc::now()
                .signed_duration_since(self.created_at)
                .num_seconds();
            elapsed >= *after_secs as i64
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// OverlayStore
// ---------------------------------------------------------------------------

const OVERLAY_FILE: &str = ".lean-ctx/overlays.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OverlayStore {
    overlays: Vec<ContextOverlay>,
}

impl OverlayStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an overlay, replacing any existing overlay with the same
    /// target + operation discriminant.
    pub fn add(&mut self, overlay: ContextOverlay) {
        let disc = overlay.operation.discriminant();
        self.overlays.retain(|existing| {
            !(existing.target == overlay.target && existing.operation.discriminant() == disc)
        });
        self.overlays.push(overlay);
    }

    pub fn remove(&mut self, id: &OverlayId) {
        self.overlays.retain(|o| o.id != *id);
    }

    #[must_use]
    pub fn for_item(&self, target: &ContextItemId) -> Vec<&ContextOverlay> {
        self.overlays
            .iter()
            .filter(|o| o.target == *target)
            .collect()
    }

    #[must_use]
    pub fn active_for_scope(&self, scope: &OverlayScope) -> Vec<&ContextOverlay> {
        self.overlays.iter().filter(|o| o.scope == *scope).collect()
    }

    /// Applies all overlays for `target` to `current_state`, returning the
    /// effective state. Later overlays take precedence.
    #[must_use]
    pub fn apply_to_state(
        &self,
        target: &ContextItemId,
        current_state: ContextState,
    ) -> ContextState {
        let mut state = current_state;
        for overlay in self.overlays.iter().filter(|o| o.target == *target) {
            state = match &overlay.operation {
                OverlayOp::Include => ContextState::Included,
                OverlayOp::Exclude { .. } => ContextState::Excluded,
                OverlayOp::Pin { .. } => ContextState::Pinned,
                OverlayOp::Unpin => ContextState::Candidate,
                OverlayOp::MarkOutdated => ContextState::Stale,
                _ => state,
            };
        }
        state
    }

    /// Marks overlays as stale when the source hash has changed.
    pub fn mark_stale_by_hash(&mut self, target: &ContextItemId, new_hash: &str) {
        for overlay in self.overlays.iter_mut().filter(|o| o.target == *target) {
            if overlay.before_hash != new_hash {
                overlay.stale = true;
            }
        }
    }

    /// Removes overlays whose `Expire` operation has elapsed.
    pub fn prune_expired(&mut self) {
        self.overlays.retain(|o| !o.is_expired());
    }

    /// Returns all overlays for `target`, ordered by creation time.
    #[must_use]
    pub fn history(&self, target: &ContextItemId) -> Vec<&ContextOverlay> {
        let mut items: Vec<&ContextOverlay> = self.for_item(target);
        items.sort_by_key(|o| o.created_at);
        items
    }

    pub fn remove_for_item(&mut self, target: &ContextItemId) {
        self.overlays.retain(|o| o.target != *target);
    }

    #[must_use]
    pub fn all(&self) -> &[ContextOverlay] {
        &self.overlays
    }

    pub fn save_project(&self, project_root: &Path) -> Result<(), String> {
        let project_dir = crate::core::pathutil::safe_project_data_dir(project_root)?;
        let path = project_dir.join("overlays.json");
        let json =
            serde_json::to_string_pretty(self).map_err(|e| format!("serialize overlays: {e}"))?;
        crate::config_io::write_atomic(&path, &json)
    }

    pub fn load_project(project_root: &Path) -> Self {
        if crate::core::pathutil::is_data_dir_collision(project_root) {
            tracing::debug!(
                "Skipping overlay load: project root {} collides with data directory",
                project_root.display()
            );
            return Self::default();
        }
        let path = project_root.join(OVERLAY_FILE);
        let mut store: Self = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let now = chrono::Utc::now();
        let session_ttl = chrono::Duration::hours(24);
        store.overlays.retain(|o| match &o.scope {
            OverlayScope::Session | OverlayScope::Call => {
                now.signed_duration_since(o.created_at) < session_ttl
            }
            _ => true,
        });
        store
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_target() -> ContextItemId {
        ContextItemId::from_file("src/main.rs")
    }

    fn make_overlay(op: OverlayOp) -> ContextOverlay {
        ContextOverlay::new(
            make_target(),
            op,
            OverlayScope::Session,
            "abc123".into(),
            OverlayAuthor::User,
        )
    }

    // -- State transitions ---------------------------------------------------

    #[test]
    fn exclude_sets_excluded_state() {
        let mut store = OverlayStore::new();
        store.add(make_overlay(OverlayOp::Exclude {
            reason: "too large".into(),
        }));
        let state = store.apply_to_state(&make_target(), ContextState::Candidate);
        assert_eq!(state, ContextState::Excluded);
    }

    #[test]
    fn include_sets_included_state() {
        let mut store = OverlayStore::new();
        store.add(make_overlay(OverlayOp::Include));
        let state = store.apply_to_state(&make_target(), ContextState::Candidate);
        assert_eq!(state, ContextState::Included);
    }

    #[test]
    fn pin_sets_pinned_state() {
        let mut store = OverlayStore::new();
        store.add(make_overlay(OverlayOp::Pin { verbatim: true }));
        let state = store.apply_to_state(&make_target(), ContextState::Candidate);
        assert_eq!(state, ContextState::Pinned);
    }

    #[test]
    fn unpin_resets_to_candidate() {
        let mut store = OverlayStore::new();
        store.add(make_overlay(OverlayOp::Unpin));
        let state = store.apply_to_state(&make_target(), ContextState::Pinned);
        assert_eq!(state, ContextState::Candidate);
    }

    #[test]
    fn mark_outdated_sets_stale_state() {
        let mut store = OverlayStore::new();
        store.add(make_overlay(OverlayOp::MarkOutdated));
        let state = store.apply_to_state(&make_target(), ContextState::Included);
        assert_eq!(state, ContextState::Stale);
    }

    #[test]
    fn non_state_ops_preserve_current_state() {
        let mut store = OverlayStore::new();
        store.add(make_overlay(OverlayOp::SetPriority { set_priority: 0.9 }));
        let state = store.apply_to_state(&make_target(), ContextState::Included);
        assert_eq!(state, ContextState::Included);
    }

    // -- Staleness -----------------------------------------------------------

    #[test]
    fn mark_stale_when_hash_changes() {
        let mut store = OverlayStore::new();
        store.add(make_overlay(OverlayOp::Include));
        assert!(!store.overlays[0].stale);

        store.mark_stale_by_hash(&make_target(), "different_hash");
        assert!(store.overlays[0].stale);
    }

    #[test]
    fn no_stale_when_hash_matches() {
        let mut store = OverlayStore::new();
        store.add(make_overlay(OverlayOp::Include));
        store.mark_stale_by_hash(&make_target(), "abc123");
        assert!(!store.overlays[0].stale);
    }

    // -- Scope filtering -----------------------------------------------------

    #[test]
    fn active_for_scope_filters_correctly() {
        let mut store = OverlayStore::new();
        store.add(make_overlay(OverlayOp::Include));
        store.add(ContextOverlay::new(
            ContextItemId::from_file("other.rs"),
            OverlayOp::Include,
            OverlayScope::Project,
            "xyz".into(),
            OverlayAuthor::User,
        ));

        let session = store.active_for_scope(&OverlayScope::Session);
        assert_eq!(session.len(), 1);

        let project = store.active_for_scope(&OverlayScope::Project);
        assert_eq!(project.len(), 1);

        let global = store.active_for_scope(&OverlayScope::Global);
        assert!(global.is_empty());
    }

    // -- Expiry pruning ------------------------------------------------------

    #[test]
    fn prune_removes_expired_overlays() {
        let mut store = OverlayStore::new();
        let mut expired = make_overlay(OverlayOp::Expire { after_secs: 0 });
        expired.created_at = Utc::now() - chrono::Duration::seconds(10);
        store.add(expired);
        store.add(make_overlay(OverlayOp::Include));

        assert_eq!(store.overlays.len(), 2);
        store.prune_expired();
        assert_eq!(store.overlays.len(), 1);
    }

    #[test]
    fn prune_keeps_unexpired_overlays() {
        let mut store = OverlayStore::new();
        store.add(make_overlay(OverlayOp::Expire { after_secs: 99999 }));
        store.prune_expired();
        assert_eq!(store.overlays.len(), 1);
    }

    // -- Persistence roundtrip -----------------------------------------------

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().expect("tmp dir");
        let root = dir.path();

        let mut store = OverlayStore::new();
        store.add(make_overlay(OverlayOp::Include));
        store.add(make_overlay(OverlayOp::Exclude {
            reason: "noise".into(),
        }));
        store.add(make_overlay(OverlayOp::SetView(ViewKind::Signatures)));

        store.save_project(root).expect("save");
        let loaded = OverlayStore::load_project(root);
        assert_eq!(loaded.overlays.len(), store.overlays.len());
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = tempfile::tempdir().expect("tmp dir");
        let store = OverlayStore::load_project(dir.path());
        assert!(store.overlays.is_empty());
    }

    // -- Override semantics --------------------------------------------------

    #[test]
    fn newer_overlay_replaces_same_target_and_op() {
        let mut store = OverlayStore::new();
        store.add(make_overlay(OverlayOp::Exclude {
            reason: "first".into(),
        }));
        assert_eq!(store.overlays.len(), 1);
        assert_eq!(
            store.overlays[0].operation,
            OverlayOp::Exclude {
                reason: "first".into()
            }
        );

        store.add(make_overlay(OverlayOp::Exclude {
            reason: "second".into(),
        }));
        assert_eq!(store.overlays.len(), 1);
        assert_eq!(
            store.overlays[0].operation,
            OverlayOp::Exclude {
                reason: "second".into()
            }
        );
    }

    #[test]
    fn different_ops_coexist_for_same_target() {
        let mut store = OverlayStore::new();
        store.add(make_overlay(OverlayOp::Include));
        store.add(make_overlay(OverlayOp::SetPriority { set_priority: 0.8 }));
        assert_eq!(store.overlays.len(), 2);
    }

    // -- History order -------------------------------------------------------

    #[test]
    fn history_returns_chronological_order() {
        let mut store = OverlayStore::new();
        let mut older = make_overlay(OverlayOp::Include);
        older.created_at = Utc::now() - chrono::Duration::seconds(60);
        store.overlays.push(older);

        let newer = make_overlay(OverlayOp::SetPriority { set_priority: 0.5 });
        store.overlays.push(newer);

        let hist = store.history(&make_target());
        assert_eq!(hist.len(), 2);
        assert!(hist[0].created_at <= hist[1].created_at);
    }

    // -- Remove --------------------------------------------------------------

    #[test]
    fn remove_deletes_by_id() {
        let mut store = OverlayStore::new();
        let ov = make_overlay(OverlayOp::Include);
        let id = ov.id.clone();
        store.add(ov);
        assert_eq!(store.overlays.len(), 1);

        store.remove(&id);
        assert!(store.overlays.is_empty());
    }
}
