// SPDX-License-Identifier: Apache-2.0
//! Rights per session: a Developer cannot touch the locked tests (ctx write
//! path, native hook, shell), `passes` is the gate's alone, a normal edit in
//! the own worktree passes, a Code Reviewer writes nothing, what cannot be
//! analysed is refused, and a broken rights file refuses everything. Each
//! repro of review r1 (F1–F7, notes 1 and 2) is a case here.

use super::*;
use serde_json::json;

fn profile(role: &str) -> serde_json::Value {
    match role {
        "developer" => json!({"id":"developer","name":"Developer","stages":["build"],
            "delivers":"commits in its own worktree","read":["repo","vault"],
            "write":["own_worktree"],"deny_write":["locked","spec","tests"],
            "may_stop":false,"availability":"now"}),
        "code_reviewer" => json!({"id":"code_reviewer","name":"Code Reviewer","stages":["review"],
            "delivers":"blind rubric verdict (G7)","read":["candidate_diff","spec"],
            "write":["review_records"],"deny_write":["locked","own_worktree"],
            "may_stop":true,"availability":"now"}),
        "spec_writer" => json!({"id":"spec_writer","name":"Spec Writer","stages":["spec"],
            "delivers":"acceptance criteria, feature list, locked tests","read":["repo","vault"],
            "write":["spec","tests","locked"],"deny_write":[],"may_stop":false,"availability":"now"}),
        other => panic!("no fixture profile {other}"),
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    worktree: PathBuf,
    rights: PathBuf,
}

const LOCKED: &str = "crates/pay/tests/refund.rs";

fn feature_list(after_g2: bool) -> serde_json::Value {
    let mut list = json!({
      "schema": "leanctx.hive.process.feature-list/v1",
      "process": "1.0",
      "task": {"vault": "#acme/payments", "number": 42},
      "test_command": "cargo test -p pay refund",
      "locked_paths": [LOCKED, "specs/42/features.json", "crates/pay/tests/golden/**"],
      "features": [
        {"id": "F1", "description": "over capture refused", "steps": ["capture 10.00", "refund 10.01"],
         "tests": ["crates/pay/tests/refund.rs::refuses_over_capture"], "passes": false}
      ]
    });
    if after_g2 {
        list["spec_commit"] = json!("1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d");
    }
    list
}

fn fixture_with(profile: &serde_json::Value, list: &serde_json::Value) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path().join("wt");
    for sub in [
        "crates/pay/src",
        "crates/pay/tests/golden",
        "crates/pay/tests/sub",
        "specs/42",
    ] {
        std::fs::create_dir_all(worktree.join(sub)).unwrap();
    }
    std::fs::write(worktree.join("crates/pay/src/lib.rs"), "fn a() {}\n").unwrap();
    std::fs::write(worktree.join(LOCKED), "#[test] fn t() {}\n").unwrap();
    std::fs::write(worktree.join("crates/pay/tests/golden/a.json"), "{}\n").unwrap();
    std::fs::write(worktree.join("specs/42/features.json"), list.to_string()).unwrap();
    let wt = worktree.to_string_lossy().to_string();
    let record = json!({
        "schema": SCHEMA,
        "process": "1.0",
        "profile": profile,
        "bindings": {
            "own_worktree": [wt],
            "spec": [format!("{wt}/specs/42")],
            "tests": [format!("{wt}/crates/pay/tests")],
        },
        "feature_list": "specs/42/features.json",
        "denied_tools": ["NotebookEdit", "ctx_refactor"],
    });
    let rights = dir.path().join("rights.json");
    std::fs::write(&rights, serde_json::to_string(&record).unwrap()).unwrap();
    set_private(&rights);
    Fixture {
        _dir: dir,
        worktree,
        rights,
    }
}

/// The same fixture with `base_ref` bound in its rights file.
fn with_base_ref(fx: Fixture, base: &str) -> Fixture {
    with_field(fx, "base_ref", base)
}

/// The same fixture with one more top-level field in its rights file.
fn with_field(fx: Fixture, key: &str, value: &str) -> Fixture {
    let mut record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&fx.rights).unwrap()).unwrap();
    record[key] = json!(value);
    std::fs::write(&fx.rights, record.to_string()).unwrap();
    set_private(&fx.rights);
    fx
}

fn fixture(role: &str, after_g2: bool) -> Fixture {
    fixture_with(&profile(role), &feature_list(after_g2))
}

fn set_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let _ = path;
}

fn state(fx: &Fixture) -> State {
    let state = from_file_arg(Some(&fx.rights.to_string_lossy()));
    assert!(matches!(state, State::Active(_)), "{state:?}");
    state
}

fn rule_for(what: &str, result: Result<(), String>) -> String {
    match result {
        Ok(()) => panic!("expected a refusal for: {what}"),
        Err(err) => {
            assert!(err.contains("rule "), "a refusal names its rule: {err}");
            err
        }
    }
}

fn rule(result: Result<(), String>) -> String {
    let err = result.expect_err("expected a refusal");
    assert!(err.contains("rule "), "a refusal names its rule: {err}");
    err
}

fn bash(s: &State, fx: &Fixture, command: &str) -> Result<(), String> {
    s.check_native_tool("Bash", &json!({"command": command}), &fx.worktree)
}

#[test]
fn session_rights_developer_ctx_write_to_locked_test_is_refused() {
    let fx = fixture("developer", true);
    let s = state(&fx);
    let err = rule(s.check_write(&fx.worktree.join(LOCKED)));
    assert!(err.contains("feature_list_locked"), "{err}");
    assert!(err.contains("Developer"), "{err}");
    let err = rule(s.check_write(&fx.worktree.join("crates/pay/tests/golden/new.json")));
    assert!(err.contains("feature_list_locked"), "glob lock: {err}");
    // The ctx write tools all end in `pathjail::enforce_writable`.
    let err = sync_scope(s.clone(), || {
        crate::core::pathjail::enforce_writable(&fx.worktree.join(LOCKED))
    })
    .unwrap_err();
    assert!(err.contains("feature_list_locked"), "{err}");
}

#[test]
fn session_rights_developer_before_g2_is_denied_by_its_profile() {
    let fx = fixture("developer", false);
    let s = state(&fx);
    let err = rule(s.check_write(&fx.worktree.join(LOCKED)));
    assert!(err.contains("deny_write:locked"), "{err}");
}

