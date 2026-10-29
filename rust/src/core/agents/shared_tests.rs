// SPDX-License-Identifier: Apache-2.0

use super::AgentRegistry;

fn project(dir: &tempfile::TempDir, name: &str) -> String {
    let root = dir.path().join(name);
    std::fs::create_dir_all(&root).unwrap();
    root.to_str().unwrap().to_string()
}

/// Regression (AP02/I01, I02): shared knowledge was one installation-wide
/// file, so a fact shared in one project reached the agents of every other
/// project, and two projects sharing the same key wrote into one mailbox.
#[test]
fn shared_knowledge_stays_in_its_project() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let (p1, p2) = (project(&dir, "p1"), project(&dir, "p2"));
    let mut registry = AgentRegistry::new();
    registry
        .share_knowledge(&p1, "a1", "decision", &[("db".into(), "canary-p1".into())])
        .unwrap();
    registry
        .share_knowledge(&p2, "a3", "decision", &[("db".into(), "canary-p2".into())])
        .unwrap();

    let in_p2 = registry.receive_shared_knowledge(&p2, "a2").unwrap();
    assert_eq!(in_p2.len(), 1);
    assert_eq!(in_p2[0].text, "db=canary-p2");
    let in_p1 = registry.receive_shared_knowledge(&p1, "b1").unwrap();
    assert_eq!(in_p1.len(), 1);
    assert_eq!(in_p1[0].text, "db=canary-p1");
    // Delivered once per reader.
    assert!(
        registry
            .receive_shared_knowledge(&p1, "b1")
            .unwrap()
            .is_empty()
    );
}

/// M01/M02: the unscoped file of earlier releases is quarantined unread,
/// never served to any project, and a second run changes nothing.
#[test]
fn legacy_unscoped_facts_are_quarantined_not_served() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let p1 = project(&dir, "p1");
    let legacy = crate::core::paths::data_dir()
        .unwrap()
        .join("shared_knowledge.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(
        &legacy,
        r#"[{"from_agent":"old","category":"c","key":"k","value":"canary-legacy","timestamp":"2026-10-01T00:00:00Z","received_by":[]}]"#,
    )
    .unwrap();
    let mut registry = AgentRegistry::new();
    assert!(
        registry
            .receive_shared_knowledge(&p1, "b1")
            .unwrap()
            .is_empty()
    );
    assert!(!legacy.exists());
    let quarantine = super::shared_dir().unwrap().join("quarantine");
    let kept: Vec<_> = std::fs::read_dir(&quarantine).unwrap().collect();
    assert_eq!(kept.len(), 1);
    assert!(
        registry
            .receive_shared_knowledge(&p1, "b1")
            .unwrap()
            .is_empty()
    );
    assert_eq!(std::fs::read_dir(&quarantine).unwrap().count(), 1);
}

/// M03/M04: a quarantine interrupted before its move serves nothing and is
/// completed by the next run. An older client that writes the
/// installation-wide file again after the migration gets it quarantined
/// beside the first copy, which stays byte-identical; nothing of either is
/// served, and this release never writes the legacy file itself.
#[test]
fn an_interrupted_quarantine_and_an_old_client_recreating_the_file_serve_nothing() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let p1 = project(&dir, "p1");
    let legacy = crate::core::paths::data_dir()
        .unwrap()
        .join("shared_knowledge.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    let old_client = |value: &str| {
        std::fs::write(
            &legacy,
            format!(
                r#"[{{"from_agent":"old","category":"c","key":"k","value":"{value}","timestamp":"2026-10-01T00:00:00Z","received_by":[]}}]"#
            ),
        )
        .unwrap();
    };
    let quarantined = || -> Vec<(std::path::PathBuf, Vec<u8>)> {
        let quarantine = super::shared_dir().unwrap().join("quarantine");
        let mut files: Vec<_> = std::fs::read_dir(quarantine)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| (entry.path(), std::fs::read(entry.path()).unwrap()))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    };
    let mut registry = AgentRegistry::new();
    old_client("canary-first");

    super::QUARANTINE_FAILS.with(|armed| armed.set(true));
    assert!(registry.receive_shared_knowledge(&p1, "b1").is_err());
    assert!(legacy.exists());
    assert!(quarantined().is_empty());

    assert!(
        registry
            .receive_shared_knowledge(&p1, "b1")
            .unwrap()
            .is_empty()
    );
    assert!(!legacy.exists());
    let first = quarantined();
    assert_eq!(first.len(), 1);

    registry
        .share_knowledge(&p1, "a1", "decision", &[("db".into(), "scoped".into())])
        .unwrap();
    assert!(
        !legacy.exists(),
        "this release never writes the legacy file"
    );

    old_client("canary-second");
    let served = registry.receive_shared_knowledge(&p1, "b1").unwrap();
    assert_eq!(served.len(), 1);
    assert_eq!(served[0].text, "db=scoped");
    assert!(!legacy.exists());
    let both = quarantined();
    assert_eq!(both.len(), 2);
    assert!(
        both.contains(&first[0]),
        "the first copy is never overwritten"
    );

    // The same content once more: already quarantined, the file just goes.
    old_client("canary-second");
    assert!(
        registry
            .receive_shared_knowledge(&p1, "b1")
            .unwrap()
            .is_empty()
    );
    assert!(!legacy.exists());
    assert_eq!(quarantined(), both);
}
