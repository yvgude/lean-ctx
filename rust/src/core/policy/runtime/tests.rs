// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::collections::BTreeMap;
use std::fs;

/// AP04 review S3: a request bound to no project gets no source view
/// for the process's working directory; it is refused as unresolved.
#[test]
fn a_request_without_a_project_gets_no_source_view() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let viewed =
        REQUEST_PROJECT.sync_scope(std::cell::RefCell::new(None), || with_source_view(|| ()));
    assert_eq!(viewed, Err("project_unresolved".to_string()));
}

use crate::test_env::CurrentDirGuard;

fn rp(allow: Option<Vec<&str>>, deny: Vec<&str>, redaction: &[(&str, &str)]) -> ResolvedPolicy {
    ResolvedPolicy {
        name: "test".into(),
        version: "1.0.0".into(),
        description: "t".into(),
        chain: vec![],
        default_read_mode: None,
        allow_tools: allow.map(|a| a.into_iter().map(String::from).collect()),
        deny_tools: deny.into_iter().map(String::from).collect(),
        max_context_tokens: None,
        audit_retention_days: None,
        redaction: redaction
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect::<BTreeMap<_, _>>(),
        filters: crate::core::policy::FilterRules::default(),
        egress: crate::core::policy::EgressRules::default(),
        routing: crate::core::policy::RoutingPolicyRules::default(),
        budgets: crate::core::policy::BudgetRules::default(),
    }
}

fn write_policy(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let directory = root.join(".lean-ctx");
    fs::create_dir_all(&directory).expect("create policy directory");
    let path = directory.join("policy.toml");
    fs::write(
        &path,
        format!("name = {name:?}\nversion = \"1.0.0\"\ndescription = \"source view test\"\n"),
    )
    .expect("write policy");
    path
}

#[test]
fn deny_list_blocks_listed_tool() {
    let p = ActivePolicy::from_resolved(rp(None, vec!["ctx_url_read"], &[]));
    assert!(!p.tool_allowed("ctx_url_read"));
    assert!(p.tool_allowed("ctx_read"));
}

#[test]
fn allow_list_is_exclusive() {
    let p = ActivePolicy::from_resolved(rp(Some(vec!["ctx_read"]), vec![], &[]));
    assert!(p.tool_allowed("ctx_read"));
    assert!(!p.tool_allowed("ctx_shell"));
}

fn hive(redaction: &[(&str, &str)]) -> crate::core::hive::policy::FilterRules {
    crate::core::hive::policy::FilterRules {
        redaction: redaction
            .iter()
            .map(|(label, pattern)| ((*label).to_string(), (*pattern).to_string()))
            .collect(),
        ..Default::default()
    }
}

/// Hive filter packs alone make a policy of content rules only: every
/// tool stays allowed and the pack rule redacts.
#[test]
fn hive_filters_alone_redact_without_tool_or_path_rules() {
    let rules = hive(&[("hive:ahv", r"756\.\d{4}\.\d{4}\.\d{2}")]);
    let policy = apply_org_floor(None, Ok(None), None, &rules, None).expect("a policy");
    assert!(policy.content_valid);
    assert!(policy.tool_allowed("ctx_read"));
    assert!(policy.tool_allowed("ctx_shell"));
    assert_eq!(policy.hive_rules, 1);
    assert_eq!(policy.redaction[0].0, "hive:ahv");
    assert!(policy.redaction[0].1.is_match("756.1234.5678.97"));
    assert!(apply_org_floor(None, Ok(None), None, &hive(&[]), None).is_none());
}

/// Hive rules have their own budget: a pack at its own limit stays
/// valid with them, and a Hive rule outside its bounds fails closed.
#[test]
fn hive_rules_keep_their_own_budget_and_fail_closed_out_of_bounds() {
    use super::super::content::MAX_REDACTION_RULES;
    let labels: Vec<String> = (0..MAX_REDACTION_RULES).map(|i| format!("r{i}")).collect();
    let full: Vec<(&str, &str)> = labels.iter().map(|l| (l.as_str(), "x[0-9]")).collect();
    let with_hive = ActivePolicy::from_resolved_with_hive(
        rp(None, vec![], &full),
        &hive(&[("hive:card", "4[0-9]{15}")]),
    );
    assert!(with_hive.content_valid);
    assert_eq!(with_hive.redaction.len(), MAX_REDACTION_RULES + 1);
    for bad in [("card", "4[0-9]{15}"), ("hive:card", "(")] {
        let policy = ActivePolicy::from_resolved_with_hive(rp(None, vec![], &[]), &hive(&[bad]));
        assert!(!policy.content_valid, "{bad:?}");
    }
}