#[test]
fn session_rights_developer_normal_work_in_own_worktree_passes() {
    let fx = fixture("developer", true);
    let s = state(&fx);
    s.check_write(&fx.worktree.join("crates/pay/src/lib.rs"))
        .unwrap();
    s.check_write(&fx.worktree.join("crates/pay/src/new_module.rs"))
        .unwrap();
    sync_scope(s.clone(), || {
        crate::core::pathjail::enforce_writable(&fx.worktree.join("crates/pay/src/lib.rs"))
    })
    .unwrap();
    s.check_native_tool(
        "Edit",
        &json!({"file_path": fx.worktree.join("crates/pay/src/lib.rs")}),
        &fx.worktree,
    )
    .unwrap();
    for command in [
        "cargo fmt 2>&1 > crates/pay/src/out.txt; echo ok >/dev/null",
        "cargo test -p pay --lib refund",
        "python3 tools/check.py --fast",
        "npm run build",
        "git status && git diff HEAD~1 | head -50",
        "git add crates/pay/src/lib.rs && git commit -m 'feat: refund'",
        "git commit -m \"$(cat <<'EOF'\nfix(pay): keep (1) rest) > nothing\nEOF\n)\"",
        "git fetch origin",
        "git diff --output=crates/pay/src/d.patch HEAD~1",
        "git config --get user.name",
        "git format-patch -o crates/pay/src HEAD~1",
        "python3 -W ignore tools/x.py",
        "bash -o pipefail scripts/x.sh",
        "node -r ts-node/register src/x.ts",
        "setsid cargo build",
        "nice -n 5 cargo test",
        "git checkout -b feat/refund",
        "git restore --staged crates/pay/src/lib.rs",
        "git reset HEAD~1",
        "cat crates/pay/tests/refund.rs | grep t > crates/pay/src/x.txt",
        "sed -n 1,5p crates/pay/tests/refund.rs",
        "sed -i 's/a/b/' crates/pay/src/lib.rs",
        "python3 --version",
        "find crates -name '*.rs' -print",
        "tar czf /dev/null crates/pay/src",
    ] {
        bash(&s, &fx, command).unwrap_or_else(|e| panic!("{command}: {e}"));
    }
}

#[test]
fn session_rights_developer_outside_worktree_and_tests_dir_refused() {
    let fx = fixture("developer", false);
    let s = state(&fx);
    let outside = fx.worktree.parent().unwrap().join("other/file.rs");
    assert!(rule(s.check_write(&outside)).contains("write_scope"));
    let err = rule(s.check_write(&fx.worktree.join("crates/pay/tests/new_test.rs")));
    assert!(err.contains("deny_write:tests"), "{err}");
    let sneaky = fx.worktree.join("crates/pay/src/../tests/refund.rs");
    assert!(rule(s.check_write(&sneaky)).contains("deny_write:"));
}

#[test]
fn session_rights_native_hook_refuses_locked_test_edits() {
    let fx = fixture("developer", true);
    let s = state(&fx);
    let locked = fx.worktree.join(LOCKED);
    for tool in ["Edit", "Write", "MultiEdit"] {
        let err = rule(s.check_native_tool(tool, &json!({"file_path": locked}), &fx.worktree));
        assert!(err.contains("feature_list_locked"), "{tool}: {err}");
    }
    let rel = rule(s.check_native_tool("Write", &json!({"file_path": LOCKED}), &fx.worktree));
    assert!(rel.contains("feature_list_locked"), "{rel}");
    let err = rule(s.check_native_tool(
        "NotebookEdit",
        &json!({"notebook_path": fx.worktree.join("n.ipynb")}),
        &fx.worktree,
    ));
    assert!(err.contains("denied_tools"), "{err}");
    assert!(rule(s.check_native_tool("Write", &json!({}), &fx.worktree)).contains("no target"));
}

/// F1: a `..` after a symlink, a symlink or hard link to a locked file.
#[cfg(unix)]
#[test]
fn session_rights_links_cannot_reach_locked_files() {
    let fx = fixture("developer", true);
    let s = state(&fx);
    // Creating links whose source is locked or in a denied scope.
    for command in [
        "ln -s crates/pay/tests/sub s",
        "ln crates/pay/tests/refund.rs hl.rs",
        "ln -sf ../tests/refund.rs crates/pay/src/l.rs",
        "cp -l crates/pay/tests/refund.rs crates/pay/src/l.rs && echo x >> crates/pay/src/l.rs",
    ] {
        rule_for(command, bash(&s, &fx, command));
    }
    // A link that already exists: `s` -> tests/sub, so `s/..` is tests/.
    std::os::unix::fs::symlink(
        fx.worktree.join("crates/pay/tests/sub"),
        fx.worktree.join("s"),
    )
    .unwrap();
    let err = rule(bash(&s, &fx, "echo x > s/../refund.rs"));
    assert!(err.contains("feature_list_locked"), "{err}");
    let through = format!("{}/s/../refund.rs", fx.worktree.display());
    let err = rule(s.check_native_tool("Edit", &json!({"file_path": through}), &fx.worktree));
    assert!(err.contains("feature_list_locked"), "{err}");
    // A hard link that already exists is the locked inode.
    std::fs::hard_link(
        fx.worktree.join(LOCKED),
        fx.worktree.join("crates/pay/src/hl.rs"),
    )
    .unwrap();
    let s = state(&fx);
    let err = rule(bash(&s, &fx, "echo x >> crates/pay/src/hl.rs"));
    assert!(err.contains("feature_list_locked"), "{err}");
    let err = rule(s.check_write(&fx.worktree.join("crates/pay/src/hl.rs")));
    assert!(err.contains("feature_list_locked"), "{err}");
}

