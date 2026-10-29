// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn procedure_reclaim_rechecks_and_scrubs_existing_store_content() {
    use crate::core::procedural_memory::{Procedure, ProcedureStep};
    use chrono::Utc;

    let _data = crate::core::data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("project root");
    let policy_dir = root.path().join(".lean-ctx");
    std::fs::create_dir_all(&policy_dir).expect("policy directory");
    std::fs::write(
        policy_dir.join("policy.toml"),
        "name = \"memory-admission-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n[redaction]\ncustomer = 'SECRET-[0-9]+'\n",
    )
    .expect("redaction policy");
    let root = root.path().to_string_lossy().into_owned();
    let hash = crate::core::project_hash::hash_project_root(&root);

    let mut store = ProceduralStore::new(&hash);
    store.procedures.push(Procedure {
        id: "procedure-1".into(),
        name: "fixture workflow".into(),
        description: "uses SECRET-123".into(),
        steps: vec![ProcedureStep {
            tool: "ctx_read".into(),
            description: "inspect SECRET-123".into(),
            optional: false,
        }],
        activation_keywords: vec!["fixture".into()],
        confidence: 0.8,
        times_used: 1,
        times_succeeded: 1,
        last_used: Utc::now(),
        project_specific: true,
        created_at: Utc::now(),
    });
    store.save().expect("save raw procedure fixture");

    let mut opts = ConsolidateOptions::manual();
    opts.import_session = false;
    opts.run_lifecycle = false;
    consolidate_project_knowledge_with(&root, &opts).expect("consolidate");

    let stored = ProceduralStore::load(&hash).expect("procedural store remains");
    assert_eq!(stored.procedures.len(), 1);
    assert_eq!(stored.procedures[0].description, "uses [REDACTED:customer]");
    assert_eq!(
        stored.procedures[0].steps[0].description,
        "inspect [REDACTED:customer]"
    );
}
