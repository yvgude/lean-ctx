// SPDX-License-Identifier: Apache-2.0

use super::*;

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

// --- parser ---------------------------------------------------------

#[test]
fn no_arguments_prints_help() {
    assert_eq!(parse_args(&[]), Parsed::Help);
    assert_eq!(parse_args(&argv(&["--help"])), Parsed::Help);
    assert_eq!(parse_args(&argv(&["--project", "/x", "-h"])), Parsed::Help);
}

#[test]
fn accepts_both_value_forms() {
    let space = parse_args(&argv(&["--project", "/tmp/p", "--codex", "/bin/c"]));
    let equals = parse_args(&argv(&["--project=/tmp/p", "--codex=/bin/c"]));
    assert_eq!(space, equals);
    assert_eq!(
        space,
        Parsed::Run(Invocation {
            project: "/tmp/p".to_string(),
            codex: Some("/bin/c".to_string()),
            gitlab: None,
            glab: None,
            check: false,
        })
    );
}

#[test]
fn selected_gitlab_requires_complete_explicit_identity() {
    for extra in [
        vec!["--gitlab-host", "gitlab.example.test"],
        vec!["--gitlab-project", "5"],
        vec!["--glab", "/bin/glab"],
        vec![
            "--gitlab-host",
            "gitlab.example.test",
            "--gitlab-project",
            "5",
            "--gitlab-namespace",
            "g/p",
            "--gitlab-project",
            "6",
        ],
    ] {
        let mut args = argv(&["--project", "/tmp/p"]);
        args.extend(argv(&extra));
        assert!(matches!(parse_args(&args), Parsed::Error(_)));
    }
    let parsed = parse_args(&argv(&[
        "--project=/tmp/p",
        "--gitlab-host=gitlab.example.test",
        "--gitlab-project=5",
        "--gitlab-namespace=g/p",
        "--glab=/bin/glab",
    ]));
    let Parsed::Run(invocation) = parsed else {
        panic!("valid source selection rejected")
    };
    assert_eq!(invocation.gitlab.unwrap().project, 5);
    assert_eq!(invocation.glab.as_deref(), Some("/bin/glab"));
}

#[test]
fn check_is_a_flag_without_value() {
    assert_eq!(
        parse_args(&argv(&["--project", "/tmp/p", "--check"])),
        Parsed::Run(Invocation {
            project: "/tmp/p".to_string(),
            codex: None,
            gitlab: None,
            glab: None,
            check: true,
        })
    );
    assert!(matches!(
        parse_args(&argv(&["--project", "/tmp/p", "--check=yes"])),
        Parsed::Error(_)
    ));
}

#[test]
fn rejects_unexpected_input() {
    for bad in [
        vec!["--project", "/tmp/p", "extra"],
        vec!["--project", "/tmp/p", "--sandbox", "danger-full-access"],
        vec!["--project", "/tmp/p", "--project", "/tmp/q"],
        vec!["--project"],
        vec!["--project", "--codex"],
        vec!["--project", ""],
        vec!["--codex", "/bin/c"],
        vec!["--check"],
    ] {
        assert!(
            matches!(parse_args(&argv(&bad)), Parsed::Error(_)),
            "expected rejection for {bad:?}"
        );
    }
}

#[test]
fn rejects_control_characters_in_paths() {
    assert!(matches!(
        parse_args(&argv(&["--project", "/tmp/a\nb"])),
        Parsed::Error(_)
    ));
}

// --- project root guards --------------------------------------------

fn guards(base: &Path) -> Guards {
    Guards {
        real_home: base.join("home"),
        temp_root: base.join("tmp"),
        codex: base.join("bin/codex"),
        lean_ctx: base.join("bin/lean-ctx"),
    }
}

#[test]
fn filesystem_root_is_refused() {
    let base = PathBuf::from("/base");
    let err = validate_project_root(Path::new("/"), &guards(&base))
        .expect_err("`/` must not be accepted");
    assert!(err.contains("filesystem root"), "{err}");
}

#[test]
fn project_may_not_swallow_session_critical_paths() {
    let base = PathBuf::from("/base");
    for root in ["/base", "/base/home", "/base/tmp", "/base/bin"] {
        assert!(
            validate_project_root(Path::new(root), &guards(&base)).is_err(),
            "{root} must be refused"
        );
    }
}

