// SPDX-License-Identifier: Apache-2.0
use super::*;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn setup_checks_by_default_and_start_requires_separate_policy_review() {
    let setup = parse(&args(&[
        "--project",
        "/work/repo",
        "--policy-pack",
        "baseline",
    ]))
    .unwrap();
    assert!(setup.invocation.check);
    assert_eq!(setup.policy_pack.as_deref(), Some("baseline"));
    let start = parse(&args(&["--project=/work/repo", "--start"])).unwrap();
    assert!(!start.invocation.check);
    for values in [
        vec!["--project", "/work", "--start", "--policy-pack", "baseline"],
        vec!["--project", "/work", "--check", "--policy-pack", "baseline"],
        vec!["--project", "/work", "--check", "--start"],
        vec!["--project", "/work", "--start", "--start"],
        vec!["--project", "/work", "--policy-pack=missing"],
        vec!["--project", "--start", "/work"],
        vec!["--project", "--policy-pack", "baseline", "/work"],
        vec!["--project", "/work", "--codex", "--start"],
        vec![
            "--project",
            "/work",
            "--policy-pack=baseline",
            "--policy-pack=baseline",
        ],
    ] {
        assert!(parse(&args(&values)).is_err(), "{values:?}");
    }
}

#[test]
fn selected_builtin_is_complete_parseable_and_existing_policy_is_preserved() {
    let temp = tempfile::tempdir().unwrap();
    create_policy(temp.path(), "baseline").unwrap();
    let path = temp.path().join(POLICY_RELATIVE);
    let bytes = std::fs::read(&path).unwrap();
    let pack = policy::parse_file(&path).unwrap();
    assert_eq!(pack.name, "baseline");
    assert!(
        policy::resolve(&pack)
            .unwrap()
            .redaction
            .contains_key("private_key")
    );
    assert!(create_policy(temp.path(), "strict-redaction").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[cfg(unix)]
#[test]
fn initialization_does_not_follow_policy_directory_or_file_symlinks() {
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let directory = project.path().join(".lean-ctx");
    std::os::unix::fs::symlink(outside.path(), &directory).unwrap();
    assert!(create_policy(project.path(), "baseline").is_err());
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    std::fs::remove_file(&directory).unwrap();
    std::fs::create_dir(&directory).unwrap();
    let target = outside.path().join("absent.toml");
    std::os::unix::fs::symlink(&target, directory.join("policy.toml")).unwrap();
    assert!(create_policy(project.path(), "baseline").is_err());
    assert!(!target.exists());
}

#[test]
fn concurrent_initializers_publish_one_whole_policy_without_overwriting() {
    let project = tempfile::tempdir().unwrap();
    let barrier = std::sync::Barrier::new(2);
    let successes = std::thread::scope(|scope| {
        let handles: Vec<_> = ["baseline", "strict-redaction"]
            .into_iter()
            .map(|name| {
                let barrier = &barrier;
                let path = project.path();
                scope.spawn(move || {
                    barrier.wait();
                    create_policy(path, name).is_ok()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| usize::from(handle.join().unwrap()))
            .sum::<usize>()
    });
    assert_eq!(successes, 1);
    let pack = policy::parse_file(&project.path().join(POLICY_RELATIVE)).unwrap();
    assert!(matches!(
        pack.name.as_str(),
        "baseline" | "strict-redaction"
    ));
    policy::resolve(&pack).unwrap();
    assert_eq!(
        std::fs::read_dir(project.path().join(".lean-ctx"))
            .unwrap()
            .count(),
        1
    );
}
