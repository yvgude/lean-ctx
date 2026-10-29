// SPDX-License-Identifier: Apache-2.0
//! Current rights of derived copies: the lineage decides, not the stored text.

use super::*;
use crate::core::context_admission::carrier::NO_SOURCE_AUTHORITY;

fn isolated(config: &str) -> crate::core::data_dir::IsolatedDataDir {
    let data = crate::core::data_dir::isolated_data_dir();
    let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.toml"), config).expect("config");
    data
}

/// A fresh load from disk: the binding as another process would read it.
fn reloaded(binding: &StoredBinding) -> StoredBinding {
    serde_json::from_str(&serde_json::to_string(binding).expect("serialize")).expect("load")
}

/// No project pack, global gateway governed. A neutral-looking excerpt (no
/// marking, no secret pattern) derived from sources A and B must be withheld
/// once A is reclassified, deleted, or read from another project — although
/// nothing in the stored text changes and B stays allowed. Re-detecting the
/// stored text alone (the previous design) serves it in every one of these
/// cases.
#[test]
fn a_neutral_excerpt_is_withheld_when_one_of_its_sources_loses_its_rights() {
    let _data = isolated("[context_gateway]\nmode = \"governed\"\n");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    assert!(
        source_authority_required(),
        "governed without a policy pack"
    );
    let project = tempfile::tempdir().expect("project");
    let root = crate::core::pathutil::safe_canonicalize(project.path()).expect("canonical root");
    let root = root.to_str().expect("utf-8 root");
    let path_a = format!("{root}/a.rs");
    let path_b = format!("{root}/b.rs");
    let original_a = "pub fn alpha() {}\n";
    std::fs::write(&path_a, original_a).expect("source a");
    std::fs::write(&path_b, "pub fn beta() {}\n").expect("source b");

    let (_, a) = SourceRef::read(&path_a, root, "ctx_read").expect("a is readable");
    let (_, b) = SourceRef::read(&path_b, root, "ctx_read").expect("b is readable");
    let excerpt = "alpha and beta are exported";
    let (stored, both) = admit_and_bind(
        excerpt,
        "test",
        Some(root),
        Lineage::of(vec![a, b.clone(), b.clone()]).expect("bounded lineage"),
    )
    .expect("permitted content is stored");
    assert_eq!(stored, excerpt, "allowed content keeps its bytes");
    let single = "beta is exported";
    let (_, only_b) = admit_and_bind(single, "test", Some(root), Lineage::of(vec![b]).unwrap())
        .expect("single-source copy");
    let copies = [both.clone(), reloaded(&both)];
    let serve = |binding: &StoredBinding| serve_stored(excerpt, Some(binding), Some(root));
    let b_alone = || serve_stored(single, Some(&only_b), Some(root));
    for binding in &copies {
        assert_eq!(serve(binding).as_deref(), Ok(excerpt));
    }

    // (ii) A is reclassified after the write.
    std::fs::write(&path_a, format!("*** TOP SECRET ***\n{original_a}")).expect("mark");
    for binding in &copies {
        let withheld = serve(binding).expect_err("a restricted source authorizes no copy");
        assert!(withheld.has("store.restricted_content"), "{withheld}");
    }
    assert_eq!(b_alone().as_deref(), Ok(single), "B alone stays readable");

    // Control: the unchanged original is authorized again, byte-identically.
    std::fs::write(&path_a, original_a).expect("restore");
    assert_eq!(serve(&copies[1]).as_deref(), Ok(excerpt));

    // (iv) another project, and a reader whose project is unresolved.
    let elsewhere = tempfile::tempdir().expect("other project");
    let withheld = serve_stored(excerpt, Some(&both), elsewhere.path().to_str())
        .expect_err("another project has no authority over this copy");
    assert!(withheld.has("store.project_mismatch"), "{withheld}");
    let withheld = serve_stored(excerpt, Some(&both), None).expect_err("unresolved project");
    assert!(withheld.has("store.project_unresolved"), "{withheld}");

    // (iii) A is deleted.
    std::fs::remove_file(&path_a).expect("delete");
    for binding in &copies {
        let withheld = serve(binding).expect_err("one remaining source is not enough");
        assert!(withheld.has("store.source_unavailable"), "{withheld}");
        assert!(
            withheld
                .message("stored content")
                .contains(NO_SOURCE_AUTHORITY)
        );
    }
    assert_eq!(b_alone().as_deref(), Ok(single), "B alone stays readable");

    // A record written before bindings existed: the originless rule.
    let withheld = serve_stored(excerpt, None, Some(root)).expect_err("legacy record");
    assert!(withheld.has("store.no_source_authority"), "{withheld}");
}

/// Community developer mode keeps today's behaviour for records without a
/// binding, and a caller-supplied item is governed by content rules only.
#[test]
fn community_developer_mode_serves_legacy_and_caller_supplied_copies() {
    let _data = isolated("");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    assert!(!source_authority_required());
    let note = "remember to rotate the build cache";
    assert_eq!(serve_stored(note, None, None).as_deref(), Ok(note));
    let (stored, binding) =
        admit_and_bind(note, "agent:a", None, Lineage::CallerSupplied).expect("stored");
    assert!(
        !binding.source_bound(),
        "caller-supplied is not source-bound"
    );
    assert_eq!(
        serve_stored(&stored, Some(&binding), None).as_deref(),
        Ok(note)
    );
    let withheld = serve_stored("another text", Some(&binding), None).expect_err("edited record");
    assert!(withheld.has("store.binding_mismatch"), "{withheld}");
}