#[test]
fn disjoint_project_root_is_accepted() {
    let base = PathBuf::from("/base");
    assert!(validate_project_root(Path::new("/base/work/repo"), &guards(&base)).is_ok());
}

// --- generated config ------------------------------------------------

struct Fixture {
    _temp: tempfile::TempDir,
    pre: Preflight,
    session: Session,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");
    let project = root.join("project");
    std::fs::create_dir_all(project.join(".lean-ctx")).expect("project dirs");
    std::fs::write(project.join(POLICY_RELATIVE), "name=\"fixture\"\n").expect("policy");

    let pre = Preflight {
        gitlab: None,
        control_paths: vec![project.join(POLICY_RELATIVE)],
        operator_credentials: vec![root.join("state/dashboard.token")],
        project,
        codex: root.join("bin/codex"),
        lean_ctx: root.join("bin/lean-ctx"),
        source_codex_home: root.join("real-codex"),
        real_home: root.join("real-home"),
        mcp_env: mcp_env_from(
            root.join("project").to_str().expect("utf8"),
            root.join("real-home").to_str().expect("utf8"),
            &|key| match key {
                "LEAN_CTX_STATE_DIR" => Some("/opt/state".to_string()),
                "XDG_CONFIG_HOME" => Some("  ".to_string()),
                "OPENAI_API_KEY" => Some("sk-should-never-appear".to_string()),
                _ => None,
            },
        ),
    };
    let session = Session {
        home: root.join("home"),
        codex_home: root.join("codex"),
        workspace: root.join("workspace"),
    };
    Fixture {
        _temp: temp,
        pre,
        session,
    }
}

#[test]
fn config_encodes_the_full_boundary() {
    let fixture = fixture();
    let profile = write_guard::profile(&fixture.pre, &fixture.session).expect("profile");
    let rendered = render_config(&fixture.pre, &fixture.session, &profile).expect("render");
    let parsed: toml::Table = rendered
        .parse()
        .expect("generated config must be valid TOML");

    assert_eq!(parsed["project_doc_max_bytes"].as_integer(), Some(0));
    assert_eq!(
        parsed["default_permissions"].as_str(),
        Some(PERMISSION_PROFILE)
    );
    assert_eq!(parsed["approval_policy"].as_str(), Some("on-request"));
    assert_eq!(parsed["features"]["shell_tool"].as_bool(), Some(false));

    let filesystem = &parsed["permissions"][PERMISSION_PROFILE]["filesystem"];
    let project = fixture.pre.project.to_str().expect("utf8");
    let workspace = fixture.session.workspace.to_str().expect("utf8");
    assert_eq!(filesystem[":minimal"].as_str(), Some("read"));
    assert_eq!(filesystem[workspace].as_str(), Some("read"));
    assert_eq!(filesystem[project].as_str(), Some("deny"));
    assert_eq!(
        filesystem[fixture.session.home.to_str().unwrap()].as_str(),
        Some("deny")
    );
    assert_eq!(
        filesystem[fixture.session.codex_home.to_str().unwrap()].as_str(),
        Some("deny")
    );

    // No legacy sandbox switch alongside the profile, and no unsupported
    // `[tools]` keys that `--strict-config` would reject.
    assert!(parsed.get("sandbox_mode").is_none());
    assert!(parsed.get("tools").is_none());
    // Nothing that would auto-approve, bypass trust or pick a provider.
    for forbidden in [
        "model",
        "model_provider",
        "model_providers",
        "hooks",
        "plugins",
        "trusted_projects",
    ] {
        assert!(
            parsed.get(forbidden).is_none(),
            "{forbidden} must not be configured"
        );
    }
}