/// F2: git and patch commands that rewrite the worktree.
#[test]
fn session_rights_git_rewrites_and_patch_are_refused() {
    let fx = fixture("developer", true);
    let s = state(&fx);
    for command in [
        "git checkout HEAD~1 crates/pay/tests/refund.rs",
        "git checkout HEAD~1 .",
        "git checkout HEAD~1 -- crates/pay/tests/refund.rs",
        "git checkout -- .",
        "git switch main",
        "git restore crates/pay/tests/refund.rs",
        "git restore .",
        "git reset --hard HEAD~1",
        "git stash",
        "git stash pop",
        "git apply x.diff",
        "git am x.mbox",
        "git cherry-pick abc123",
        "git revert abc123",
        "git clean -fdx",
        "git rebase origin/main",
        "git co main",
        "git -C ../other checkout main",
        "patch -p1 < x.diff",
        "git rm crates/pay/tests/refund.rs",
        "git mv crates/pay/tests/refund.rs x.rs",
    ] {
        rule_for(command, bash(&s, &fx, command));
    }
    let err = rule(bash(&s, &fx, "git reset --hard HEAD~1"));
    assert!(err.contains("git_rewrite"), "{err}");
    let err = rule(bash(&s, &fx, "patch -p1 < x.diff"));
    assert!(err.contains("unanalysable"), "{err}");
}

/// F3: nested shells, interpreters and indirect writers.
#[test]
fn session_rights_unanalysable_commands_are_refused() {
    let fx = fixture("developer", true);
    let s = state(&fx);
    for command in [
        "bash -c 'echo x > crates/pay/tests/refund.rs'",
        "sh -c 'echo x > crates/pay/tests/refund.rs'",
        "zsh -lc true",
        "eval 'echo x > crates/pay/tests/refund.rs'",
        "source ./x.sh",
        ". ./x.sh",
        "python3 -c \"open('crates/pay/tests/refund.rs','w')\"",
        "python3 - <<'PY'\nopen('crates/pay/tests/refund.rs','w')\nPY",
        "python3 <<PY\nopen('x','w')\nPY",
        "cat x.py | python3",
        "perl -pi -e 's/a/b/' crates/pay/src/lib.rs",
        "node -e 'require(\"fs\").writeFileSync(\"x\",\"\")'",
        "ruby -e 'File.write(\"x\",\"\")'",
        "awk -i inplace 1 crates/pay/tests/refund.rs",
        "awk '{print > \"crates/pay/tests/refund.rs\"}' x",
        "echo crates/pay/tests/refund.rs | xargs rm",
        "find crates -name '*.rs' -delete",
        "find crates -exec rm {} +",
        "tar xf a.tar",
        "tar -xzf a.tgz",
        "unzip a.zip",
        "pushd crates/pay/tests && echo x > refund.rs",
        "echo x > {crates/pay/tests,x}/refund.rs",
        "vim -c ':wq' crates/pay/src/lib.rs",
        "sed -n 'w crates/pay/tests/refund.rs' crates/pay/src/lib.rs",
        "sed 's/a/b/w crates/pay/tests/refund.rs' crates/pay/src/lib.rs",
        "sed -e '1e rm -rf crates' crates/pay/src/lib.rs",
        "timeout 5 bash -c 'true'",
        "env -i sh -c 'true'",
        "echo $(rm crates/pay/tests/refund.rs)",
        "echo `rm crates/pay/tests/refund.rs`",
        "diff <(cat a) >(tee crates/pay/tests/refund.rs)",
        "cat <<EOF > crates/pay/src/notes.txt\n$(rm crates/pay/tests/refund.rs)\nEOF",
        "(cd crates/pay; rm tests/refund.rs)",
        "cd crates/pay && rm tests/refund.rs",
        "cd $DIR && rm refund.rs",
        "echo x > \"$TARGET\"",
        "curl -O https://example.com/x",
    ] {
        rule_for(command, bash(&s, &fx, command));
    }
    let err = rule(bash(&s, &fx, "bash -c 'true'"));
    assert!(err.contains("unanalysable"), "{err}");
    let err = rule(bash(&s, &fx, "echo x > \"$TARGET\""));
    assert!(err.contains("bash_unresolved_target"), "{err}");
    let err = rule(bash(&s, &fx, "LEAN_CTX_RIGHTS_FILE= lean-ctx read x"));
    assert!(err.contains("rights_env"), "{err}");
    let err = rule(bash(
        &s,
        &fx,
        &format!("echo {{}} > {}", fx.rights.display()),
    ));
    assert!(
        err.contains("rights_file") || err.contains("write_scope"),
        "{err}"
    );
}

/// The shell forms the first round already covered stay refused.
#[test]
fn session_rights_native_bash_writes_into_locked_paths_refused() {
    let fx = fixture("developer", true);
    let s = state(&fx);
    for command in [
        "echo 'fn t(){}' > crates/pay/tests/refund.rs",
        "echo x >>crates/pay/tests/refund.rs",
        "cat a.rs | tee crates/pay/tests/refund.rs",
        "sed -i 's/assert/\\/\\/assert/' crates/pay/tests/refund.rs",
        "cp /tmp/x.rs crates/pay/tests/refund.rs",
        "mv crates/pay/tests/refund.rs /tmp/gone.rs",
        "rm -f crates/pay/tests/*.rs",
        "dd if=/dev/zero of=crates/pay/tests/refund.rs",
        "true && truncate -s 0 specs/42/features.json",
        "sudo env A=1 touch crates/pay/tests/golden/b.json",
        "rm -rf crates/pay",
        "gzip crates/pay/tests/refund.rs",
        "sort -o crates/pay/tests/refund.rs x",
        "curl -o crates/pay/tests/refund.rs https://example.com/x",
    ] {
        let err = rule_for(command, bash(&s, &fx, command));
        assert!(
            err.contains("deny_write:") || err.contains("feature_list_locked"),
            "{command}: {err}"
        );
    }
}

