//! Standalone package verification (spec §8 integrity, §9 signing).
//!
//! `lean-ctx pack verify` and the import path share these primitives. All
//! hashing operates on the *document text* of the content member — never on
//! re-serialized parsed values, which would be lossy across languages
//! (a writer's `1.0` re-serializes as `1` in JavaScript and breaks the hash).

use sha2::{Digest, Sha256};
use std::path::Path;

use super::content::{
    CHECKPOINT_PACKAGE_SCHEMA_V1, CheckpointPackageContentV1, MAX_CHECKPOINT_ENTRIES,
    MAX_CHECKPOINT_PACKAGE_BYTES, MAX_CHECKPOINT_PACKAGE_PINS, MAX_CHECKPOINT_REFS,
    MAX_CHECKPOINT_SOURCES, PackageContent,
};
use super::manifest::{PackageKind, PackageLayer, PackageManifest};

mod text;
pub(crate) use text::compact_json_text;

/// Extract the exact text of one top-level member's value from a JSON object
/// document, so integrity hashing sees the writer's bytes (spec §8).
pub(crate) fn extract_top_level_value_text<'a>(doc: &'a str, member: &str) -> Option<&'a str> {
    let bytes = doc.as_bytes();
    let n = bytes.len();
    let mut i = 0;

    let skip_ws = |i: &mut usize| {
        while *i < n && matches!(bytes[*i], b' ' | b'\t' | b'\n' | b'\r') {
            *i += 1;
        }
    };
    let skip_string = |i: &mut usize| {
        *i += 1; // opening quote
        while *i < n {
            match bytes[*i] {
                b'\\' => *i += 2,
                b'"' => {
                    *i += 1;
                    return;
                }
                _ => *i += 1,
            }
        }
    };
    let skip_value = |i: &mut usize| {
        skip_ws(i);
        match bytes.get(*i) {
            Some(b'"') => skip_string(i),
            Some(&open @ (b'{' | b'[')) => {
                let close = if open == b'{' { b'}' } else { b']' };
                let mut depth = 0usize;
                while *i < n {
                    match bytes[*i] {
                        b'"' => {
                            skip_string(i);
                            continue;
                        }
                        c if c == open => depth += 1,
                        c if c == close => {
                            depth -= 1;
                            if depth == 0 {
                                *i += 1;
                                return;
                            }
                        }
                        _ => {}
                    }
                    *i += 1;
                }
            }
            _ => {
                while *i < n
                    && !matches!(bytes[*i], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
                {
                    *i += 1;
                }
            }
        }
    };

    skip_ws(&mut i);
    if bytes.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    loop {
        skip_ws(&mut i);
        match bytes.get(i) {
            Some(b'"') => {}
            _ => return None,
        }
        let key_start = i;
        skip_string(&mut i);
        let key: String = serde_json::from_str(&doc[key_start..i]).ok()?;
        skip_ws(&mut i);
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        skip_ws(&mut i);
        if key == member {
            let start = i;
            skip_value(&mut i);
            return Some(&doc[start..i]);
        }
        skip_value(&mut i);
        skip_ws(&mut i);
        if bytes.get(i) == Some(&b',') {
            i += 1;
        }
    }
}