#[test]
fn selected_source_uses_wrapper_and_denies_credentials_to_native_tools() {
    let mut fixture = fixture();
    let config_dir = fixture.pre.real_home.join("glab-cli");
    fixture.pre.operator_credentials.push(config_dir.clone());
    fixture.pre.gitlab = Some(gitlab_bootstrap::Launch {
        source: crate::core::providers::selected_gitlab::Selection {
            host: "gitlab.example.test".into(),
            project: 5,
            namespace: "g/p".into(),
        },
        glab: fixture.pre.real_home.join("bin/glab"),
        config_dir: config_dir.clone(),
    });
    let profile = write_guard::profile(&fixture.pre, &fixture.session).unwrap();
    let rendered = render_config(&fixture.pre, &fixture.session, &profile).unwrap();
    let doc: toml::Table = rendered.parse().unwrap();
    assert_eq!(
        doc["mcp_servers"]["lean-ctx"]["command"].as_str(),
        fixture.pre.lean_ctx.to_str()
    );
    let args = doc["mcp_servers"]["lean-ctx"]["args"].as_array().unwrap();
    assert_eq!(args[0].as_str(), Some(gitlab_bootstrap::WRAPPER));
    assert_eq!(args[2].as_str(), Some(profile.as_str()));
    let launch: gitlab_bootstrap::Launch = serde_json::from_str(args[1].as_str().unwrap()).unwrap();
    assert_eq!(launch.source.project, 5);
    assert_eq!(
        doc["permissions"][PERMISSION_PROFILE]["filesystem"][config_dir.to_str().unwrap()].as_str(),
        Some("deny")
    );
    assert!(!rendered.contains("PRIVATE-TOKEN"));
}

#[test]
fn config_has_exactly_one_mcp_server_bound_to_the_project() {
    let fixture = fixture();
    let profile = write_guard::profile(&fixture.pre, &fixture.session).expect("profile");
    let rendered = render_config(&fixture.pre, &fixture.session, &profile).expect("render");
    let parsed: toml::Table = rendered.parse().expect("valid TOML");

    let servers = parsed["mcp_servers"].as_table().expect("mcp_servers table");
    assert_eq!(servers.len(), 1);
    let lean = &servers["lean-ctx"];
    // The ordinary start uses the same outside wrapper as a selected
    // source: it applies the inline kernel profile to the MCP executable
    // and then supervises that server's command children, which the
    // sandboxed server cannot do for itself.
    assert_eq!(lean["command"].as_str(), fixture.pre.lean_ctx.to_str());
    assert_eq!(
        lean["args"].as_array().map(Vec::len),
        Some(3),
        "the wrapper takes exactly a launch configuration and the profile"
    );
    assert_eq!(lean["args"][0].as_str(), Some(gitlab_bootstrap::WRAPPER));
    assert_eq!(
        lean["args"][1].as_str(),
        Some("null"),
        "an ordinary protected start carries no launch configuration"
    );
    assert_eq!(lean["args"][2].as_str(), Some(profile.as_str()));

    let env = lean["env"].as_table().expect("env table");
    assert_eq!(
        env["LEAN_CTX_PROJECT_ROOT"].as_str(),
        fixture.pre.project.to_str()
    );
    assert_eq!(env["HOME"].as_str(), fixture.pre.real_home.to_str());
    assert_eq!(
        env[REQUIRED_POLICY_ROOT_ENV].as_str(),
        fixture.pre.project.to_str()
    );
    assert_eq!(env["LEAN_CTX_STATE_DIR"].as_str(), Some("/opt/state"));
    // Blank values are not forwarded, and nothing outside the documented
    // list ever is.
    assert!(env.get("XDG_CONFIG_HOME").is_none());
    assert!(!rendered.contains("sk-should-never-appear"));
}

#[test]
fn config_rendering_is_deterministic() {
    let fixture = fixture();
    let profile = write_guard::profile(&fixture.pre, &fixture.session).expect("profile");
    let first = render_config(&fixture.pre, &fixture.session, &profile).expect("render");
    let second = render_config(&fixture.pre, &fixture.session, &profile).expect("render");
    assert_eq!(first, second);
}

#[test]
fn instructions_name_the_bound_root_without_quoting_project_files() {
    let text = instructions_text("/work/repo");
    assert!(text.contains("/work/repo"));
    assert!(text.contains("lean-ctx"));
    assert!(text.contains("DENIED"));
}

// --- environment ------------------------------------------------------