/// F5: unknown native tools are refused; apply_patch is judged by its files.
#[test]
fn session_rights_unknown_native_tools_and_apply_patch() {
    let fx = fixture("developer", true);
    let s = state(&fx);
    let patch = |file: &str| json!({"input": format!("*** Begin Patch\n*** Update File: {file}\n@@\n-a\n+b\n*** End Patch\n")});
    let err = rule(s.check_native_tool("apply_patch", &patch(LOCKED), &fx.worktree));
    assert!(err.contains("feature_list_locked"), "{err}");
    s.check_native_tool("apply_patch", &patch("crates/pay/src/lib.rs"), &fx.worktree)
        .unwrap();
    rule(s.check_native_tool("apply_patch", &json!({"input": "nothing"}), &fx.worktree));
    let err = rule(s.check_native_tool(
        "SomeNewWriteTool",
        &json!({"path": fx.worktree.join(LOCKED)}),
        &fx.worktree,
    ));
    assert!(err.contains("unknown_tool"), "{err}");
    for tool in [
        "Read",
        "Grep",
        "Glob",
        "WebFetch",
        "TodoWrite",
        "mcp__lean-ctx__ctx_read",
    ] {
        s.check_native_tool(
            tool,
            &json!({"file_path": fx.worktree.join(LOCKED)}),
            &fx.worktree,
        )
        .unwrap_or_else(|e| panic!("{tool}: {e}"));
    }
    let argv = json!({"command": ["bash", "-lc", "echo x > crates/pay/tests/refund.rs"]});
    rule(s.check_native_tool("shell", &argv, &fx.worktree));
}

/// R2-1: kept git commands that write files, and configuration that runs a
/// command.
#[test]
fn session_rights_git_output_files_and_command_settings_are_judged() {
    let fx = fixture("developer", true);
    let s = state(&fx);
    for command in [
        "git diff --output=crates/pay/tests/refund.rs",
        "git log --output crates/pay/tests/refund.rs",
        "git format-patch -o crates/pay/tests HEAD~1",
        "git archive -o crates/pay/tests/refund.rs HEAD",
    ] {
        let err = rule_for(command, bash(&s, &fx, command));
        assert!(
            err.contains("feature_list_locked") || err.contains("deny_write"),
            "{command}: {err}"
        );
    }
    for command in [
        "git config --file crates/pay/tests/refund.rs a.b c",
        "git config diff.external ./x.sh",
        "git -c core.pager=./x.sh log",
        "git -c diff.external=./x.sh diff",
        "git --config-env=core.pager=X log",
        "git --exec-path=/tmp/x status",
        "git fetch --upload-pack='sh -c x' ../repo",
        "git fetch 'ext::sh -c x'",
        "git grep -Ovim pattern",
        "git clone -c core.hooksPath=/tmp/h ../src crates/pay/src/c",
        "git clone --config core.fsmonitor=./x ../src crates/pay/src/c",
        "git clone --config=core.fsmonitor=./x ../src crates/pay/src/c",
        "git clone --template=/tmp/t ../src crates/pay/src/c",
        "git clone -u 'sh -c x' ../src crates/pay/src/c",
        "git clone -ush ../src crates/pay/src/c",
        "git clone -ccore.hooksPath=/tmp/h ../src crates/pay/src/c",
        "git clone --conf=core.fsmonitor=./x ../src crates/pay/src/c",
        "git clone --temp=/tmp/t ../src crates/pay/src/c",
        "git clone --sep=/tmp/g ../src crates/pay/src/c",
        "git ls-remote -u 'sh -c x' ../src",
        "git fetch --upload-p='sh -c x' ../repo",
        "git push --receive-pack=./x origin",
        "git push --exe=./x origin",
        "git archive --remote=x --ex=./y HEAD",
        "git fetch --refmap= ../repo",
        "git fetch --refm=+a:b ../repo",
        "git fetch fd::3",
    ] {
        let err = rule_for(command, bash(&s, &fx, command));
        assert!(err.contains("git_config"), "{command}: {err}");
    }
    let err = rule_for("format-patch", bash(&s, &fx, "git format-patch HEAD~1"));
    assert!(err.contains("git_rewrite"), "{err}");
    for command in [
        "GIT_EXTERNAL_DIFF=./x.sh git diff",
        "PAGER='sh -c x' git log",
        "GIT_SSH_COMMAND=x git fetch",
        "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.pager GIT_CONFIG_VALUE_0=x git log",
        "export GIT_PAGER=x",
        "env GIT_EDITOR=x git commit",
        "declare GIT_PAGER=x",
        "typeset PAGER=x",
        "GIT_DIR=../other/.git git status",
        "GIT_WORK_TREE=../other git status",
    ] {
        let err = rule_for(command, bash(&s, &fx, command));
        assert!(err.contains("unanalysable"), "{command}: {err}");
    }
}