#[test]
fn pack_labels_with_the_hive_prefix_are_refused() {
    let pack = crate::core::policy::load(
        "name = \"p\"\nversion = \"1.0.0\"\ndescription = \"t\"\n[redaction]\n\"hive:x\" = \"y\"\n",
    );
    assert!(pack.is_err(), "{pack:?}");
}

#[test]
fn compiles_redaction_patterns() {
    let p = ActivePolicy::from_resolved(rp(None, vec![], &[("emp", r"EMP-\d{4}")]));
    assert_eq!(p.redaction.len(), 1);
    assert_eq!(p.redaction[0].0, "emp");
}

#[test]
fn load_from_disk_fails_closed_on_invalid_pack() {
    let temp = tempfile::tempdir().expect("create temporary directory");
    let policy_dir = temp.path().join(".lean-ctx");
    fs::create_dir(&policy_dir).expect("create policy directory");
    fs::write(policy_dir.join("policy.toml"), "[policy\n").expect("write invalid policy");
    let _cwd = CurrentDirGuard::enter(temp.path());

    let policy = load_from_disk().expect("invalid pack activates deny-all");

    assert!(!policy.tool_allowed("ctx_read"));
    assert!(!policy.tool_allowed("ctx_shell"));
}

#[test]
fn invalid_org_policy_cannot_fall_back_to_permissive_local_policy() {
    for local in [None, Some(rp(None, vec![], &[]))] {
        let policy = apply_org_floor(
            local,
            Err("invalid org signature".into()),
            None,
            &Default::default(),
            None,
        )
        .unwrap();
        assert!(!policy.tool_allowed("ctx_read"));
        assert!(!policy.tool_allowed("ctx_shell"));
    }
    assert!(apply_org_floor(None, Ok(None), None, &Default::default(), None).is_none());
}

#[test]
fn a_policy_ends_with_its_signed_horizon_for_every_holder_and_for_good() {
    let floor = || Ok(Some(rp(None, vec![], &[])));
    let hour = chrono::Duration::hours(1);
    // No horizon, or one ahead: the policy's own rules decide.
    for horizon in [None, Some(chrono::Utc::now() + hour)] {
        let policy = apply_org_floor(None, floor(), horizon, &Default::default(), None).unwrap();
        assert!(policy.within_horizon() && policy.tool_allowed("ctx_read"));
    }

    // Past its horizon the same rules permit nothing, through any clone.
    let mut policy = ActivePolicy::from_resolved(rp(None, vec![], &[]))
        .until(Some(chrono::Utc::now() - chrono::Duration::seconds(1)));
    assert!(!policy.within_horizon() && !policy.tool_allowed("ctx_read"));
    // The expiry was observed. Were the clock set back behind the horizon
    // — here: the horizon moved ahead of the clock — nothing comes back.
    policy.horizon = Some(chrono::Utc::now() + hour);
    let shared = Arc::new(policy);
    let twin = Arc::clone(&shared);
    assert!(!shared.within_horizon() && !twin.tool_allowed("ctx_read"));
}

#[test]
fn explicit_project_policy_does_not_leak_between_roots() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let denied = tempfile::tempdir().unwrap();
    let absent = tempfile::tempdir().unwrap();
    let directory = denied.path().join(".lean-ctx");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("policy.toml"),
        "name = \"project\"\nversion = \"1.0.0\"\ndescription = \"test\"\n[context]\ndeny_tools = [\"ctx_work_graph\"]\n").unwrap();
    assert!(
        !for_project(denied.path())
            .unwrap()
            .unwrap()
            .tool_allowed("ctx_work_graph")
    );
    assert!(for_project(absent.path()).unwrap().is_none());
    fs::write(directory.join("policy.toml"), "invalid policy").unwrap();
    assert!(for_project(denied.path()).is_err());
    assert!(for_project(&absent.path().join("missing")).is_err());
}

#[test]
fn source_view_pins_active_policy_and_rejects_persistent_change() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _override_guard = lock_test_override();
    let root = tempfile::tempdir().expect("create project");
    let path = write_policy(root.path(), "view-a");

    let result = REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(root.path().to_path_buf())),
        || {
            with_source_view(|| {
                assert_eq!(active().unwrap().resolved.name, "view-a");
                write_policy(root.path(), "view-b");
                assert_eq!(active().unwrap().resolved.name, "view-a");
            })
        },
    );

    assert!(result.is_err());
    assert!(path.exists());
}