#[test]
fn client_env_is_built_from_scratch() {
    let temp = tempfile::tempdir().expect("tempdir");
    let session = Session {
        home: temp.path().join("home"),
        codex_home: temp.path().join("codex"),
        workspace: temp.path().join("workspace"),
    };
    let env = client_env(&session, &|key| match key {
        "DO_NOT_TRACK" => Some("1".to_string()),
        "PATH" => Some("/usr/bin:/bin".to_string()),
        "TERM" => Some("xterm-256color".to_string()),
        "LANG" => Some(String::new()),
        "CODEX_PROFILE" => Some("some-profile".to_string()),
        "LEAN_CTX_CODEX_PROFILE" => Some("other".to_string()),
        "OPENAI_API_KEY" => Some("sk-leak".to_string()),
        "LEAN_CTX_CONFIG_DIR" => Some("/opt/lean".to_string()),
        _ => None,
    });
    assert_eq!(env.get("DO_NOT_TRACK").map(String::as_str), Some("1"));

    assert_eq!(env["PATH"], "/usr/bin:/bin");
    assert_eq!(env["TERM"], "xterm-256color");
    assert_eq!(env["HOME"], session.home.display().to_string());
    assert_eq!(env["CODEX_HOME"], session.codex_home.display().to_string());
    // Empty values are dropped rather than forwarded as empty strings.
    assert!(!env.contains_key("LANG"));
    // Profile selectors, API keys and lean-ctx client config never reach
    // the Codex client — the MCP server gets those separately.
    for forbidden in [
        "CODEX_PROFILE",
        "LEAN_CTX_CODEX_PROFILE",
        "OPENAI_API_KEY",
        "LEAN_CTX_CONFIG_DIR",
    ] {
        assert!(!env.contains_key(forbidden), "{forbidden} leaked to Codex");
    }
    for key in env.keys() {
        assert!(
            key == "HOME" || key == "CODEX_HOME" || CLIENT_ENV_PASSTHROUGH.contains(&key.as_str()),
            "unexpected client env key {key}"
        );
    }
}

// --- launch argv/env, via an isolated recording executable -----------

#[cfg(unix)]
fn recording_executable(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("recorder.sh");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {dir}/argv\n/usr/bin/env > {dir}/env\npwd > {dir}/cwd\nexit 7\n",
        dir = dir.display()
    );
    std::fs::write(&path, script).expect("write recorder");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
        .expect("chmod recorder");
    path
}

#[cfg(unix)]
#[test]
fn launch_sends_only_the_fixed_flags_and_a_scrubbed_environment() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical");
    let session = Session {
        home: root.join("home"),
        codex_home: root.join("codex"),
        workspace: root.join("workspace"),
    };
    for dir in [&session.home, &session.codex_home, &session.workspace] {
        std::fs::create_dir(dir).expect("session dir");
    }
    let recorder = recording_executable(&root);

    let env = client_env(&session, &|key| match key {
        // A real PATH is required for `/usr/bin/env` inside the recorder.
        "PATH" => std::env::var("PATH").ok(),
        "CODEX_PROFILE" => Some("leak".to_string()),
        "OPENAI_API_KEY" => Some("sk-leak".to_string()),
        _ => None,
    });

    let status = spawn_codex(&recorder, &session.workspace, &env).expect("spawn recorder");
    assert_eq!(status.code(), Some(7), "exit code must be propagated");

    let workspace_arg = session.workspace.display().to_string();
    let recorded_argv = std::fs::read_to_string(root.join("argv")).expect("argv");
    let lines: Vec<&str> = recorded_argv.lines().collect();
    assert_eq!(
        lines,
        vec![
            "--strict-config",
            "--no-daemon",
            "-C",
            workspace_arg.as_str(),
        ],
        "no extra, exec-only or pass-through flags may be sent"
    );

    let recorded_env = std::fs::read_to_string(root.join("env")).expect("env");
    for forbidden in ["CODEX_PROFILE", "OPENAI_API_KEY", "LEAN_CTX_"] {
        assert!(
            !recorded_env.contains(forbidden),
            "{forbidden} reached the child process:\n{recorded_env}"
        );
    }
    assert!(recorded_env.contains(&format!("HOME={}", session.home.display())));
    assert!(recorded_env.contains(&format!("CODEX_HOME={}", session.codex_home.display())));

    let recorded_cwd = std::fs::read_to_string(root.join("cwd")).expect("cwd");
    assert_eq!(
        recorded_cwd.trim(),
        session.workspace.display().to_string(),
        "the child starts in the empty workspace, never in the project"
    );
}