/// Run git in `dir` with a fixed identity and no user configuration.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// R2-2/R3-2: merge brings in only the bound base commit, by its sha or a
/// name resolving to it now; pull, remote changes, refspecs and moving the
/// base line's name are refused.
#[test]
fn session_rights_merge_only_of_the_base_commit() {
    let fx = fixture("developer", true);
    git(&fx.worktree, &["init", "-q", "-b", "work"]);
    git(&fx.worktree, &["add", "-A"]);
    git(&fx.worktree, &["commit", "-q", "-m", "base"]);
    let base = git(&fx.worktree, &["rev-parse", "HEAD"]);
    git(&fx.worktree, &["branch", "base-line"]);
    std::fs::write(fx.worktree.join("crates/pay/src/other.rs"), "fn b() {}\n").unwrap();
    git(&fx.worktree, &["add", "-A"]);
    git(&fx.worktree, &["commit", "-q", "-m", "other"]);
    git(&fx.worktree, &["branch", "other-line"]);

    let s = state(&fx);
    let err = rule_for("no base", bash(&s, &fx, &format!("git merge {base}")));
    assert!(err.contains("git_rewrite"), "{err}");

    let fx = with_field(with_base_ref(fx, "base-line"), "base_commit", &base);
    let s = state(&fx);
    for command in [
        format!("git merge {base}"),
        format!("git merge --no-ff -m 'merge the base line' {base}"),
        "git merge base-line".to_string(),
        "git merge --abort".to_string(),
        format!("git fetch origin && git merge {base}"),
        "git fetch origin".to_string(),
        "git remote -v".to_string(),
    ] {
        bash(&s, &fx, &command).unwrap_or_else(|e| panic!("{command}: {e}"));
    }
    for command in [
        "git merge other-line".to_string(),
        "git merge FETCH_HEAD".to_string(),
        "git fetch ../other && git merge FETCH_HEAD".to_string(),
        "git fetch ../other && git merge base-line".to_string(),
        "git tag base-line FETCH_HEAD".to_string(),
        "git branch base-line FETCH_HEAD".to_string(),
        "git branch -f base-line other-line".to_string(),
        "git fetch ../other +HEAD:refs/remotes/base-line".to_string(),
        "git remote set-url origin ../other".to_string(),
        "git remote add evil ../other".to_string(),
        "git remote rename origin old".to_string(),
        format!("git merge {base} other-line"),
        "git pull".to_string(),
        "git pull origin base-line".to_string(),
        "git pull ../repo main".to_string(),
        // R4-1 bis: pushing to a local path runs that repo's receive hooks.
        "git push .".to_string(),
        "git push ../other HEAD".to_string(),
        "git push /tmp/bare main".to_string(),
        "git push file:///tmp/bare main".to_string(),
        "git push --repo /tmp/bare main".to_string(),
        "git push --repo=/tmp/bare main".to_string(),
        "git push --rep=/tmp/bare main".to_string(),
        "git fetch origin --refmap=x +a:b".to_string(),
    ] {
        let err = rule_for(&command, bash(&s, &fx, &command));
        assert!(
            err.contains("git_rewrite") || err.contains("git_config"),
            "{command}: {err}"
        );
    }
    // A push to a named remote is fine.
    bash(&s, &fx, "git push origin HEAD").unwrap();
    bash(&s, &fx, "git push").unwrap();
    // R4-1 bis: a replace ref can make base-line resolve to other content.
    git(&fx.worktree, &["replace", &base, "other-line"]);
    let err = rule_for(
        "merge with replace ref",
        bash(&s, &fx, "git merge base-line"),
    );
    assert!(err.contains("git_rewrite"), "{err}");
    git(&fx.worktree, &["replace", "-d", &base]);
    // R5-1: a cd this check cannot resolve refuses even the bound sha.
    let err = rule_for(
        "cd unknown then merge",
        bash(&s, &fx, &format!("cd \"$D\" && git merge {base}")),
    );
    assert!(err.contains("git_rewrite"), "{err}");
    // R5-1: a prefix assignment of a namespace variable is refused.
    for var in [
        "GIT_REPLACE_REF_BASE",
        "GIT_GRAFT_FILE",
        "GIT_DIR",
        "GIT_COMMON_DIR",
    ] {
        let err = rule_for(
            var,
            bash(&s, &fx, &format!("{var}=/tmp/x git merge {base}")),
        );
        assert!(err.contains("unanalysable"), "{var}: {err}");
    }
    // R5-1: when git cannot tell (the cwd is not a repo), the merge is
    // refused, not allowed. A fresh fixture has no .git.
    let no_repo = fixture("developer", true);
    let no_repo = with_field(with_base_ref(no_repo, "base-line"), "base_commit", &base);
    let s2 = state(&no_repo);
    let err = rule_for(
        "merge where git cannot tell",
        bash(&s2, &no_repo, &format!("git merge {base}")),
    );
    assert!(err.contains("git_rewrite"), "{err}");
    for (key, bad) in [
        ("base_ref", "FETCH_HEAD"),
        ("base_ref", "HEAD"),
        ("base_ref", "main@{1}"),
        ("base_ref", "main~1"),
        ("base_ref", "main^"),
        ("base_ref", "a..b"),
        ("base_commit", "abc123"),
    ] {
        let fx = with_field(fixture("developer", true), key, bad);
        assert!(
            matches!(
                from_file_arg(Some(&fx.rights.to_string_lossy())),
                State::Unavailable(_)
            ),
            "{key}={bad}"
        );
    }
}

/// R2-3: inline code behind option values and wrappers.
#[test]
fn session_rights_inline_code_behind_options_and_wrappers_is_refused() {
    let fx = fixture("developer", true);
    let s = state(&fx);
    for command in [
        "bash -o pipefail -c 'echo x > crates/pay/tests/refund.rs'",
        "bash -O extglob -c 'true'",
        "python3 -W ignore -c 'open(\"x\",\"w\")'",
        "python3 -X dev -c 'pass'",
        "python3 -x -c 'pass'",
        "node -r mod -e 'x'",
        "ruby -I lib -e 'x'",
        "perl -d -e 'x'",
        "setsid bash -c 'true'",
        "flock /tmp/f -c 'true'",
        "su -c 'true'",
        "script -c 'true'",
        "watch 'echo x > crates/pay/tests/refund.rs'",
        "chrt 5 bash -c 'true'",
        "taskset 1 bash -c 'true'",
        "taskset -c 0 python3 -c 'pass'",
        "strace -o f ls",
        "nice -n 5 bash -c 'true'",
        "alias ls=rm",
        "chroot / rm x",
        "nsenter -t 1 sh",
        "ltrace ls",
        "cat x.py | uv run python3",
        "uv run bash <<EOF\necho x > crates/pay/tests/refund.rs\nEOF",
        "poetry run python3 -c 'x'",
        "pipenv run bash -c 'x'",
        "npx bash -c 'x'",
        "echo 'x' | npx node",
        "pnpm dlx bash -c 'x'",
        "bunx python3 -c 'x'",
        "cat s.sh | xvfb-run bash",
        "unshare -w crates/pay/tests rm refund.rs",
        "numactl --bogus rm x",
        "sudo -s",
        "time -o crates/pay/tests/refund.rs ls",
        "flock /tmp/f -c 'true'",
        "php -B 'x' -R 'y'",
        "php -E 'x'",
        "julia -E 'x'",
        "perl -M'strict;system(1)' x.pl",
        "node --import=data:text/javascript,x x.js",
        "node --require data:text/javascript,x x.js",
    ] {
        let err = rule_for(command, bash(&s, &fx, command));
        assert!(err.contains("unanalysable"), "{command}: {err}");
    }
    for command in [
        "setsid rm crates/pay/tests/refund.rs",
        "numactl -C 0 rm crates/pay/tests/refund.rs",
        "xvfb-run -s '-screen 0 1x1x8' rm crates/pay/tests/refund.rs",
        "fakeroot -i state rm crates/pay/tests/refund.rs",
        "uv run rm crates/pay/tests/refund.rs",
    ] {
        let err = rule_for(command, bash(&s, &fx, command));
        assert!(err.contains("feature_list_locked"), "{command}: {err}");
    }
    for command in [
        "uv run pytest -q",
        "poetry run pytest",
        "npx prettier --check .",
        "numactl -C 0 cargo test",
        "command -v python3",
        "which python3",
        "echo hi | python3 tools/x.py",
        "perl -Mstrict tools/x.pl",
    ] {
        bash(&s, &fx, command).unwrap_or_else(|e| panic!("{command}: {e}"));
    }
}