#[test]
fn source_view_a_to_b_to_a_never_uses_transient_policy_b() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _override_guard = lock_test_override();
    let root = tempfile::tempdir().expect("create project");
    write_policy(root.path(), "view-a");

    let result = REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(root.path().to_path_buf())),
        || {
            with_source_view(|| {
                write_policy(root.path(), "view-b");
                assert_eq!(active().unwrap().resolved.name, "view-a");
                write_policy(root.path(), "view-a");
                active().unwrap().resolved.name.clone()
            })
        },
    );

    assert_eq!(result.unwrap(), "view-a");
}

#[test]
fn source_view_pins_explicit_no_policy_and_rejects_policy_creation() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _override_guard = lock_test_override();
    let root = tempfile::tempdir().expect("create project");

    let result = REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(root.path().to_path_buf())),
        || {
            with_source_view(|| {
                assert!(active().is_none());
                write_policy(root.path(), "new-policy");
                assert!(active().is_none());
            })
        },
    );

    assert!(result.is_err());
}

#[test]
fn source_view_rejects_nested_project_rebinding() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _override_guard = lock_test_override();
    let first = tempfile::tempdir().expect("create first project");
    let second = tempfile::tempdir().expect("create second project");
    write_policy(first.path(), "first-project");
    write_policy(second.path(), "second-project");

    let result = REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(first.path().to_path_buf())),
        || {
            with_source_view(|| {
                REQUEST_PROJECT.sync_scope(
                    std::cell::RefCell::new(Some(second.path().to_path_buf())),
                    || assert!(!active().unwrap().tool_allowed("ctx_read")),
                );
            })
        },
    );

    assert!(result.is_err());
}

#[test]
fn source_view_rejects_policy_removal_and_invalid_initial_policy() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _override_guard = lock_test_override();
    let root = tempfile::tempdir().expect("create project");
    let path = write_policy(root.path(), "view-a");

    let removed = REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(root.path().to_path_buf())),
        || {
            with_source_view(|| {
                assert_eq!(active().unwrap().resolved.name, "view-a");
                fs::remove_file(&path).expect("remove policy");
                assert_eq!(active().unwrap().resolved.name, "view-a");
            })
        },
    );
    assert!(removed.is_err());

    fs::write(&path, "[policy\n").expect("write invalid policy");
    let called = std::cell::Cell::new(false);
    let invalid = REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(root.path().to_path_buf())),
        || with_source_view(|| called.set(true)),
    );
    assert!(invalid.is_err());
    assert!(!called.get());
}

fn fixture_role(
    name: &str,
    boundary_mode: &str,
    allow_secret_paths: bool,
    allowed: &[&str],
) -> crate::core::roles::Role {
    use crate::core::roles::{IoPolicy, Role, RoleMeta, ToolPolicy};
    Role {
        role: RoleMeta {
            name: name.into(),
            inherits: None,
            description: "source view test".into(),
            shell_policy: "deny".into(),
        },
        tools: ToolPolicy {
            allowed: allowed.iter().map(|tool| (*tool).into()).collect(),
            denied: Vec::new(),
        },
        io: IoPolicy {
            boundary_mode: boundary_mode.into(),
            allow_secret_paths,
            ..Default::default()
        },
    }
}

#[test]
fn source_view_pins_full_role_through_a_to_b_to_a_and_rejects_persistent_change() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _override_guard = lock_test_override();
    let root = tempfile::tempdir().expect("create project");
    let role_a = fixture_role("role-a", "warn", true, &["ctx_read"]);
    // Secret-path visibility stays fixed so the regression exercises the
    // full role fingerprint (tool and boundary policy), not only that bit.
    let role_b = fixture_role("role-b", "enforce", true, &["ctx_shell"]);

    let transient = crate::core::roles::with_test_active_role(role_a.clone(), || {
        with_project_source_view(root.path().to_str().unwrap(), || {
            with_source_view(|| {
                assert_eq!(
                    role_fingerprint(&crate::core::roles::active_role()).unwrap(),
                    role_fingerprint(&role_a).unwrap()
                );
                crate::core::roles::set_test_active_role(role_b.clone());
                let pinned = crate::core::roles::active_role();
                assert_eq!(pinned.role.name, "role-a");
                assert_eq!(pinned.io.boundary_mode, "warn");
                assert!(pinned.io.allow_secret_paths);
                assert!(pinned.is_tool_allowed("ctx_read"));
                assert!(!pinned.is_tool_allowed("ctx_shell"));
                crate::core::roles::set_test_active_role(role_a.clone());
            })
        })
    });
    assert!(transient.is_ok(), "a transient role B is never observed");

    let persistent = crate::core::roles::with_test_active_role(role_a.clone(), || {
        with_project_source_view(root.path().to_str().unwrap(), || {
            with_source_view(|| {
                crate::core::roles::set_test_active_role(role_b.clone());
                assert_eq!(crate::core::roles::active_role().role.name, "role-a");
            })
        })
    });
    assert!(
        persistent.is_err(),
        "a changed full role invalidates the view"
    );
}