// --- provisioning and credentials ------------------------------------

// Provisioning validates the profile with macOS sandbox-exec; the protected
// Codex launcher exists only there.
#[cfg(target_os = "macos")]
#[test]
fn provisioning_writes_config_but_never_credentials() {
    let fixture = fixture();
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical");

    let session = provision(&fixture.pre, &root).expect("provision");

    assert!(session.codex_home.join("config.toml").is_file());
    assert!(
        !session.codex_home.join("auth.json").exists(),
        "provisioning (and therefore --check) must never place credentials"
    );
    assert!(
        std::fs::read_dir(&session.workspace)
            .expect("workspace")
            .next()
            .is_none(),
        "the working directory must start empty"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&session.codex_home)
            .expect("stat codex home")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        let mode = std::fs::metadata(session.codex_home.join("config.toml"))
            .expect("stat config")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn missing_credentials_point_at_codex_login() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("codex-home");
    let target = temp.path().join("session-codex");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::create_dir_all(&target).expect("target");

    let err = copy_auth(&source, &target).expect_err("missing auth must fail");
    assert!(err.contains("codex login"), "{err}");
    assert!(!err.contains("API key"), "{err}");
}

#[test]
fn credential_copy_is_bounded_and_owner_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("codex-home");
    let target = temp.path().join("session-codex");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::create_dir_all(&target).expect("target");
    // Synthetic, non-credential content.
    std::fs::write(source.join("auth.json"), "{\"synthetic\":true}").expect("auth");
    // Something that must NOT be copied along.
    std::fs::write(source.join("config.toml"), "model=\"whatever\"\n").expect("config");

    copy_auth(&source, &target).expect("copy");

    assert_eq!(
        std::fs::read_to_string(target.join("auth.json")).expect("copied"),
        "{\"synthetic\":true}"
    );
    assert!(
        !target.join("config.toml").exists(),
        "only auth.json may be copied"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(target.join("auth.json"))
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[cfg(unix)]
#[test]
fn credential_copy_refuses_symlinks_and_oversize_sources() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("codex-home");
    let target = temp.path().join("session-codex");
    std::fs::create_dir_all(&source).expect("source");
    std::fs::create_dir_all(&target).expect("target");

    std::fs::write(temp.path().join("elsewhere.json"), "{}").expect("elsewhere");
    std::os::unix::fs::symlink(temp.path().join("elsewhere.json"), source.join("auth.json"))
        .expect("symlink");
    let err = copy_auth(&source, &target).expect_err("symlink must be refused");
    assert!(err.contains("symlink"), "{err}");
    assert!(!target.join("auth.json").exists());

    std::fs::remove_file(source.join("auth.json")).expect("unlink");
    std::fs::write(
        source.join("auth.json"),
        vec![b'x'; usize::try_from(MAX_AUTH_BYTES).unwrap_or(usize::MAX) + 1],
    )
    .expect("oversize");
    let err = copy_auth(&source, &target).expect_err("oversize must be refused");
    assert!(err.contains("unexpected size"), "{err}");
    assert!(!target.join("auth.json").exists());
}

// --- version gate -----------------------------------------------------

#[test]
fn only_the_qualified_version_is_accepted() {
    assert!(verify_version(QUALIFIED_VERSION).is_ok());
    let err = verify_version("codex-cli 0.157.0").expect_err("other versions must fail closed");
    assert!(err.contains(QUALIFIED_VERSION), "{err}");
    assert!(err.contains("0.157.0"), "{err}");
    assert!(
        !err.to_lowercase().contains("protected against"),
        "the message must not claim protection it has not proven"
    );
}

#[cfg(unix)]
#[test]
fn version_probe_reads_the_executable_output() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let fake = temp.path().join("fake-codex");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\necho '{QUALIFIED_VERSION}'\nexit 0\n"),
    )
    .expect("write");
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).expect("chmod");

    assert_eq!(codex_version(&fake).expect("version"), QUALIFIED_VERSION);
    assert!(ensure_executable_file(&fake).is_ok());

    let plain = temp.path().join("not-executable");
    std::fs::write(&plain, "x").expect("write");
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    assert!(ensure_executable_file(&plain).is_err());
}