/// F6: a profiled session may not reach the daemon socket itself.
#[test]
fn session_rights_direct_daemon_access_is_refused() {
    let fx = fixture("developer", true);
    let s = state(&fx);
    for command in [
        "curl --unix-socket ~/.lean-ctx/daemon.sock -d '{}' http://x/v1/tools/call",
        "curl -s http://127.0.0.1:8080/v1/tools/call -d '{}'",
        "socat - UNIX-CONNECT:/run/x.sock",
        "nc -U /run/x.sock",
    ] {
        let err = rule_for(command, bash(&s, &fx, command));
        assert!(err.contains("daemon_socket"), "{command}: {err}");
    }
}

#[test]
fn session_rights_feature_list_after_g2_refused_for_every_role() {
    for role in ["developer", "spec_writer", "code_reviewer"] {
        let fx = fixture(role, true);
        let s = state(&fx);
        let list = fx.worktree.join("specs/42/features.json");
        let err = rule(s.check_write(&list));
        assert!(err.contains("feature_list_locked"), "{role}: {err}");
        let err = rule(s.check_native_tool("Edit", &json!({"file_path": list}), &fx.worktree));
        assert!(err.contains("feature_list_locked"), "{role}: {err}");
    }
}

/// Note 1 (§9): after G2 the Spec Writer no longer writes locked tests; it
/// still writes spec files that are not locked.
#[test]
fn session_rights_spec_writer_loses_locked_paths_at_g2() {
    let fx = fixture("spec_writer", false);
    let s = state(&fx);
    s.check_write(&fx.worktree.join("specs/42/features.json"))
        .unwrap();
    s.check_write(&fx.worktree.join(LOCKED)).unwrap();
    assert!(
        rule(s.check_write(&fx.worktree.join("crates/pay/src/lib.rs"))).contains("write_scope")
    );
    let fx = fixture("spec_writer", true);
    let s = state(&fx);
    let err = rule(s.check_write(&fx.worktree.join(LOCKED)));
    assert!(err.contains("feature_list_locked"), "{err}");
    s.check_write(&fx.worktree.join("specs/42/notes.md"))
        .unwrap();
}

/// Note 2: on a case-insensitive file system another spelling of a locked
/// path is that path.
#[test]
fn session_rights_case_insensitive_file_systems_fold_locked_paths() {
    let fx = fixture("developer", true);
    let mut rights = load(&fx.rights).unwrap();
    rights.set_fold_case(true);
    let s = State::Active(Arc::new(rights));
    let err = rule(s.check_write(&fx.worktree.join("Crates/Pay/Tests/Refund.rs")));
    assert!(err.contains("feature_list_locked"), "{err}");
    let err = rule(s.check_write(&fx.worktree.join("SPECS/42/Features.JSON")));
    assert!(err.contains("feature_list_locked"), "{err}");
    s.check_write(&fx.worktree.join("Crates/Pay/Src/Lib.rs"))
        .unwrap();
}

#[test]
fn session_rights_code_reviewer_profile_is_read_only() {
    let fx = fixture("code_reviewer", true);
    let s = state(&fx);
    let State::Active(rights) = &s else {
        panic!("expected an active profile");
    };
    assert!(rights.may_stop());
    assert_eq!(rights.profile().id, "code_reviewer");
    for path in [
        "crates/pay/src/lib.rs",
        "crates/pay/src/new.rs",
        LOCKED,
        "README.md",
    ] {
        let err = rule(s.check_write(&fx.worktree.join(path)));
        assert!(
            err.contains("deny_write:") || err.contains("write_scope") || err.contains("locked"),
            "{path}: {err}"
        );
        assert!(
            s.check_native_tool(
                "Write",
                &json!({"file_path": fx.worktree.join(path)}),
                &fx.worktree
            )
            .is_err()
        );
    }
    assert!(
        s.check_bash("echo x > notes.txt", &fx.worktree, &[])
            .is_err()
    );
    s.check_bash("git diff main...HEAD | head -50", &fx.worktree, &[])
        .unwrap();
}

#[test]
fn session_rights_denied_tools_refuse_ctx_tools_under_every_name() {
    let fx = fixture("developer", true);
    let s = state(&fx);
    assert!(rule(s.check_tool("ctx_refactor")).contains("denied_tools"));
    assert!(rule(s.check_tool_as("refactor_alias", "ctx_refactor")).contains("denied_tools"));
    s.check_tool("ctx_edit").unwrap();
}

#[test]
fn session_rights_ctx_execute_code_check() {
    let fx = fixture("developer", true);
    let s = state(&fx);
    assert!(rule(s.check_code("python", "print(1)", &fx.worktree)).contains("unanalysable"));
    rule(s.check_code("shell", "echo x > crates/pay/tests/refund.rs", &fx.worktree));
    s.check_code("bash", "echo x > crates/pay/src/x.txt", &fx.worktree)
        .unwrap();
}

