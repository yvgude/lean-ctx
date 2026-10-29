// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::core::knowledge::{FactOrigin, ProjectKnowledge};
use crate::core::memory_policy::MemoryPolicy;
use crate::core::providers::config_provider::{ConfigProvider, schema::ProviderConfig};
use crate::core::providers::hardened_http::redirect_tests::Server;

#[test]
fn source_view_reacquires_after_revoke_and_reload_and_rejects_unrelated_value() {
    exercise_reuse(Projection::Raw);
}

#[test]
fn source_view_reacquires_the_redacted_snapshot_projection() {
    exercise_reuse(Projection::SnapshotV1);
}

/// Reacquisition checks `roles::active_role()`, which other tests change
/// process-wide (`set_active_role("reviewer")` denies `ctx_provider`); a
/// concurrent switch withheld every provider fact (Linux CI, #2036). Pin the
/// role for this test the way the compose tests do.
fn exercise_reuse(projection: Projection) {
    let role = crate::core::roles::load_role("coder").unwrap();
    crate::core::roles::with_test_active_role(role, || exercise_reuse_as_coder(projection));
}

fn exercise_reuse_as_coder(projection: Projection) {
    let _data = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().unwrap();
    // Project-local providers load only for a trusted workspace.
    crate::core::workspace_trust::trust(root.path()).unwrap();
    let project = root.path().to_str().unwrap();
    let configs = root.path().join(".lean-ctx/providers");
    std::fs::create_dir_all(&configs).unwrap();
    std::fs::write(
        root.path().join(".lean-ctx/policy.toml"),
        "name='source-proof'\nversion='1.0.0'\ndescription='fixture'\n",
    )
    .unwrap();
    let title = if projection == Projection::SnapshotV1 {
        format!(
            "Orchidquartz token={}",
            ["synthetic", "-secret-value"].concat()
        )
    } else {
        "Orchidquartz login revocation canary".to_owned()
    };
    let body = serde_json::json!([{"id":"1", "title":title, "body":"Investigate src/auth.rs"}])
        .to_string();
    let server = Server::new(200, None, &body);
    let config_text = format!(
        "id='knowledge-origin-fixture'\nname='Fixture'\nbase_url='{}'\n[auth]\ntype='none'\n[resources.issues]\nmethod='GET'\npath='/items'\n[resources.issues.response.mapping]\nid='id'\ntitle='title'\nbody='body'\n",
        server.url
    );
    std::fs::write(configs.join("fixture.toml"), &config_text).unwrap();
    let config: ProviderConfig = toml::from_str(&config_text).unwrap();
    let provider = ConfigProvider::from_config(config).unwrap();
    let acquired = BoundResult::execute(&provider, "issues", &ProviderParams::default()).unwrap();
    let mut indexed = acquired.result.clone();
    indexed.items = indexed
        .items
        .iter()
        .map(|item| projection.item(item).unwrap())
        .collect();
    let facts = crate::core::knowledge_provider_extract::extract_facts(
        &acquired.chunks_with_projection(&indexed, projection),
    );
    if projection == Projection::SnapshotV1 {
        assert!(
            facts
                .iter()
                .all(|fact| !fact.value.contains("synthetic-secret-value"))
        );
        assert!(facts.iter().any(|fact| fact.value.contains("[REDACTED")));
    }
    assert!(!facts.is_empty());
    assert!(
        facts
            .iter()
            .all(|fact| matches!(fact.origin, FactOrigin::Provider(_)))
    );
    let mut knowledge = ProjectKnowledge::new(project);
    for fact in facts {
        knowledge.remember_with_origin(
            &fact.category,
            &fact.key,
            &fact.value,
            "ingest",
            fact.confidence,
            &MemoryPolicy::default(),
            fact.origin,
        );
    }
    knowledge.save().unwrap();
    let before = server.count();
    assert_eq!(
        ProjectKnowledge::load(project).unwrap().facts.len(),
        knowledge.facts.len()
    );
    assert_eq!(
        server.count() - before,
        1,
        "one live resource check for all facts from this item"
    );
    server.respond(403, "denied");
    assert!(ProjectKnowledge::load(project).unwrap().facts.is_empty());
    // A newly deserialized store cannot reuse the prior authorization.
    let stored = ProjectKnowledge::load_for_checked_capture(project).unwrap();
    let reloaded: ProjectKnowledge =
        serde_json::from_str(&serde_json::to_string(&stored).unwrap()).unwrap();
    assert!(reloaded.admit_sources().facts.is_empty());
    ProjectKnowledge::mutate_locked(project, |view| {
        view.remember(
            "decision",
            "local",
            "use a bounded parser",
            "local",
            0.9,
            &MemoryPolicy::default(),
        );
    })
    .unwrap();
    server.respond(200, &body);
    assert_eq!(
        ProjectKnowledge::load(project).unwrap().facts.len(),
        knowledge.facts.len() + 1
    );
    let mut forged = knowledge.clone();
    forged.facts[0].value = "unrelated confidential material".into();
    assert!(
        !forged
            .admit_sources()
            .facts
            .iter()
            .any(|f| f.value == "unrelated confidential material")
    );
    let mut origin_changed = knowledge.clone();
    if let FactOrigin::Provider(origin) = &mut origin_changed.facts[0].origin {
        origin.binding = "0".repeat(64);
    }
    assert!(
        !origin_changed
            .admit_sources()
            .facts
            .iter()
            .any(|f| f.key == knowledge.facts[0].key)
    );
    assert!(
        super::super::registry::global_registry()
            .get("knowledge-origin-fixture")
            .is_none(),
        "reuse must not mutate global provider registry"
    );
}