// --- session cleanup --------------------------------------------------

/// A provisioned-looking session root with a copied credential in it, plus
/// a sibling directory that cleanup must never touch.
fn session_root(temp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let root = temp.join("session");
    let codex_home = root.join("codex");
    std::fs::create_dir_all(&codex_home).expect("session dirs");
    std::fs::write(codex_home.join("auth.json"), b"{\"token\":\"x\"}").expect("auth");
    std::fs::write(codex_home.join("state.sqlite-wal"), b"wal").expect("scratch");

    let neighbour = temp.join("neighbour");
    std::fs::create_dir(&neighbour).expect("neighbour");
    std::fs::write(neighbour.join("keep"), b"keep").expect("neighbour file");

    (root, codex_home, neighbour)
}

fn refilled() -> std::io::Error {
    std::io::Error::from(std::io::ErrorKind::DirectoryNotEmpty)
}

#[test]
fn cleanup_retries_while_the_tree_is_still_being_filled() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (root, _, _) = session_root(temp.path());
    let calls = std::cell::Cell::new(0_u32);

    let outcome = close_session_root_with(
        &root,
        6,
        Duration::ZERO,
        |path: &Path| -> std::io::Result<()> {
            assert_eq!(path, root, "only the session root may be removed");
            calls.set(calls.get() + 1);
            // A descendant is still writing for the first two sweeps.
            if calls.get() < 3 {
                Err(refilled())
            } else {
                std::fs::remove_dir_all(path)
            }
        },
    );

    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(!root.exists());
    assert_eq!(calls.get(), 3, "the retry must stop as soon as it wins");
}

#[test]
fn cleanup_reports_a_tree_that_never_drains() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (root, codex_home, _) = session_root(temp.path());
    let calls = std::cell::Cell::new(0_u32);

    let outcome = close_session_root_with(
        &root,
        4,
        Duration::ZERO,
        |_: &Path| -> std::io::Result<()> {
            calls.set(calls.get() + 1);
            Err(refilled())
        },
    );

    // Bounded: the retry gives up and the failure is reported, never slept
    // away and never swallowed.
    assert_eq!(calls.get(), 4, "the retry budget must be a hard bound");
    assert!(tree_refilled(
        &outcome.expect_err("persistent residue must fail")
    ));
    assert!(codex_home.join("auth.json").exists());
}

#[test]
fn cleanup_does_not_retry_an_error_that_retrying_cannot_fix() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (root, _, _) = session_root(temp.path());
    let calls = std::cell::Cell::new(0_u32);

    let outcome = close_session_root_with(
        &root,
        8,
        Duration::ZERO,
        |_: &Path| -> std::io::Result<()> {
            calls.set(calls.get() + 1);
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        },
    );

    assert_eq!(calls.get(), 1, "a permanent error must fail on the spot");
    assert_eq!(
        outcome.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
}

#[cfg(unix)]
#[test]
fn cleanup_does_not_follow_a_replaced_codex_home() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (root, codex_home, neighbour) = session_root(temp.path());
    std::fs::write(neighbour.join("auth.json"), "must survive").unwrap();
    std::fs::remove_dir_all(&codex_home).unwrap();
    std::os::unix::fs::symlink(&neighbour, &codex_home).unwrap();
    close_session_root(&root).unwrap();
    assert!(!root.exists());
    assert_eq!(
        std::fs::read_to_string(neighbour.join("auth.json")).unwrap(),
        "must survive"
    );
    assert!(neighbour.join("keep").is_file(), "a sibling was touched");
}

#[test]
fn cleanup_rejects_zero_attempts_and_only_accepts_an_absent_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let (root, _, _) = session_root(temp.path());
    assert!(close_session_root_with(&root, 0, Duration::ZERO, remove_tree).is_err());
    assert!(root.exists());
    let missing = |_: &Path| Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    assert!(close_session_root_with(&root, 1, Duration::ZERO, missing).is_err());
    std::fs::remove_dir_all(&root).unwrap();
    close_session_root(&root).unwrap();
}