#[test]
fn session_rights_broken_rights_file_fails_closed() {
    let fx = fixture("developer", true);
    let missing = fx.rights.with_file_name("missing.json");
    let cases = [
        from_file_arg(Some(&missing.to_string_lossy())),
        from_file_arg(Some("relative/rights.json")),
        from_file_arg(Some("")),
    ];
    for s in cases {
        assert!(matches!(s, State::Unavailable(_)), "{s:?}");
        assert!(
            s.check_write(&fx.worktree.join("crates/pay/src/lib.rs"))
                .is_err()
        );
        assert!(s.check_tool("ctx_read").is_err());
        assert!(
            s.check_native_tool("Bash", &json!({"command": "ls"}), &fx.worktree)
                .is_err()
        );
    }
    std::fs::write(&fx.rights, "{\"schema\": \"other\"}").unwrap();
    assert!(matches!(
        from_file_arg(Some(&fx.rights.to_string_lossy())),
        State::Unavailable(_)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let fx = fixture("developer", true);
        std::fs::set_permissions(&fx.rights, std::fs::Permissions::from_mode(0o666)).unwrap();
        let s = from_file_arg(Some(&fx.rights.to_string_lossy()));
        assert!(
            matches!(&s, State::Unavailable(r) if r.contains("writable by group")),
            "{s:?}"
        );
    }
    // A broken feature list is a broken profile, never "no locks".
    let fx = fixture("developer", true);
    std::fs::write(fx.worktree.join("specs/42/features.json"), "{}").unwrap();
    assert!(matches!(
        from_file_arg(Some(&fx.rights.to_string_lossy())),
        State::Unavailable(_)
    ));
}

/// F7: the profile and the feature list follow their schemas.
#[test]
fn session_rights_profile_and_list_schema_rules() {
    let broken_profiles = [
        ("availability", json!("soon")),
        ("stages", json!(["coding"])),
        ("id", json!("bee")),
        ("name", json!("")),
        ("delivers", json!("x".repeat(501))),
        ("write", json!(["own_worktree", "own_worktree"])),
        ("deny_write", json!(["nowhere"])),
    ];
    for (field, value) in broken_profiles {
        let mut p = profile("developer");
        p[field] = value;
        let fx = fixture_with(&p, &feature_list(true));
        let s = from_file_arg(Some(&fx.rights.to_string_lossy()));
        assert!(matches!(s, State::Unavailable(_)), "{field}: {s:?}");
    }
    let mut list = feature_list(true);
    list["locked_paths"] = json!([LOCKED]);
    let fx = fixture_with(&profile("developer"), &list);
    let s = from_file_arg(Some(&fx.rights.to_string_lossy()));
    assert!(
        matches!(&s, State::Unavailable(r) if r.contains("not among its own locked_paths")),
        "{s:?}"
    );
}

#[test]
fn session_rights_without_profile_changes_nothing() {
    let s = from_file_arg(None);
    assert!(!s.is_active());
    s.check_write(Path::new("/anything")).unwrap();
    s.check_native_tool("Bash", &json!({"command": "rm -rf x"}), Path::new("/"))
        .unwrap();
    s.check_native_tool("SomeNewWriteTool", &json!({}), Path::new("/"))
        .unwrap();
}

mod feature_list_rules {
    use crate::core::feature_list::*;
    use std::collections::BTreeMap;

    const SPEC: &str = r##"{
      "schema": "leanctx.hive.process.feature-list/v1",
      "process": "1.0",
      "task": {"vault": "#acme/payments", "number": 42},
      "spec_commit": "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d",
      "test_command": "cargo test -p pay refund",
      "locked_paths": ["crates/pay/tests/refund.rs", "specs/42/features.json"],
      "features": [
        {"id": "F1", "description": "over capture refused", "steps": ["capture 10.00", "refund 10.01"],
         "tests": ["crates/pay/tests/refund.rs::refuses_over_capture"], "passes": false},
        {"id": "F2", "description": "partial refund", "steps": ["capture 10.00", "refund 4.00"],
         "tests": ["crates/pay/tests/refund.rs::partial_then_rest"], "passes": false}
      ]
    }"##;

    fn spec() -> FeatureList {
        FeatureList::parse(SPEC).unwrap()
    }

    #[test]
    fn feature_list_passes_flip_by_developer_is_refused() {
        let before = spec();
        let mut after = before.clone();
        after.features[0].passes = true;
        let err = validate_transition(&before, &after, Writer::Role("developer")).unwrap_err();
        assert!(err.starts_with("feature_list_locked"), "{err}");
        assert!(validate_transition(&before, &after, Writer::Gate).is_ok());
    }

    #[test]
    fn feature_list_other_change_after_g2_is_refused_even_for_the_gate() {
        let before = spec();
        for change in [
            |l: &mut FeatureList| l.features[0].tests.push("x".into()),
            |l: &mut FeatureList| l.features[1].steps.clear(),
            |l: &mut FeatureList| l.test_command = "true".into(),
            |l: &mut FeatureList| l.locked_paths.pop().map(drop).unwrap_or(()),
            |l: &mut FeatureList| l.features.truncate(1),
        ] {
            let mut after = before.clone();
            change(&mut after);
            for writer in [
                Writer::Gate,
                Writer::Role("developer"),
                Writer::Role("spec_writer"),
            ] {
                let err = validate_transition(&before, &after, writer).unwrap_err();
                assert!(err.contains("only features[].passes may change"), "{err}");
            }
        }
    }

    #[test]
    fn feature_list_before_g2_belongs_to_the_spec_writer() {
        let mut before = spec();
        before.spec_commit = None;
        let mut after = before.clone();
        after.features[0].description = "reworded".into();
        assert!(validate_transition(&before, &after, Writer::Role("spec_writer")).is_ok());
        assert!(validate_transition(&before, &after, Writer::Role("developer")).is_err());
    }

    #[test]
    fn feature_list_gate_write_sets_passes_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("features.json");
        std::fs::write(&path, SPEC).unwrap();
        let mut results = BTreeMap::new();
        results.insert("F1".to_string(), true);
        assert!(
            apply_gate_passes(&path, &results).is_err(),
            "incomplete result"
        );
        results.insert("F2".to_string(), false);
        apply_gate_passes(&path, &results).unwrap();
        let after = FeatureList::load(&path).unwrap();
        assert!(after.features[0].passes);
        assert!(!after.features[1].passes);
        assert_eq!(after.test_command, spec().test_command);
    }

    #[test]
    fn feature_list_rejects_unsafe_locked_paths() {
        for bad in ["/etc/passwd", "../x", "a/../../b"] {
            let text = SPEC.replace(
                "crates/pay/tests/refund.rs\", \"specs",
                &format!("{bad}\", \"specs"),
            );
            assert!(FeatureList::parse(&text).is_err(), "{bad}");
        }
    }

    /// F7: the schema rules beyond the field types.
    #[test]
    fn feature_list_schema_rules_are_enforced() {
        let base: serde_json::Value = serde_json::from_str(SPEC).unwrap();
        let person = serde_json::json!({"kind": "person",
            "person_id": "0f8fad5b-d9cb-469f-a165-70867728950e", "session": "human"});
        let good_unlock = serde_json::json!([{"by": person,
            "decision": "dec_01HZX3K8Q9V5T2N7M4B6C1D0EF", "at": "2026-11-05T12:00:00Z"}]);
        let mut ok = base.clone();
        ok["unlocks"] = good_unlock;
        FeatureList::parse(&ok.to_string()).unwrap();
        let broken: Vec<(&str, serde_json::Value)> = vec![
            ("spec_commit", serde_json::json!("abc")),
            ("process", serde_json::json!("2.0")),
            ("test_command", serde_json::json!("x".repeat(2001))),
            ("task", serde_json::json!({"vault": "acme", "number": 0})),
            ("locked_paths", serde_json::json!(["a.rs", "a.rs"])),
            ("locked_paths", serde_json::json!(["x".repeat(501)])),
            (
                "unlocks",
                serde_json::json!([{"by": {"kind": "agent"}, "decision": "d", "at": "now"}]),
            ),
        ];
        for (field, value) in broken {
            let mut list = base.clone();
            list[field] = value;
            assert!(FeatureList::parse(&list.to_string()).is_err(), "{field}");
        }
        let mut empty_step = base.clone();
        empty_step["features"][0]["steps"] = serde_json::json!([""]);
        assert!(FeatureList::parse(&empty_step.to_string()).is_err());
    }
}

