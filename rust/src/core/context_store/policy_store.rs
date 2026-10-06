// SPDX-License-Identifier: Apache-2.0
//! The promoted read-strategy policy of one tenant/project scope, as the
//! optional licensed runtime returned it: the active policy and the last
//! stable one it replaced, each with the learner's artifact the Engine keeps
//! opaque and hands back for monitoring.
//!
//! Nothing here decides. Promotion and rollback are the runtime's verdicts on
//! evidence; this store only keeps them, and the planner reads the active
//! policy — in shadow (recorded as a decision reason) unless the user switched
//! it to apply.
//!
//! A kept policy is local state like any other: it stays in effect without
//! the runtime (open core — only learning is licensed) until the user rolls
//! it back. Monitoring compares a policy with its stable predecessor, so the
//! first promotion has none to fall back to except "no policy" via rollback.

use std::path::{Path, PathBuf};

use lean_ctx_protocol::context_policy_evidence::ContextPolicyV1;
use serde::{Deserialize, Serialize};

const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolicyEntry {
    pub policy: ContextPolicyV1,
    /// The learner's artifact, opaque to the Engine.
    pub artifact: serde_json::Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScopePolicies {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<PolicyEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_stable: Option<PolicyEntry>,
}

fn path_in(dir: &Path, scope: &str) -> PathBuf {
    let digest = blake3::hash(scope.as_bytes()).to_hex();
    dir.join(format!("{}.json", &digest[..32]))
}

fn policy_dir() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join("context-policy"))
}

fn valid(entry: &PolicyEntry) -> bool {
    entry.policy.validate().is_ok()
}

/// A missing file is no policy. A damaged one is also no policy: the planner
/// then runs exactly as without a runtime.
pub(crate) fn load_in(dir: &Path, scope: &str) -> ScopePolicies {
    let path = path_in(dir, scope);
    let Ok(metadata) = std::fs::metadata(&path) else {
        return ScopePolicies::default();
    };
    if metadata.len() > MAX_FILE_BYTES {
        return ScopePolicies::default();
    }
    std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ScopePolicies>(&bytes).ok())
        .filter(|stored| {
            stored.schema_version == 1
                && stored.active.as_ref().is_none_or(valid)
                && stored.last_stable.as_ref().is_none_or(valid)
        })
        .unwrap_or_default()
}

pub(crate) fn save_in(dir: &Path, scope: &str, policies: &ScopePolicies) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut stored = policies.clone();
    stored.schema_version = 1;
    let bytes = serde_json::to_vec_pretty(&stored).map_err(|e| e.to_string())?;
    // What could not be read back must not be kept as if it were.
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("the policy exceeds the store's size bound".into());
    }
    crate::core::atomic_fs::try_atomic_write(&path_in(dir, scope), &bytes, None)
        .map_err(|e| e.to_string())
}

impl ScopePolicies {
    /// A promotion: the new policy becomes active, the previous one stable.
    pub(crate) fn promote(&mut self, entry: PolicyEntry) {
        self.last_stable = self.active.take();
        self.active = Some(entry);
    }

    /// A rollback to `restored` (the runtime's verdict or the user's choice):
    /// it becomes active and nothing older is known to be stable.
    pub(crate) fn roll_back_to(&mut self, restored: PolicyEntry) {
        self.active = Some(restored);
        self.last_stable = None;
    }
}

/// Applies `change` only if the stored policies still equal `expected`, the
/// state the caller decided on, under the store's lock: a promotion or
/// rollback that landed in between fails this one instead of being lost.
pub(crate) fn update_in(
    dir: &Path,
    scope: &str,
    expected: &ScopePolicies,
    change: impl FnOnce(&mut ScopePolicies),
) -> Result<ScopePolicies, String> {
    let _lock = crate::core::context_admission::receipt_store::lock_task_files(dir)
        .ok_or("the policy store is busy")?;
    let mut current = load_in(dir, scope);
    if current != *expected {
        return Err("the scope's policy changed concurrently; retry".into());
    }
    change(&mut current);
    save_in(dir, scope, &current)?;
    Ok(load_in(dir, scope))
}

pub(crate) fn update(
    scope: &str,
    expected: &ScopePolicies,
    change: impl FnOnce(&mut ScopePolicies),
) -> Result<ScopePolicies, String> {
    update_in(
        &policy_dir().ok_or("no lean-ctx data directory")?,
        scope,
        expected,
        change,
    )
}

pub(crate) fn load(scope: &str) -> ScopePolicies {
    policy_dir()
        .map(|dir| load_in(&dir, scope))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::context_policy_evidence::CONTEXT_POLICY_VERSION;

    fn entry(version: u64) -> PolicyEntry {
        PolicyEntry {
            policy: ContextPolicyV1 {
                schema_version: CONTEXT_POLICY_VERSION,
                version,
                parent_version: version.checked_sub(1).filter(|v| *v > 0),
                entries: Vec::new(),
                learner_digest: "b".repeat(64),
            },
            artifact: serde_json::json!({"version": version}),
        }
    }

    #[test]
    fn promotion_keeps_the_last_stable_and_damage_means_no_policy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let scope = "[null,\"p\"]";
        assert_eq!(load_in(dir.path(), scope), ScopePolicies::default());

        let mut policies = ScopePolicies::default();
        policies.promote(entry(1));
        policies.promote(entry(2));
        save_in(dir.path(), scope, &policies).expect("save");
        let loaded = load_in(dir.path(), scope);
        assert_eq!(loaded.active, Some(entry(2)));
        assert_eq!(loaded.last_stable, Some(entry(1)));
        assert_eq!(
            load_in(dir.path(), "[null,\"other\"]"),
            ScopePolicies::default(),
            "another project never sees this policy"
        );

        // Two writers that decided on the same state: the second one fails
        // instead of silently replacing the first one's promotion.
        let decided = loaded.clone();
        let first =
            update_in(dir.path(), scope, &decided, |p| p.promote(entry(3))).expect("first writer");
        assert_eq!(first.active, Some(entry(3)));
        assert!(update_in(dir.path(), scope, &decided, |p| p.roll_back_to(entry(1))).is_err());
        assert_eq!(load_in(dir.path(), scope), first);

        let mut rolled = loaded;
        rolled.roll_back_to(entry(1));
        assert_eq!((rolled.active, rolled.last_stable), (Some(entry(1)), None));

        let mut invalid = entry(4);
        invalid.policy.learner_digest = "not hex".into();
        policies.promote(invalid);
        save_in(dir.path(), scope, &policies).expect("save");
        assert_eq!(
            load_in(dir.path(), scope),
            ScopePolicies::default(),
            "an invalid stored policy is no policy"
        );
        std::fs::write(path_in(dir.path(), scope), "{broken").expect("damage");
        assert_eq!(load_in(dir.path(), scope), ScopePolicies::default());

        let mut oversized = ScopePolicies::default();
        let mut huge = entry(5);
        huge.artifact = serde_json::json!({"blob": "x".repeat(MAX_FILE_BYTES as usize)});
        oversized.promote(huge);
        assert!(
            save_in(dir.path(), scope, &oversized).is_err(),
            "a policy that would read back as none is refused, not kept"
        );
    }
}
