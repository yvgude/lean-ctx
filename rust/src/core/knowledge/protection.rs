// SPDX-License-Identifier: Apache-2.0
//! Current project policy at durable knowledge boundaries.
use std::path::Path;
use std::sync::Arc;

use super::ProjectKnowledge;
use crate::core::policy::{diagnostics, runtime};

#[cfg(test)]
#[path = "protection_tests.rs"]
mod tests;

pub(crate) fn current(root: &str) -> Result<Option<Arc<runtime::ActivePolicy>>, String> {
    if let Some(request) = diagnostics::request_project()
        && runtime::for_project(&request)?.is_some()
        && crate::core::pathutil::safe_canonicalize_bounded(&request, 2000)
            != crate::core::pathutil::safe_canonicalize_bounded(Path::new(root), 2000)
    {
        return Err("knowledge project does not match protected request".into());
    }
    let policy = runtime::for_project(Path::new(root))?;
    if policy
        .as_ref()
        .is_some_and(|p| !p.tool_allowed("ctx_knowledge"))
    {
        return Err("knowledge access withheld by current policy".into());
    }
    Ok(policy)
}

fn identity(value: &mut serde_json::Value) {
    for (collection, fields) in [
        ("facts", &["value"][..]),
        ("patterns", &["description", "examples"][..]),
        ("history", &["summary"][..]),
    ] {
        if let Some(items) = value[collection].as_array_mut() {
            for item in items {
                for field in fields {
                    item[*field] = serde_json::Value::Null;
                }
            }
        }
    }
}

pub(crate) fn view(
    knowledge: &ProjectKnowledge,
    policy: &runtime::ActivePolicy,
) -> Result<ProjectKnowledge, String> {
    let original = serde_json::to_value(knowledge).map_err(|_| "knowledge cannot be inspected")?;
    let safe = diagnostics::inspect(&original, Some(policy))
        .ok_or("knowledge content withheld by current policy")?;
    let mut old_identity = original;
    let mut new_identity = safe.clone();
    identity(&mut old_identity);
    identity(&mut new_identity);
    if old_identity != new_identity {
        return Err("knowledge identity requires an explicit policy-safe migration".into());
    }
    let mut result: ProjectKnowledge = serde_json::from_value(safe)
        .map_err(|_| "knowledge policy result is not structurally valid")?;
    result.rebuild_index();
    result.withheld.clone_from(&knowledge.withheld);
    Ok(result)
}

pub(crate) fn require_safe(
    knowledge: &ProjectKnowledge,
    policy: &runtime::ActivePolicy,
) -> Result<(), String> {
    let safe = view(knowledge, policy)?;
    if serde_json::to_value(knowledge).ok() != serde_json::to_value(&safe).ok() {
        return Err("existing knowledge requires an explicit policy-safe migration".into());
    }
    Ok(())
}