/// Contract follow-up (process-v1 7a6a0ef2): a Spec Writer profile copied
/// from roles.v1.json carries `write_until: "G2"`. It loads, writes spec and
/// tests before G2, and writes nothing after G2.
#[test]
fn session_rights_spec_writer_write_until_g2() {
    let mut writer = profile("spec_writer");
    writer["write_until"] = json!("G2");
    let fx = fixture_with(&writer, &feature_list(false));
    let s = state(&fx);
    s.check_write(&fx.worktree.join(LOCKED)).unwrap();
    s.check_write(&fx.worktree.join("specs/42/notes.md"))
        .unwrap();
    let fx = fixture_with(&writer, &feature_list(true));
    let s = state(&fx);
    let err = rule(s.check_write(&fx.worktree.join("specs/42/notes.md")));
    assert!(err.contains("write_until"), "{err}");
    let err = rule(s.check_write(&fx.worktree.join(LOCKED)));
    assert!(err.contains("feature_list_locked"), "{err}");
    let mut wrong = profile("spec_writer");
    wrong["write_until"] = json!("G6");
    let fx = fixture_with(&wrong, &feature_list(false));
    assert!(matches!(
        from_file_arg(Some(&fx.rights.to_string_lossy())),
        State::Unavailable(_)
    ));
    // R4-2: write_until with no feature list would never expire -> refused.
    let mut writer = profile("spec_writer");
    writer["write_until"] = json!("G2");
    let fx = fixture_with(&writer, &feature_list(false));
    let mut record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&fx.rights).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove("feature_list");
    std::fs::write(&fx.rights, record.to_string()).unwrap();
    set_private(&fx.rights);
    let s = from_file_arg(Some(&fx.rights.to_string_lossy()));
    assert!(
        matches!(&s, State::Unavailable(r) if r.contains("write_until_without_feature_list")),
        "{s:?}"
    );
}

/// R4-1: the rights file is protected against a rename of a directory that
/// holds it, and a rights file inside a write scope is refused at load.
#[test]
fn session_rights_rights_file_cannot_be_replaced_through_its_directory() {
    // Rights file in its own dir, outside every write scope: holding dirs
    // are refused, so `mv` of a parent cannot swap it.
    let fx = fixture("developer", true);
    let s = state(&fx);
    let dir = fx.rights.parent().unwrap().to_path_buf();
    let err = rule(s.check_write(&dir));
    assert!(err.contains("rights_file"), "{err}");
    let err = rule(bash(
        &s,
        &fx,
        &format!("mv {} {}.old", dir.display(), dir.display()),
    ));
    assert!(err.contains("rights_file"), "{err}");

    // Rights file inside a second own_worktree entry: refused at load.
    let fx = fixture("developer", true);
    let inside = fx.worktree.join("scratch");
    std::fs::create_dir_all(&inside).unwrap();
    let moved = inside.join("rights.json");
    std::fs::rename(&fx.rights, &moved).unwrap();
    set_private(&moved);
    let mut record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&moved).unwrap()).unwrap();
    record["bindings"]["own_worktree"]
        .as_array_mut()
        .unwrap()
        .push(json!(inside.to_string_lossy()));
    std::fs::write(&moved, record.to_string()).unwrap();
    set_private(&moved);
    let s = from_file_arg(Some(&moved.to_string_lossy()));
    assert!(
        matches!(&s, State::Unavailable(r) if r.contains("rights_file_in_scope")),
        "{s:?}"
    );
}

/// Hive Desktop binds a Developer's scratchpad and build folder as further
/// `own_worktree` entries: writable, but not roots of the locked paths.
#[test]
fn session_rights_extra_own_worktree_folders_are_writable_not_lock_roots() {
    let fx = fixture("developer", true);
    let scratch = fx.worktree.parent().unwrap().join("scratch");
    std::fs::create_dir_all(scratch.join("crates/pay/tests")).unwrap();
    let mut record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&fx.rights).unwrap()).unwrap();
    record["bindings"]["own_worktree"]
        .as_array_mut()
        .unwrap()
        .push(json!(scratch.to_string_lossy()));
    std::fs::write(&fx.rights, record.to_string()).unwrap();
    set_private(&fx.rights);
    let s = state(&fx);
    s.check_write(&scratch.join("out.log")).unwrap();
    s.check_write(&scratch.join(LOCKED)).unwrap();
    let err = rule(s.check_write(&fx.worktree.join(LOCKED)));
    assert!(err.contains("feature_list_locked"), "{err}");
}

/// `resolve` keeps the drive of a Windows path: `C:\x` must not
/// become `\x` (the rights file then pointed at a drive-less path).
#[test]
fn resolve_keeps_the_drive_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("wt");
    std::fs::create_dir_all(&dir).unwrap();
    let resolved = resolve(&dir).unwrap();
    assert_eq!(
        resolved.components().next(),
        dir.components().next(),
        "{} lost its prefix or root",
        resolved.display()
    );
    assert!(resolved.is_absolute(), "{}", resolved.display());
    assert!(resolved.is_dir(), "{}", resolved.display());
}