/// Kind ↔ payload coherence (GH #724/#726): the declared `kind` must match
/// the content payload it ships, so a mislabeled package can never route
/// into the wrong trust chain (an "addon" without an addon manifest, or a
/// context pack smuggling one in).
pub(crate) fn validate_kind_coherence(
    manifest: &PackageManifest,
    content: &PackageContent,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    match manifest.kind {
        PackageKind::Addon => {
            errors.push("kind=addon packages are no longer supported".into());
        }

        PackageKind::Skills => {
            if content.addon.is_some() {
                errors.push("content.addon payload requires kind=addon".into());
            }
            match &content.documents {
                None => errors.push("kind=skills requires a content.documents payload".into()),
                Some(docs) => validate_documents(docs, &mut errors),
            }
        }
        PackageKind::Context | PackageKind::Grammar => {
            if content.addon.is_some() {
                errors.push(format!(
                    "content.addon payload requires kind=addon (manifest declares kind={})",
                    manifest.kind.as_str()
                ));
            }
            if content.documents.is_some() {
                errors.push(format!(
                    "content.documents payload requires kind=skills (manifest declares kind={})",
                    manifest.kind.as_str()
                ));
            }
        }
    }
    let has_checkpoint_layer = manifest.has_layer(PackageLayer::Checkpoint);
    if has_checkpoint_layer != content.checkpoint.is_some() {
        errors.push(
            "manifest checkpoint layer and content.checkpoint must be present together".into(),
        );
    }
    if let Some(checkpoint) = &content.checkpoint {
        if manifest.schema_version != crate::core::contracts::CONTEXT_PACKAGE_V2_SCHEMA_VERSION
            || manifest.kind != PackageKind::Context
        {
            errors.push("checkpoint content requires schema_version 2 and kind=context".into());
        }
        validate_checkpoint_content(checkpoint, &mut errors);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn validate_checkpoint_content(portable: &CheckpointPackageContentV1, errors: &mut Vec<String>) {
    if portable.schema_version != CHECKPOINT_PACKAGE_SCHEMA_V1 {
        errors.push(format!(
            "unsupported checkpoint package schema `{}`",
            portable.schema_version
        ));
        return;
    }
    let Ok(encoded) = serde_json::to_vec(portable) else {
        errors.push("checkpoint content is not canonical JSON".into());
        return;
    };
    if encoded.len() > MAX_CHECKPOINT_PACKAGE_BYTES {
        errors.push(format!(
            "checkpoint content exceeds {MAX_CHECKPOINT_PACKAGE_BYTES} byte cap"
        ));
        return;
    }
    validate_checkpoint_object(&portable.checkpoint, errors);
    validate_migration_provenance(
        portable.migration_provenance.as_ref(),
        &portable.checkpoint,
        errors,
    );

    let mut absolute_paths = Vec::new();
    collect_non_portable_paths(&portable.checkpoint, "$.checkpoint", &mut absolute_paths);
    absolute_paths.sort();
    absolute_paths.dedup();
    let mut declared = portable.non_portable_fields.clone();
    declared.sort();
    declared.dedup();
    if declared != portable.non_portable_fields
        || declared.len() > 256
        || declared
            .iter()
            .any(|item| item.is_empty() || item.len() > 1024 || item.chars().any(char::is_control))
        || declared != absolute_paths
    {
        errors.push(
            "non_portable_fields must exactly classify every machine-local absolute path".into(),
        );
    }

    let portable_text = serde_json::to_string(portable).unwrap_or_default();
    if !crate::core::secret_detection::detect_secrets(&portable_text).is_empty() {
        errors.push("checkpoint content contains credential-shaped material".into());
    }
}

fn validate_checkpoint_object(value: &serde_json::Value, errors: &mut Vec<String>) {
    const KEYS: &[&str] = &[
        "schema_version",
        "checkpoint_id",
        "workspace_id",
        "state_digest",
        "state_schema_version",
        "workspace_state_ref",
        "logical_state",
        "source_anchors",
        "recovery_refs",
        "package_pins",
        "package_lock_digest",
        "policy_digest",
        "project_context_digest",
        "lineage",
        "engine_identity",
        "sdk_contract",
        "envelope_digest",
    ];
    let Some(object) = value.as_object() else {
        errors.push("checkpoint envelope must be an object".into());
        return;
    };
    validate_exact_keys(object, KEYS, "checkpoint", errors);
    if object
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        != Some("leanctx.context-checkpoint/v2")
        || object
            .get("state_schema_version")
            .and_then(serde_json::Value::as_str)
            != Some("leanctx.workspace.state/v1")
        || object
            .get("sdk_contract")
            .and_then(serde_json::Value::as_str)
            != Some("leanctx-product-sdk-research/p6")
    {
        errors.push("checkpoint contract identity is unsupported".into());
    }
    for name in ["checkpoint_id", "workspace_id"] {
        let valid = object
            .get(name)
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| uuid::Uuid::parse_str(raw).ok().map(|parsed| (raw, parsed)))
            .is_some_and(|(raw, parsed)| parsed.hyphenated().to_string() == raw);
        if !valid {
            errors.push(format!("checkpoint.{name} must be a canonical UUID"));
        }
    }
    for name in [
        "state_digest",
        "policy_digest",
        "project_context_digest",
        "envelope_digest",
    ] {
        if !object.get(name).is_some_and(valid_prefixed_digest) {
            errors.push(format!("checkpoint.{name} must be a sha256 digest"));
        }
    }
    if !object.get("workspace_state_ref").is_some_and(|item| {
        item.as_str().is_some_and(|raw| {
            raw.starts_with("event:sha256:")
                && raw.len() == 77
                && raw[13..]
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    }) {
        errors.push("checkpoint.workspace_state_ref must bind an event digest".into());
    }
    if !object
        .get("package_lock_digest")
        .is_some_and(|item| item.is_null() || valid_prefixed_digest(item))
    {
        errors.push("checkpoint.package_lock_digest is invalid".into());
    }

    let arrays = [
        ("source_anchors", MAX_CHECKPOINT_SOURCES),
        ("recovery_refs", MAX_CHECKPOINT_REFS),
        ("package_pins", MAX_CHECKPOINT_PACKAGE_PINS),
    ];
    for (name, cap) in arrays {
        if object
            .get(name)
            .and_then(serde_json::Value::as_array)
            .is_none_or(|items| items.len() > cap)
        {
            errors.push(format!("checkpoint.{name} is missing or exceeds cap {cap}"));
        }
    }
    let Some(logical) = object
        .get("logical_state")
        .and_then(serde_json::Value::as_object)
    else {
        errors.push("checkpoint.logical_state must be an object".into());
        return;
    };
    const LOGICAL_KEYS: &[&str] = &[
        "schema_version",
        "workspace_id",
        "policy",
        "sources",
        "entries",
        "package_pins",
        "package_lock_digest",
    ];
    validate_exact_keys(logical, LOGICAL_KEYS, "checkpoint.logical_state", errors);
    if logical
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        != Some("leanctx.workspace.state/v1")
    {
        errors.push("checkpoint logical-state schema is unsupported".into());
    }
    validate_workspace_policy(logical.get("policy"), errors);
    validate_lineage(object.get("lineage"), object.get("workspace_id"), errors);
    validate_engine_identity(object.get("engine_identity"), errors);
    if logical.get("workspace_id") != object.get("workspace_id")
        || logical.get("sources") != object.get("source_anchors")
        || logical.get("package_pins") != object.get("package_pins")
        || logical.get("package_lock_digest") != object.get("package_lock_digest")
    {
        errors.push("checkpoint cross-field projections disagree".into());
    }
    if logical
        .get("sources")
        .and_then(serde_json::Value::as_array)
        .is_none_or(|items| items.len() > MAX_CHECKPOINT_SOURCES)
        || logical
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|items| items.len() > MAX_CHECKPOINT_ENTRIES)
    {
        errors.push("checkpoint logical-state arrays exceed bounds".into());
    }
    let source_ids = logical
        .get("sources")
        .and_then(serde_json::Value::as_array)
        .map(|sources| {
            sources
                .iter()
                .filter_map(|source| source.get("source_id").and_then(serde_json::Value::as_str))
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    if let Some(sources) = logical.get("sources").and_then(serde_json::Value::as_array) {
        validate_source_anchors(sources, errors);
    }
    if let Some(entries) = logical.get("entries").and_then(serde_json::Value::as_array) {
        validate_context_entries(entries, &source_ids, errors);
        let mut expected_refs = entries
            .iter()
            .filter_map(|entry| {
                entry
                    .get("recovery_refs")
                    .and_then(serde_json::Value::as_array)
            })
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        expected_refs.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
        expected_refs.dedup();
        if object
            .get("recovery_refs")
            .and_then(serde_json::Value::as_array)
            != Some(&expected_refs)
        {
            errors.push("checkpoint recovery refs disagree with context entries".into());
        }
    }
    if let Some(refs) = object
        .get("recovery_refs")
        .and_then(serde_json::Value::as_array)
    {
        validate_string_array(
            refs,
            MAX_CHECKPOINT_REFS,
            512,
            true,
            "checkpoint recovery refs",
            errors,
        );
    }
    if let Some(pins) = logical
        .get("package_pins")
        .and_then(serde_json::Value::as_array)
    {
        validate_package_pins(pins, logical.get("package_lock_digest"), errors);
    }
    validate_domain_digest(
        "leanctx.workspace.state.v1",
        &serde_json::Value::Object(logical.clone()),
        object.get("state_digest"),
        "checkpoint.state_digest",
        errors,
    );
    if let Some(policy) = logical.get("policy") {
        validate_domain_digest(
            "leanctx.workspace.policy.v1",
            policy,
            object.get("policy_digest"),
            "checkpoint.policy_digest",
            errors,
        );
    }
    if let Some(entries) = logical.get("entries") {
        validate_domain_digest(
            "leanctx.project-context.state.v1",
            entries,
            object.get("project_context_digest"),
            "checkpoint.project_context_digest",
            errors,
        );
    }
    let mut unsigned = object.clone();
    unsigned.remove("envelope_digest");
    validate_domain_digest(
        "leanctx.checkpoint.envelope.v2",
        &serde_json::Value::Object(unsigned),
        object.get("envelope_digest"),
        "checkpoint.envelope_digest",
        errors,
    );
}

fn validate_workspace_policy(value: Option<&serde_json::Value>, errors: &mut Vec<String>) {
    const KEYS: &[&str] = &[
        "schema_version",
        "allowed_categories",
        "max_events",
        "max_context_entries",
        "max_entry_bytes",
        "max_context_bytes",
        "max_sources",
        "max_sessions",
        "allow_external_sources",
    ];
    let Some(object) = value.and_then(serde_json::Value::as_object) else {
        errors.push("checkpoint workspace policy must be an object".into());
        return;
    };
    validate_exact_keys(object, KEYS, "checkpoint workspace policy", errors);
    let Some(categories) = object
        .get("allowed_categories")
        .and_then(serde_json::Value::as_array)
    else {
        errors.push("checkpoint workspace policy categories must be an array".into());
        return;
    };
    let category_values = categories
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    if object
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        != Some("leanctx.workspace-policy/v1")
        || category_values.len() != categories.len()
        || !category_values.windows(2).all(|pair| pair[0] < pair[1])
        || category_values.iter().any(|category| {
            !matches!(
                *category,
                "facts" | "decisions" | "constraints" | "unresolved_questions" | "source_refs"
            )
        })
        || [
            "max_events",
            "max_context_entries",
            "max_entry_bytes",
            "max_context_bytes",
            "max_sources",
            "max_sessions",
        ]
        .iter()
        .any(|name| {
            object
                .get(*name)
                .and_then(serde_json::Value::as_u64)
                .is_none_or(|n| n == 0)
        })
        || !object
            .get("allow_external_sources")
            .is_some_and(serde_json::Value::is_boolean)
    {
        errors.push("checkpoint workspace policy is invalid".into());
    }
}

fn validate_lineage(
    value: Option<&serde_json::Value>,
    workspace_id: Option<&serde_json::Value>,
    errors: &mut Vec<String>,
) {
    let Some(object) = value.and_then(serde_json::Value::as_object) else {
        errors.push("checkpoint lineage must be an object".into());
        return;
    };
    validate_exact_keys(
        object,
        &["kind", "workspace_id", "state_id"],
        "checkpoint lineage",
        errors,
    );
    if object.get("kind").and_then(serde_json::Value::as_str) != Some("workspace")
        || object.get("workspace_id") != workspace_id
        || !object.get("state_id").is_some_and(valid_prefixed_digest)
    {
        errors.push("checkpoint lineage is invalid".into());
    }
}

fn validate_engine_identity(value: Option<&serde_json::Value>, errors: &mut Vec<String>) {
    let Some(object) = value.and_then(serde_json::Value::as_object) else {
        errors.push("checkpoint engine identity must be an object".into());
        return;
    };
    validate_exact_keys(
        object,
        &["interface_version", "schema_version", "transport_version"],
        "checkpoint engine identity",
        errors,
    );
    if object
        .get("interface_version")
        .and_then(serde_json::Value::as_str)
        != Some("1.0.0")
        || object
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
        || object
            .get("transport_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
    {
        errors.push("checkpoint engine identity is unsupported".into());
    }
}

fn validate_source_anchors(sources: &[serde_json::Value], errors: &mut Vec<String>) {
    const KEYS: &[&str] = &[
        "schema_version",
        "source_id",
        "kind",
        "canonical_id",
        "revision",
        "freshness",
        "recovery",
        "trust",
        "scope",
        "engine_binding",
    ];
    let mut source_ids = Vec::new();
    for source in sources {
        let Some(object) = source.as_object() else {
            errors.push("checkpoint source anchor must be an object".into());
            continue;
        };
        validate_exact_keys(object, KEYS, "checkpoint source anchor", errors);
        if object
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
            != Some("leanctx.source-anchor/v1")
        {
            errors.push("checkpoint source anchor schema is unsupported".into());
        }
        let source_id = bounded_string(object.get("source_id"), 128);
        let kind = object.get("kind").and_then(serde_json::Value::as_str);
        if source_id.is_none()
            || !matches!(
                kind,
                Some("filesystem" | "git" | "archive" | "api" | "custom")
            )
            || bounded_string(object.get("canonical_id"), 2048).is_none()
        {
            errors.push("checkpoint source anchor identity is invalid".into());
        }
        if let Some(source_id) = source_id {
            source_ids.push(source_id);
        }
        validate_revision(object.get("revision"), kind, errors);
        validate_freshness(object.get("freshness"), errors);
        validate_recovery(object.get("recovery"), errors);
        validate_trust(object.get("trust"), errors);
        validate_pair_object(
            object.get("scope"),
            "checkpoint source scope",
            64,
            2048,
            errors,
        );
        validate_engine_binding(object.get("engine_binding"), kind, errors);
    }
    if !source_ids.windows(2).all(|pair| pair[0] < pair[1]) {
        errors.push("checkpoint source ids must be unique and sorted".into());
    }
}

fn validate_revision(
    value: Option<&serde_json::Value>,
    source_kind: Option<&str>,
    errors: &mut Vec<String>,
) {
    let Some(value) = value else { return };
    if value.is_null() {
        return;
    }
    let Some(object) = value.as_object() else {
        errors.push("checkpoint source revision must be null or an object".into());
        return;
    };
    validate_exact_keys(
        object,
        &["kind", "value"],
        "checkpoint source revision",
        errors,
    );
    if bounded_string(object.get("kind"), 64).is_none()
        || bounded_string(object.get("value"), 2048).is_none()
        || object.get("kind").and_then(serde_json::Value::as_str) != source_kind
    {
        errors.push("checkpoint source revision is invalid".into());
    }
}

fn validate_freshness(value: Option<&serde_json::Value>, errors: &mut Vec<String>) {
    let Some(object) = value.and_then(serde_json::Value::as_object) else {
        errors.push("checkpoint source freshness must be an object".into());
        return;
    };
    let keys = if object.contains_key("valid_until") {
        &["observed_at", "status", "valid_until"][..]
    } else {
        &["observed_at", "status"][..]
    };
    validate_exact_keys(object, keys, "checkpoint source freshness", errors);
    let observed_raw = object
        .get("observed_at")
        .and_then(serde_json::Value::as_str);
    let observed = observed_raw.and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok());
    let valid_until_raw = object
        .get("valid_until")
        .and_then(serde_json::Value::as_str);
    let valid_until =
        valid_until_raw.and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok());
    if observed_raw.is_none_or(|raw| !canonical_utc_timestamp(raw))
        || !object
            .get("status")
            .is_some_and(|value| matches!(value.as_str(), Some("current" | "stale" | "unknown")))
        || (object.contains_key("valid_until")
            && valid_until_raw.is_none_or(|raw| !canonical_utc_timestamp(raw)))
        || valid_until
            .zip(observed)
            .is_some_and(|(valid, observed)| valid < observed)
    {
        errors.push("checkpoint source freshness is invalid".into());
    }
}

fn validate_recovery(value: Option<&serde_json::Value>, errors: &mut Vec<String>) {
    let Some(value) = value else { return };
    if value.is_null() {
        return;
    }
    let Some(object) = value.as_object() else {
        errors.push("checkpoint source recovery must be null or an object".into());
        return;
    };
    let keys = if object.contains_key("digest") {
        &["kind", "immutable_ref", "digest"][..]
    } else {
        &["kind", "immutable_ref"][..]
    };
    validate_exact_keys(object, keys, "checkpoint source recovery", errors);
    if bounded_string(object.get("kind"), 64).is_none()
        || !object
            .get("immutable_ref")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|raw| valid_ref(raw, 2048))
        || object
            .get("digest")
            .is_some_and(|value| !valid_prefixed_digest(value))
    {
        errors.push("checkpoint source recovery is invalid".into());
    }
}

fn validate_trust(value: Option<&serde_json::Value>, errors: &mut Vec<String>) {
    let Some(object) = value.and_then(serde_json::Value::as_object) else {
        errors.push("checkpoint source trust must be an object".into());
        return;
    };
    validate_exact_keys(
        object,
        &["level", "evidence_refs"],
        "checkpoint source trust",
        errors,
    );
    let level = object.get("level").and_then(serde_json::Value::as_str);
    let Some(refs) = object
        .get("evidence_refs")
        .and_then(serde_json::Value::as_array)
    else {
        errors.push("checkpoint source trust evidence must be an array".into());
        return;
    };
    validate_string_array(
        refs,
        32,
        512,
        true,
        "checkpoint source trust evidence",
        errors,
    );
    if !matches!(level, Some("unverified" | "local" | "verified"))
        || (level == Some("verified")
            && (refs.is_empty()
                || refs.iter().any(|value| {
                    !value.as_str().is_some_and(|raw| {
                        raw.len() == 79
                            && raw.starts_with("receipt:sha256:")
                            && raw[15..]
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    })
                })))
    {
        errors.push("checkpoint source trust is invalid".into());
    }
}

fn validate_pair_object(
    value: Option<&serde_json::Value>,
    label: &str,
    first_cap: usize,
    second_cap: usize,
    errors: &mut Vec<String>,
) {
    let Some(object) = value.and_then(serde_json::Value::as_object) else {
        errors.push(format!("{label} must be an object"));
        return;
    };
    validate_exact_keys(object, &["kind", "value"], label, errors);
    if bounded_string(object.get("kind"), first_cap).is_none()
        || bounded_string(object.get("value"), second_cap).is_none()
    {
        errors.push(format!("{label} is invalid"));
    }
}

fn validate_engine_binding(
    value: Option<&serde_json::Value>,
    source_kind: Option<&str>,
    errors: &mut Vec<String>,
) {
    let Some(value) = value else { return };
    if value.is_null() {
        return;
    }
    let Some(object) = value.as_object() else {
        errors.push("checkpoint engine binding must be null or an object".into());
        return;
    };
    if source_kind != Some("filesystem") {
        errors.push("checkpoint engine binding requires a filesystem source".into());
    }
    let required = ["path", "project_root", "media_type"];
    let allowed = [
        "path",
        "project_root",
        "media_type",
        "source_ref",
        "source_digest",
    ];
    if object.keys().any(|key| !allowed.contains(&key.as_str()))
        || required.iter().any(|key| !object.contains_key(*key))
        || !object
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(valid_relative_source_path)
        || !object
            .get("project_root")
            .and_then(serde_json::Value::as_str)
            .is_some_and(valid_normalized_project_root)
        || bounded_string(object.get("media_type"), 512).is_none()
        || object
            .get("source_ref")
            .is_some_and(|value| !value.as_str().is_some_and(|raw| valid_ref(raw, 512)))
        || object
            .get("source_digest")
            .is_some_and(|value| !valid_prefixed_digest(value))
    {
        errors.push("checkpoint engine binding is invalid".into());
    }
}

fn valid_relative_source_path(raw: &str) -> bool {
    valid_text(raw, 4096)
        && !is_absolute_path(raw)
        && !raw.contains('\\')
        && raw
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

fn valid_normalized_project_root(raw: &str) -> bool {
    if !valid_text(raw, 4096) || !is_absolute_path(raw) {
        return false;
    }
    let tail = if let Some(tail) = raw.strip_prefix("\\\\") {
        tail
    } else if let Some(tail) = raw.strip_prefix('/') {
        tail
    } else if raw.len() >= 3
        && raw.as_bytes()[1] == b':'
        && matches!(raw.as_bytes()[2], b'/' | b'\\')
    {
        &raw[3..]
    } else {
        return false;
    };
    tail.is_empty()
        || tail
            .split(['/', '\\'])
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

fn validate_context_entries(
    entries: &[serde_json::Value],
    source_ids: &std::collections::HashSet<&str>,
    errors: &mut Vec<String>,
) {
    const KEYS: &[&str] = &[
        "schema_version",
        "entry_id",
        "category",
        "value",
        "source_ids",
        "session_id",
        "receipt_refs",
        "recovery_refs",
    ];
    let mut entry_ids = Vec::new();
    for entry in entries {
        let Some(object) = entry.as_object() else {
            errors.push("checkpoint context entry must be an object".into());
            continue;
        };
        validate_exact_keys(object, KEYS, "checkpoint context entry", errors);
        if object
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
            != Some("leanctx.project-context-entry/v1")
            || !object
                .get("entry_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|raw| {
                    uuid::Uuid::parse_str(raw)
                        .is_ok_and(|parsed| parsed.hyphenated().to_string() == raw)
                })
            || !object.get("category").is_some_and(|value| {
                matches!(
                    value.as_str(),
                    Some(
                        "facts"
                            | "decisions"
                            | "constraints"
                            | "unresolved_questions"
                            | "source_refs"
                    )
                )
            })
            || bounded_string(object.get("value"), 4096).is_none()
            || object
                .get("session_id")
                .is_some_and(|value| !value.is_null() && bounded_string(Some(value), 512).is_none())
        {
            errors.push("checkpoint context entry identity/value is invalid".into());
        }
        if let Some(entry_id) = object.get("entry_id").and_then(serde_json::Value::as_str) {
            entry_ids.push(entry_id);
        }
        for (field, cap, item_cap, printable) in [
            ("source_ids", 16, 128, false),
            ("receipt_refs", 16, 512, true),
            ("recovery_refs", 16, 512, true),
        ] {
            let Some(items) = object.get(field).and_then(serde_json::Value::as_array) else {
                errors.push(format!("checkpoint context entry {field} must be an array"));
                continue;
            };
            validate_string_array(
                items,
                cap,
                item_cap,
                printable,
                "checkpoint context entry refs",
                errors,
            );
            if field == "source_ids"
                && items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .any(|source_id| !source_ids.contains(source_id))
            {
                errors.push("checkpoint context entry references an unknown source".into());
            }
        }
    }
    if !entry_ids.windows(2).all(|pair| pair[0] < pair[1]) {
        errors.push("checkpoint context entry ids must be unique and sorted".into());
    }
}

fn validate_string_array(
    values: &[serde_json::Value],
    cap: usize,
    item_cap: usize,
    printable_ascii: bool,
    label: &str,
    errors: &mut Vec<String>,
) {
    if values.len() > cap
        || values.iter().any(|value| {
            bounded_string(Some(value), item_cap).is_none()
                || (printable_ascii
                    && !value
                        .as_str()
                        .is_some_and(|raw| raw.bytes().all(|byte| (0x20..=0x7e).contains(&byte))))
        })
        || !values
            .windows(2)
            .all(|pair| pair[0].as_str() < pair[1].as_str())
    {
        errors.push(format!("{label} is invalid or exceeds its bound"));
    }
}

fn valid_ref(raw: &str, cap: usize) -> bool {
    !raw.is_empty() && raw.len() <= cap && raw.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

fn canonical_utc_timestamp(raw: &str) -> bool {
    (raw.len() == 20 || raw.len() == 27)
        && raw.ends_with('Z')
        && (raw.len() == 20 || raw.as_bytes().get(19) == Some(&b'.'))
        && chrono::DateTime::parse_from_rfc3339(raw).is_ok()
}

fn bounded_string(value: Option<&serde_json::Value>, cap: usize) -> Option<&str> {
    value
        .and_then(serde_json::Value::as_str)
        .filter(|raw| valid_text(raw, cap))
}

fn valid_text(raw: &str, cap: usize) -> bool {
    !raw.is_empty() && raw.len() <= cap && !raw.chars().any(char::is_control)
}

fn validate_package_pins(
    pins: &[serde_json::Value],
    lock_digest: Option<&serde_json::Value>,
    errors: &mut Vec<String>,
) {
    const KEYS: &[&str] = &[
        "schema_version",
        "name",
        "version",
        "artifact_digest",
        "manifest_digest",
        "content_hash",
        "signature_state",
        "signer_public_key",
        "trust_state",
        "policy_decision",
    ];
    let mut identities = Vec::new();
    for pin in pins {
        let Some(object) = pin.as_object() else {
            errors.push("checkpoint package pin must be an object".into());
            continue;
        };
        validate_exact_keys(object, KEYS, "checkpoint package pin", errors);
        if object
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
            != Some("leanctx.package-pin/v1")
            || object
                .get("policy_decision")
                .and_then(serde_json::Value::as_str)
                != Some("admitted")
        {
            errors.push("checkpoint package pin contract is unsupported".into());
        }
        for name in ["artifact_digest", "manifest_digest", "content_hash"] {
            if !object.get(name).is_some_and(valid_prefixed_digest) {
                errors.push(format!("checkpoint package pin {name} is invalid"));
            }
        }
        let signature = object
            .get("signature_state")
            .and_then(serde_json::Value::as_str);
        let signer = object.get("signer_public_key");
        if !matches!(signature, Some("signed_valid" | "unsigned"))
            || (signature == Some("signed_valid")
                && !signer.is_some_and(|value| {
                    value.as_str().is_some_and(|raw| {
                        raw.len() == 64
                            && raw
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    })
                }))
            || (signature == Some("unsigned") && !signer.is_some_and(serde_json::Value::is_null))
        {
            errors.push("checkpoint package pin signature identity is invalid".into());
        }
        if !object.get("trust_state").is_some_and(|value| {
            matches!(value.as_str(), Some("trusted" | "untrusted" | "unknown"))
        }) {
            errors.push("checkpoint package pin trust state is invalid".into());
        }
        let Some(name) = object.get("name").and_then(serde_json::Value::as_str) else {
            errors.push("checkpoint package pin name is invalid".into());
            continue;
        };
        let Some(version) = object.get("version").and_then(serde_json::Value::as_str) else {
            errors.push("checkpoint package pin version is invalid".into());
            continue;
        };
        if !valid_text(name, 128) || !valid_text(version, 64) {
            errors.push("checkpoint package pin name/version exceeds bounds".into());
        }
        identities.push((name, version));
    }
    if !identities.windows(2).all(|pair| pair[0] < pair[1]) {
        errors.push("checkpoint package pins must be unique and sorted".into());
    }
    if pins.is_empty() {
        if !lock_digest.is_some_and(serde_json::Value::is_null) {
            errors.push("empty checkpoint package pins require a null lock digest".into());
        }
    } else {
        validate_domain_digest(
            "leanctx.package.lock.v1",
            &serde_json::Value::Array(pins.to_vec()),
            lock_digest,
            "checkpoint.package_lock_digest",
            errors,
        );
    }
}

fn validate_migration_provenance(
    value: Option<&serde_json::Value>,
    checkpoint: &serde_json::Value,
    errors: &mut Vec<String>,
) {
    let Some(value) = value else { return };
    const KEYS: &[&str] = &[
        "origin",
        "legacy_snapshot_id",
        "legacy_snapshot_digest",
        "migration_contract",
        "checkpoint_id",
        "state_digest",
        "limitations",
    ];
    let Some(object) = value.as_object() else {
        errors.push("migration_provenance must be an object".into());
        return;
    };
    validate_exact_keys(object, KEYS, "migration_provenance", errors);
    if object.get("origin").and_then(serde_json::Value::as_str) != Some("SnapshotV1")
        || object
            .get("migration_contract")
            .and_then(serde_json::Value::as_str)
            != Some("leanctx.snapshot-v1-migration/v1")
    {
        errors.push("SnapshotV1 migration provenance is unsupported".into());
    }
    for name in ["legacy_snapshot_digest", "state_digest"] {
        if !object.get(name).is_some_and(valid_prefixed_digest) {
            errors.push(format!("migration_provenance.{name} is invalid"));
        }
    }
    if bounded_string(object.get("legacy_snapshot_id"), 512).is_none()
        || !object
            .get("limitations")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| {
                items.len() <= 64
                    && items
                        .iter()
                        .all(|item| bounded_string(Some(item), 2048).is_some())
            })
    {
        errors.push("migration_provenance.limitations exceeds its bound".into());
    }
    if object.get("checkpoint_id") != checkpoint.get("checkpoint_id")
        || object.get("state_digest") != checkpoint.get("state_digest")
    {
        errors.push("migration provenance is not bound to the carried checkpoint".into());
    }
}

fn validate_exact_keys(
    object: &serde_json::Map<String, serde_json::Value>,
    expected: &[&str],
    label: &str,
    errors: &mut Vec<String>,
) {
    if object.len() != expected.len()
        || object.keys().any(|key| !expected.contains(&key.as_str()))
        || expected.iter().any(|key| !object.contains_key(*key))
    {
        errors.push(format!("{label} fields do not match the open contract"));
    }
}

fn valid_prefixed_digest(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(|raw| {
        raw.len() == 71
            && raw.starts_with("sha256:")
            && raw[7..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn validate_domain_digest(
    domain: &str,
    value: &serde_json::Value,
    claimed: Option<&serde_json::Value>,
    label: &str,
    errors: &mut Vec<String>,
) {
    let Ok(canonical) = serde_json::to_vec(value) else {
        errors.push(format!("{label} input is not canonical JSON"));
        return;
    };
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update(b"\n");
    hasher.update(canonical);
    let expected = format!(
        "sha256:{}",
        crate::core::agent_identity::hex_encode(&hasher.finalize())
    );
    if claimed.and_then(serde_json::Value::as_str) != Some(expected.as_str()) {
        errors.push(format!("{label} does not match canonical content"));
    }
}

fn collect_non_portable_paths(value: &serde_json::Value, pointer: &str, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, item) in object {
                let child = format!("{pointer}.{key}");
                if matches!(key.as_str(), "path" | "project_root")
                    && item.as_str().is_some_and(is_absolute_path)
                {
                    out.push(child.clone());
                }
                if matches!(key.as_str(), "canonical_id" | "immutable_ref")
                    && item.as_str().is_some_and(|value| {
                        value.starts_with("file:///") || is_absolute_path(value)
                    })
                {
                    out.push(child.clone());
                }
                collect_non_portable_paths(item, &child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_non_portable_paths(item, &format!("{pointer}[{index}]"), out);
            }
        }
        _ => {}
    }
}

fn is_absolute_path(value: &str) -> bool {
    value.starts_with('/')
        || value.starts_with("\\\\")
        || (value.len() > 2
            && value.as_bytes()[1] == b':'
            && matches!(value.as_bytes()[2], b'/' | b'\\'))
}

/// Structural + integrity validation of a `kind=skills` payload (GH #727).
/// Every blob must decode and match its plaintext hash — a tampered body
/// fails verification, so it can never be materialized on disk.
fn validate_documents(docs: &super::content::DocumentsContent, errors: &mut Vec<String>) {
    use super::content::{MAX_DOCUMENT_FILES, MAX_DOCUMENTS_TOTAL_BYTES};

    if docs.files.is_empty() {
        errors.push("kind=skills payload has no files".into());
        return;
    }
    if docs.files.len() > MAX_DOCUMENT_FILES {
        errors.push(format!(
            "skills payload has {} files (cap: {MAX_DOCUMENT_FILES})",
            docs.files.len()
        ));
        return;
    }

    let mut seen = std::collections::HashSet::new();
    let mut total: usize = 0;
    for blob in &docs.files {
        if let Err(e) = validate_document_path(&blob.path) {
            errors.push(e);
            continue;
        }
        if !seen.insert(blob.path.as_str()) {
            errors.push(format!("duplicate document path `{}`", blob.path));
            continue;
        }
        if blob.sha256.len() != 64 || !blob.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            errors.push(format!(
                "`{}`: sha256 must be a 64-char hex string",
                blob.path
            ));
            continue;
        }
        match blob.decode_verified() {
            Ok(plain) => total += plain.len(),
            Err(e) => errors.push(e),
        }
    }
    if total > MAX_DOCUMENTS_TOTAL_BYTES {
        errors.push(format!(
            "skills payload decodes to {total} bytes (cap: {MAX_DOCUMENTS_TOTAL_BYTES})"
        ));
    }
}

/// Path safety for document blobs: relative, `/`-separated, no traversal, no
/// absolute/drive/backslash forms — the materializer joins these under the
/// pack store and must never be able to escape it.
pub(crate) fn validate_document_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > 512 {
        return Err(format!(
            "invalid document path `{path}` (empty or too long)"
        ));
    }
    if path.starts_with('/') || path.contains('\\') || path.contains(':') {
        return Err(format!(
            "invalid document path `{path}` (must be relative with `/` separators)"
        ));
    }
    let has_bad_component = path
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == ".." || c.starts_with(".."));
    if has_bad_component || path.chars().any(char::is_control) {
        return Err(format!("invalid document path `{path}` (unsafe component)"));
    }
    Ok(())
}

/// Outcome of one verification check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckOutcome {
    Pass,
    Fail,
    /// Not applicable — e.g. signature check on an unsigned package.
    Skipped,
}

impl CheckOutcome {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }
}