#[test]
fn closed_inherited_source_view_returns_a_denying_pinned_role() {
    let root = tempfile::tempdir().expect("create project");
    let role = fixture_role("role-a", "warn", true, &["ctx_read"]);
    let role_fingerprint = role_fingerprint(&role).unwrap();
    let valid = Arc::new(AtomicBool::new(true));
    let view = SourceView {
        project: root.path().to_path_buf(),
        policy: None,
        fingerprint: blake3::hash(b"test"),
        role,
        role_fingerprint,
        lifetime: None,
        observation: None,
        valid: valid.clone(),
        live: Arc::new(AtomicBool::new(false)),
    };

    REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(root.path().to_path_buf())),
        || {
            SOURCE_VIEW.sync_scope(std::cell::RefCell::new(Some(view)), || {
                let closed = crate::core::roles::active_role();
                assert_eq!(closed.role.name, "closed-source-view");
                assert!(!closed.is_tool_allowed("ctx_read"));
                assert!(!closed.io.allow_secret_paths);
                // A straggler on an ended view is refused, but must not
                // invalidate it for the owner's closing check (false
                // ctx_compose "source authority changed" refusals).
                assert!(with_source_view(|| ()).is_err());
            });
        },
    );
    assert!(
        valid.load(Ordering::Acquire),
        "an ended view is not invalidated by a late nested check"
    );
}

#[test]
fn project_source_view_uses_fallback_and_preserves_bound_project() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _override_guard = lock_test_override();
    let fallback = tempfile::tempdir().expect("create fallback project");
    let bound = tempfile::tempdir().expect("create bound project");
    // The runtime's spelling of a root: no Windows `\\?\` prefix.
    let fallback_canonical = crate::core::pathutil::safe_canonicalize(fallback.path()).unwrap();
    let bound_canonical = crate::core::pathutil::safe_canonicalize(bound.path()).unwrap();
    write_policy(fallback.path(), "fallback-project");
    write_policy(bound.path(), "bound-project");
    let fallback_root = fallback.path().to_str().unwrap();

    let unbound = with_project_source_view(fallback_root, || {
        assert_eq!(
            detached_project_root(fallback.path()).unwrap().as_path(),
            fallback_canonical.as_path()
        );
        active().unwrap().resolved.name.clone()
    });
    assert_eq!(unbound.unwrap(), "fallback-project");

    let already_bound = REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(bound.path().to_path_buf())),
        || {
            with_project_source_view(fallback_root, || {
                assert_eq!(
                    detached_project_root(fallback.path()).unwrap().as_path(),
                    bound_canonical.as_path()
                );
                active().unwrap().resolved.name.clone()
            })
        },
    );
    assert_eq!(already_bound.unwrap(), "bound-project");
}

#[cfg(unix)]
#[test]
fn source_view_canonicalizes_alias_without_weakening_conflicting_root_rejection() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _override_guard = lock_test_override();
    let parent = tempfile::tempdir().expect("create temporary parent");
    let root = parent.path().join("project");
    fs::create_dir(&root).expect("create project root");
    write_policy(&root, "alias-project");
    let alias = parent.path().join("project-alias");
    std::os::unix::fs::symlink(&root, &alias).expect("create project alias");
    let canonical = root.canonicalize().unwrap();
    assert_eq!(alias.canonicalize().unwrap(), canonical);
    assert_ne!(alias, canonical, "fixture must exercise a real path alias");

    let other = tempfile::tempdir().expect("create conflicting project");
    write_policy(other.path(), "other-project");
    let result = with_project_source_view(&alias.to_string_lossy(), || {
        let bound = REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .unwrap()
            .unwrap();
        assert_eq!(bound, canonical, "request authority is normalized once");

        with_explicit_project_source_view(&root, |authority| {
            authority
                .verify()
                .expect("matching alias remains authorized");
        })
        .expect("canonical root matches the active project view");

        let called = std::cell::Cell::new(false);
        assert!(
            with_explicit_project_source_view(other.path(), |_| called.set(true)).is_err(),
            "a genuinely different root remains refused"
        );
        assert!(!called.get(), "conflicting operation must not run");

        with_explicit_project_source_view(&root, |_| ())
            .expect("a rejected conflicting request must not poison the valid view");
    });
    assert!(
        result.is_ok(),
        "a filesystem alias is the same project root"
    );
}