#[test]
fn source_view_reuse_deadline_is_nested_and_expires_closed() {
    assert!(reuse_time_remaining().is_none());
    REUSE_DEADLINE.sync_scope(std::time::Instant::now(), || {
        with_reuse_deadline(|| assert!(reuse_time_remaining().unwrap().is_zero()));
    });
    assert!(reuse_time_remaining().is_none());
}

#[test]
fn source_view_nested_checks_share_the_same_budget_and_next_operation_is_fresh() {
    with_reuse_deadline(|| {
        for _ in 0..32 {
            with_reuse_deadline(|| reserve_reuse_check().unwrap());
        }
        assert!(with_reuse_deadline(reserve_reuse_check).is_err());
    });
    assert!(reserve_reuse_check().is_err());
    with_reuse_deadline(|| reserve_reuse_check().unwrap());
}

/// Doc 19: a provider re-check answers in the source-validity vocabulary —
/// the same item again is valid, an edited one stale, one that left the result
/// deleted, an access refusal revoked, a hanging source unavailable — and the
/// reason code says which, never the bare "not reusable" it was before.
#[test]
fn a_provider_recheck_names_the_source_validity() {
    let role = crate::core::roles::load_role("coder").unwrap();
    crate::core::roles::with_test_active_role(role, || {
        use lean_ctx_protocol::context_lineage::SourceValidityV1 as V;
        let _data = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().unwrap();
        let project = root.path().to_str().unwrap();
        // Project-local providers load only for a trusted workspace.
        crate::core::workspace_trust::trust(root.path()).unwrap();
        let configs = root.path().join(".lean-ctx/providers");
        std::fs::create_dir_all(&configs).unwrap();
        let item = |title: &str, id: &str| {
            serde_json::json!([{"id": id, "title": title, "body": "Investigate src/auth.rs"}])
                .to_string()
        };
        let server = Server::new(200, None, &item("Validity canary", "1"));
        let config_text = format!(
            "id='validity-fixture'\nname='Fixture'\nbase_url='{}'\n[auth]\ntype='none'\n[resources.issues]\nmethod='GET'\npath='/items'\n[resources.issues.response.mapping]\nid='id'\ntitle='title'\nbody='body'\n",
            server.url
        );
        std::fs::write(configs.join("fixture.toml"), &config_text).unwrap();
        let config: ProviderConfig = toml::from_str(&config_text).unwrap();
        let provider = ConfigProvider::from_config(config).unwrap();
        let acquired =
            BoundResult::execute(&provider, "issues", &ProviderParams::default()).unwrap();
        let origin = acquired.chunks(&acquired.result)[0]
            .origin
            .clone()
            .expect("acquired chunk carries its origin");
        let recheck = || {
            crate::core::policy::runtime::with_project_source_view(project, || {
                with_reuse_deadline(|| origin.current_chunks(project).map(|_| ()))
            })
            .expect("source view")
            .map_err(|refusal| refusal.validity)
        };
        assert_eq!(recheck(), Ok(()), "the same item: valid");
        server.respond(200, &item("Validity canary, edited", "1"));
        assert_eq!(recheck(), Err(V::Stale));
        server.respond(200, &item("Another item", "2"));
        assert_eq!(recheck(), Err(V::Deleted));
        server.respond(403, "denied");
        assert_eq!(recheck(), Err(V::Revoked));
        server.respond(200, &item("Validity canary", "1"));
        assert_eq!(recheck(), Ok(()), "access restored: valid again");
    });
}