/// Per-check verification report, mirroring the checks every conforming
/// reader runs (and the shape of the @ctxpkg/verify reference output).
#[derive(Debug)]
pub(crate) struct VerifyReport {
    pub name: Option<String>,
    pub version: Option<String>,
    pub structure: CheckOutcome,
    pub content_hash: CheckOutcome,
    pub package_hash: CheckOutcome,
    pub signature: CheckOutcome,
    pub errors: Vec<String>,
}

impl VerifyReport {
    pub(crate) fn valid(&self) -> bool {
        self.errors.is_empty()
    }

    fn failed(error: String) -> Self {
        Self {
            name: None,
            version: None,
            structure: CheckOutcome::Fail,
            content_hash: CheckOutcome::Skipped,
            package_hash: CheckOutcome::Skipped,
            signature: CheckOutcome::Skipped,
            errors: vec![error],
        }
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    crate::core::agent_identity::hex_encode(&h.finalize())
}

/// Verify a `.ctxpkg` document without installing anything.
pub(crate) fn verify_package_text(doc: &str) -> VerifyReport {
    let value: serde_json::Value = match serde_json::from_str(doc) {
        Ok(v) => v,
        Err(e) => return VerifyReport::failed(format!("not valid JSON: {e}")),
    };

    let Some(manifest_value) = value.get("manifest") else {
        return VerifyReport::failed("missing required member: manifest".into());
    };
    if value.get("content").is_none() {
        return VerifyReport::failed("missing required member: content".into());
    }

    let manifest: PackageManifest = match serde_json::from_value(manifest_value.clone()) {
        Ok(m) => m,
        Err(e) => return VerifyReport::failed(format!("manifest does not parse: {e}")),
    };
    let mut report = VerifyReport {
        name: Some(manifest.name.clone()),
        version: Some(manifest.version.clone()),
        structure: CheckOutcome::Pass,
        content_hash: CheckOutcome::Skipped,
        package_hash: CheckOutcome::Skipped,
        signature: CheckOutcome::Skipped,
        errors: Vec::new(),
    };
    if let Err(errs) = manifest.validate() {
        report.structure = CheckOutcome::Fail;
        report.errors.extend(errs);
        return report;
    }

    // Kind ↔ payload coherence (GH #726) — a structural property: the
    // declared kind must match the payload the document actually carries.
    let content = match serde_json::from_value::<PackageContent>(
        value.get("content").cloned().unwrap_or_default(),
    ) {
        Ok(content) => content,
        Err(error) => {
            report.structure = CheckOutcome::Fail;
            report
                .errors
                .push(format!("content does not parse: {error}"));
            return report;
        }
    };
    if let Err(errs) = validate_kind_coherence(&manifest, &content) {
        report.structure = CheckOutcome::Fail;
        report.errors.extend(errs);
        return report;
    }

    // §8 — integrity against the writer's bytes.
    let Some(content_text) = extract_top_level_value_text(doc, "content") else {
        report.structure = CheckOutcome::Fail;
        report
            .errors
            .push("could not locate the content member in the document".into());
        return report;
    };
    let canonical = compact_json_text(content_text);
    let actual_content_hash = sha256_hex(canonical.as_bytes());

    if actual_content_hash == manifest.integrity.content_hash {
        report.content_hash = CheckOutcome::Pass;
    } else {
        report.content_hash = CheckOutcome::Fail;
        report.errors.push(format!(
            "content_hash mismatch: manifest says {}, content hashes to {actual_content_hash}",
            manifest.integrity.content_hash
        ));
    }
    if manifest.integrity.byte_size != canonical.len() as u64 {
        report.content_hash = CheckOutcome::Fail;
        report.errors.push(format!(
            "byte_size mismatch: manifest says {}, content is {} bytes",
            manifest.integrity.byte_size,
            canonical.len()
        ));
    }

    let expected_sha = sha256_hex(
        format!(
            "{}:{}:{actual_content_hash}",
            manifest.name, manifest.version
        )
        .as_bytes(),
    );
    if expected_sha == manifest.integrity.sha256 {
        report.package_hash = CheckOutcome::Pass;
    } else {
        report.package_hash = CheckOutcome::Fail;
        report.errors.push(format!(
            "package sha256 mismatch: manifest says {}, recomputed {expected_sha}",
            manifest.integrity.sha256
        ));
    }

    // §9 — a present-but-invalid signature is always tampering.
    if manifest.signature.is_some() {
        match super::signing::verify_signature(&manifest) {
            Ok(true) => report.signature = CheckOutcome::Pass,
            Ok(false) => {
                report.signature = CheckOutcome::Fail;
                report.errors.push(
                    "signature verification failed — the package was modified after signing".into(),
                );
            }
            Err(e) => {
                report.signature = CheckOutcome::Fail;
                report.errors.push(format!("signature check errored: {e}"));
            }
        }
    }

    report
}

/// Read and verify a `.ctxpkg` file (size- and extension-gated like import).
pub(crate) fn verify_package_file(path: &Path) -> Result<VerifyReport, String> {
    if !crate::core::contracts::is_package_file(path) {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("(none)");
        return Err(format!(
            "unsupported file extension '.{ext}' — expected .{} or .{}",
            crate::core::contracts::PACKAGE_EXTENSION,
            crate::core::contracts::LEGACY_PACKAGE_EXTENSION,
        ));
    }
    let meta = std::fs::metadata(path).map_err(|e| format!("stat package file: {e}"))?;
    if meta.len() > crate::core::contracts::MAX_PACKAGE_FILE_BYTES {
        return Err(format!(
            "package file too large ({} bytes, max {} bytes)",
            meta.len(),
            crate::core::contracts::MAX_PACKAGE_FILE_BYTES,
        ));
    }
    let doc = std::fs::read_to_string(path).map_err(|e| format!("read package file: {e}"))?;
    Ok(verify_package_text(&doc))
}

#[cfg(test)]
mod tests;
