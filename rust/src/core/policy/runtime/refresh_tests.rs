// SPDX-License-Identifier: Apache-2.0
use super::{refresh::RefreshCache, *};
use crate::core::policy::{files, org};

const BASE: &str = "name = \"refresh\"\nversion = \"1.0.0\"\ndescription = \"test\"\n";

fn project() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(PROJECT_PACK_PATH);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    (root, path)
}

#[test]
fn exact_bytes_reuse_compilation_and_changed_bytes_revoke_without_reload() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let (root, path) = project();
    let mut cache = RefreshCache::default();
    assert!(cache.refresh(root.path()).unwrap().is_none());
    std::fs::write(
        &path,
        format!("{BASE}[redaction]\ncustomer = 'CUS-[0-9]{{4}}'\n"),
    )
    .unwrap();
    let first = cache.refresh(root.path()).unwrap().unwrap();
    let unchanged = cache.refresh(root.path()).unwrap().unwrap();
    assert!(Arc::ptr_eq(&first, &unchanged));
    let previous = std::fs::metadata(&path).unwrap();
    // Same length and restored timestamp: metadata is not an authorization key.
    std::fs::write(
        &path,
        format!("{BASE}[redaction]\ncustomer = 'ACC-[0-9]{{4}}'\n"),
    )
    .unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(previous.modified().unwrap()))
        .unwrap();
    let changed = cache.refresh(root.path()).unwrap().unwrap();
    assert!(!Arc::ptr_eq(&first, &changed));
    assert!(!first.redaction[0].1.is_match("ACC-1234"));
    assert!(changed.redaction[0].1.is_match("ACC-1234"));
    std::fs::write(
        &path,
        format!("{BASE}[context]\ndeny_tools = ['ctx_read']\n"),
    )
    .unwrap();
    assert!(
        !cache
            .refresh(root.path())
            .unwrap()
            .unwrap()
            .tool_allowed("ctx_read")
    );
    std::fs::write(&path, "invalid").unwrap();
    assert!(cache.refresh(root.path()).is_err());
    std::fs::write(&path, BASE).unwrap();
    assert!(
        cache
            .refresh(root.path())
            .unwrap()
            .unwrap()
            .tool_allowed("ctx_read")
    );
}

#[test]
fn current_signed_organization_and_trust_are_rechecked() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let (root, _) = project();
    let key = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
    let mut artifact = org::OrgPolicyV1::build("fixture", "1", true, BASE).unwrap();
    artifact.sign_with_key(&key);
    org::trust::pin("fixture", artifact.signer_public_key.as_deref().unwrap()).unwrap();
    let installed = org::store::install(&artifact).unwrap();
    let mut cache = RefreshCache::default();
    assert!(
        cache
            .refresh(root.path())
            .unwrap()
            .unwrap()
            .tool_allowed("ctx_read")
    );
    artifact.pack_toml = format!("{BASE}[context]\ndeny_tools = ['ctx_read']\n");
    // Changed content with the previous signature cannot use the cached authority.
    org::store::install(&artifact).unwrap();
    assert!(cache.refresh(root.path()).is_err());
    artifact.sign_with_key(&key);
    org::store::install(&artifact).unwrap();
    assert!(
        !cache
            .refresh(root.path())
            .unwrap()
            .unwrap()
            .tool_allowed("ctx_read")
    );
    org::trust::remove(artifact.signer_public_key.as_deref().unwrap()).unwrap();
    assert!(cache.refresh(root.path()).is_err());
    org::trust::pin("fixture", artifact.signer_public_key.as_deref().unwrap()).unwrap();
    std::fs::remove_file(&installed).unwrap();
    assert!(
        cache.refresh(root.path()).is_err(),
        "pinned organization cannot silently disappear"
    );
    artifact.enforced = false;
    artifact.sign_with_key(&key);
    org::store::install(&artifact).unwrap();
    assert!(cache.refresh(root.path()).unwrap().is_none());
}

#[test]
fn bounded_inputs_and_project_separation_cannot_reuse_prior_policy() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let (root, path) = project();
    std::fs::write(
        &path,
        format!("{BASE}[context]\ndeny_tools = ['ctx_read']\n"),
    )
    .unwrap();
    let child = root.path().join("nested");
    std::fs::create_dir(&child).unwrap();
    let other = tempfile::tempdir().unwrap();
    let mut cache = RefreshCache::default();
    assert!(
        !cache
            .refresh(&child)
            .unwrap()
            .unwrap()
            .tool_allowed("ctx_read")
    );
    assert!(cache.refresh(other.path()).unwrap().is_none());
    std::fs::File::create(&path)
        .unwrap()
        .set_len(files::MAX_BYTES + 1)
        .unwrap();
    assert!(cache.refresh(root.path()).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(cache.refresh(root.path()).is_err());
    #[cfg(unix)]
    {
        std::fs::remove_dir(&path).unwrap();
        let target = root.path().join("policy-target");
        std::fs::write(&target, BASE).unwrap();
        std::os::unix::fs::symlink(target, &path).unwrap();
        assert!(cache.refresh(root.path()).is_err());
    }
}

#[tokio::test]
async fn request_project_scope_survives_await_and_cannot_leak_to_another_request() {
    let _isolation = crate::core::data_dir::isolated_data_dir();
    let (root, path) = project();
    let other = tempfile::tempdir().unwrap();
    std::fs::write(
        &path,
        format!("{BASE}[context]\ndeny_tools = ['ctx_read']\n"),
    )
    .unwrap();
    REQUEST_PROJECT
        .scope(std::cell::RefCell::new(Some(root.path().into())), async {
            tokio::task::yield_now().await;
            assert!(!active().unwrap().tool_allowed("ctx_read"));
            REQUEST_PROJECT
                .scope(std::cell::RefCell::new(Some(other.path().into())), async {
                    assert!(active().is_none());
                })
                .await;
            assert!(!active().unwrap().tool_allowed("ctx_read"));
            std::fs::write(&path, BASE).unwrap();
            assert!(active().unwrap().tool_allowed("ctx_read"));
        })
        .await;
}
