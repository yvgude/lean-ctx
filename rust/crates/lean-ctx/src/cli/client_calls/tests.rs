// SPDX-License-Identifier: Apache-2.0
//! Client Calls v1: what an ACP client (Hive Desktop) builds against, and
//! the boundaries every call passes.

use serde_json::{Value, json};

use super::exec::{self, Tail, Utf8Stream, prepare};
use super::fs::{read, slice_lines, write};
use super::*;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn caller_in(root: &str) -> Caller {
    Caller {
        session: "sess-1".into(),
        agent: "codex".into(),
        project_root: root.to_owned(),
    }
}

/// A project `proj` inside a temp dir, with `outside.txt` next to it.
fn project() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = lean_ctx::core::pathutil::safe_canonicalize(tmp.path())
        .unwrap()
        .join("proj");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(tmp.path().join("outside.txt"), "out").unwrap();
    (tmp, root.to_string_lossy().into_owned())
}

fn client_entries() -> Vec<lean_ctx::core::audit_trail::AuditEntry> {
    lean_ctx::core::audit_trail::load_recent(100)
        .into_iter()
        .filter(|e| e.tool.starts_with("client."))
        .collect()
}

#[test]
fn documents_and_attribution_follow_the_contract() {
    let ok: Value =
        serde_json::from_str(&document("client.fs_write", &json!({"bytes": 1}))).unwrap();
    assert_eq!(ok["contract"], "client-calls/1");
    assert_eq!(ok["kind"], "client.fs_write");
    let err: Value =
        serde_json::from_str(&error_document(&Failure::new(Code::TooLarge, "big"))).unwrap();
    assert_eq!(err["kind"], "error");
    assert_eq!(err["error"]["code"], "too_large");
    assert_eq!(Code::InvalidArgument.exit_status(), 2);
    assert_eq!(Code::Forbidden.exit_status(), 1);

    let named = Caller::from_args(&args(&["--session", "s", "--agent", "claude"]), "/p").unwrap();
    assert_eq!(named.agent, "claude");
    for bad in [
        args(&["--agent", "claude"]),
        args(&["--session", "s"]),
        args(&["--session", "a\nb", "--agent", "x"]),
    ] {
        let failure = Caller::from_args(&bad, "/p").unwrap_err();
        assert_eq!(failure.code, Code::InvalidArgument, "{bad:?}");
    }
}

#[test]
fn fs_read_is_verbatim_sliced_and_bounded() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let (_tmp, root) = project();
    // Text a compressed view would change: comments, blank lines, trailing
    // whitespace, CRLF.
    let text = "// keep\nfn a() {}   \n\n\n/* block */\r\nlast";
    let path = format!("{root}/src.rs");
    std::fs::write(&path, text).unwrap();

    let whole = read(&args(&["--path", &path]), &root, &caller_in(&root)).unwrap();
    assert_eq!(whole.content, text);
    assert_eq!(whole.total_lines, 6);
    let part = read(
        &args(&["--path", &path, "--line", "2", "--limit", "2"]),
        &root,
        &caller_in(&root),
    )
    .unwrap();
    assert_eq!(part.content, "fn a() {}   \n\n");
    assert_eq!(slice_lines(text, Some(6), None), "last");
    assert_eq!(slice_lines(text, Some(9), Some(1)), "");

    let relative = read(&args(&["--path", "src.rs"]), &root, &caller_in(&root)).unwrap_err();
    assert_eq!(relative.code, Code::InvalidArgument);

    lean_ctx::test_env::set_var("LEAN_CTX_MAX_READ_BYTES", "1024");
    let big = format!("{root}/big.txt");
    std::fs::write(&big, "x".repeat(2048)).unwrap();
    let too_large = read(&args(&["--path", &big]), &root, &caller_in(&root));
    lean_ctx::test_env::remove_var("LEAN_CTX_MAX_READ_BYTES");
    assert_eq!(too_large.unwrap_err().code, Code::TooLarge);

    let binary = format!("{root}/blob.txt");
    std::fs::write(&binary, [0xff, 0xfe, 0x00, 0x41]).unwrap();
    let not_text = read(&args(&["--path", &binary]), &root, &caller_in(&root)).unwrap_err();
    assert_eq!(not_text.code, Code::InvalidArgument);

    let entries = client_entries();
    assert!(
        entries.iter().any(|e| e.tool == "client.fs_read"
            && e.agent_id == "codex"
            && e.action.as_deref() == Some("session=sess-1 outcome=done")),
        "every read is in the audit trail with its caller"
    );
    let stream = lean_ctx::core::activity::store()
        .unwrap()
        .list(&lean_ctx::core::activity::Filter {
            session: Some("sess-1".into()),
            project_root: Some(lean_ctx::core::activity::project_root(&root)),
            ..Default::default()
        })
        .unwrap();
    // The stream records targets with `/` separators (Windows: `C:/...`).
    let reads: Vec<_> = stream
        .events
        .iter()
        .filter(|e| e.kind == lean_ctx::core::activity::Kind::FileRead)
        .map(|e| (e.target.clone(), e.outcome, e.source, e.agent.clone()))
        .collect();
    assert!(
        reads.contains(&(
            Some(path.replace('\\', "/")),
            Some(lean_ctx::core::activity::Outcome::Ok),
            lean_ctx::core::activity::Source::Acp,
            "codex".to_owned()
        )),
        "every read is in the session's activity stream: {reads:?}"
    );
}

#[test]
fn fs_calls_refuse_jail_escapes_and_record_them() {
    if cfg!(feature = "no-jail") {
        return;
    }
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let (_tmp, root) = project();
    let outside = format!("{root}/../outside.txt");

    let read_refused = read(&args(&["--path", &outside]), &root, &caller_in(&root)).unwrap_err();
    assert_eq!(read_refused.code, Code::Forbidden);
    let escape = format!("{root}/../escaped.txt");
    let write_refused =
        write(&args(&["--path", &escape]), &root, &caller_in(&root), "x").unwrap_err();
    assert_eq!(write_refused.code, Code::Forbidden);
    assert!(
        !std::path::Path::new(&escape).exists(),
        "nothing written outside"
    );

    let jailed: Vec<_> = client_entries()
        .into_iter()
        .filter(|e| e.action.as_deref() == Some("session=sess-1 outcome=jailed"))
        .map(|e| e.tool)
        .collect();
    assert!(jailed.contains(&"client.fs_read".to_owned()), "{jailed:?}");
    assert!(jailed.contains(&"client.fs_write".to_owned()), "{jailed:?}");
}

#[test]
fn fs_write_replaces_the_file_and_refuses_redaction_markers() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let (_tmp, root) = project();
    let path = format!("{root}/new/dir/file.txt");

    let created = write(&args(&["--path", &path]), &root, &caller_in(&root), "one\n").unwrap();
    assert!(created.created);
    assert_eq!(created.bytes, 4);
    let replaced = write(
        &args(&["--path", &path]),
        &root,
        &caller_in(&root),
        "two\nlines\n",
    )
    .unwrap();
    assert!(!replaced.created);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "two\nlines\n");

    let marker = "DB_URL=[REDACTED:Database credentials]\n";
    let refused = write(&args(&["--path", &path]), &root, &caller_in(&root), marker).unwrap_err();
    assert_eq!(refused.code, Code::Forbidden);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "two\nlines\n");
    let config = format!("{root}/config.toml");
    let refused = write(
        &args(&["--path", &config]),
        &root,
        &caller_in(&root),
        marker,
    )
    .unwrap_err();
    assert_eq!(refused.code, Code::Forbidden);
    // A test folder walked out of with `..` is not a test folder.
    std::fs::create_dir_all(format!("{root}/tests")).unwrap();
    let settings = format!("{root}/tests/../settings.py");
    let refused = write(
        &args(&["--path", &settings]),
        &root,
        &caller_in(&root),
        marker,
    )
    .unwrap_err();
    assert_eq!(refused.code, Code::Forbidden);
    assert!(!std::path::Path::new(&format!("{root}/settings.py")).exists());

    // A Rust test that asserts on the marker is source code, not data.
    let test_file = format!("{root}/src/redact_tests.rs");
    let source = "assert!(out.contains(\"[REDACTED:AWS key]\"));\n";
    write(
        &args(&["--path", &test_file]),
        &root,
        &caller_in(&root),
        source,
    )
    .unwrap();
    assert_eq!(std::fs::read_to_string(&test_file).unwrap(), source);
}

#[test]
fn exec_checks_allowlist_jail_and_env_before_it_runs() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let (_tmp, root) = project();
    lean_ctx::test_env::remove_var("LEAN_CTX_ALLOWLIST_WARN_ONLY");
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "printf");
    let flags = args(&[]);

    let denied = prepare(&flags, &args(&["xxd", "-p"]), &root, &caller_in(&root));
    let outside = prepare(
        &args(&["--cwd", &format!("{root}/..")]),
        &args(&["printf", "x"]),
        &root,
        &caller_in(&root),
    );
    let reserved = prepare(
        &args(&["--env", "NODE_OPTIONS=--require=./evil.js"]),
        &args(&["printf", "x"]),
        &root,
        &caller_in(&root),
    );
    let not_a_program = prepare(&flags, &args(&["X=1", "printf"]), &root, &caller_in(&root));
    let allowed = prepare(
        &args(&["--env", "A=b", "--output-byte-limit", "10"]),
        &args(&["printf", "x"]),
        &root,
        &caller_in(&root),
    );
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");

    assert_eq!(denied.unwrap_err().code, Code::Forbidden);
    if !cfg!(feature = "no-jail") {
        assert_eq!(outside.unwrap_err().code, Code::Forbidden);
    }
    assert_eq!(reserved.unwrap_err().code, Code::Forbidden);
    assert_eq!(not_a_program.unwrap_err().code, Code::InvalidArgument);
    let allowed = allowed.unwrap();
    assert_eq!(allowed.checked, "printf x");
    assert!(allowed.cwd.ends_with("proj"), "{}", allowed.cwd);
    assert_eq!(allowed.env, vec![("A".to_owned(), "b".to_owned())]);
    assert_eq!(allowed.output_limit, 10);
    let exec_outcomes: Vec<_> = client_entries()
        .into_iter()
        .filter(|e| e.tool == "client.exec")
        .filter_map(|e| e.action)
        .collect();
    assert!(
        exec_outcomes.contains(&"session=sess-1 outcome=denied".to_owned()),
        "a refused command is in the audit trail: {exec_outcomes:?}"
    );
    assert!(
        exec_outcomes.contains(&"session=sess-1 outcome=failed".to_owned()),
        "a refused argument is in the audit trail too: {exec_outcomes:?}"
    );
}

/// C1: an `--env` override passes the ONE override policy of `ctx_shell`.
#[test]
fn exec_env_refuses_every_hijacking_family() {
    for key in [
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "DYLD_INSERT_LIBRARIES",
        "GIT_SSH_COMMAND",
        "GIT_CONFIG_COUNT",
        "GIT_ALLOW_PROTOCOL",
        "GIT_ASKPASS",
        "BASH_ENV",
        "BASH_FUNC_ls%%",
        "PROMPT_COMMAND",
        "NODE_OPTIONS",
        "npm_config_node_options",
        "PERL5OPT",
        "RUBYOPT",
        "PYTHONPATH",
        "JAVA_TOOL_OPTIONS",
        "PATH",
        "HOME",
        "XDG_CONFIG_HOME",
        "LCTX_ALLOW_PATH",
        "LEAN_CTX_DISABLED",
        "EDITOR",
    ] {
        let pair = format!("{key}=/proj/evil");
        let refused = exec::environment(&[pair.as_str()]).unwrap_err();
        assert_eq!(refused.code, Code::Forbidden, "{key}");
        assert!(refused.message.contains(key), "{key}: {}", refused.message);
    }
    let ok = exec::environment(&["A=b", "RUST_LOG=debug", "GIT_PAGER=cat"]).unwrap();
    assert_eq!(ok.len(), 3);
    assert_eq!(
        exec::environment(&["novalue"]).unwrap_err().code,
        Code::InvalidArgument
    );
}

/// C2: the gate checks exactly the argv that runs.
#[test]
fn exec_checks_the_argv_that_runs() {
    for argv in [
        &["X=1", "ls"][..],
        &["X=/d/x"],
        &["export", "ls"],
        &["/usr/bin/declare", "ls"],
        &["eval", "ls"],
        &[""],
    ] {
        let failure = exec::checked_command(&args(argv)).unwrap_err();
        assert_eq!(failure.code, Code::InvalidArgument, "{argv:?}");
    }
    let argv = args(&["git", "commit", "-m", "a b; rm -rf /", "$HOME", "it's"]);
    let checked = exec::checked_command(&argv).unwrap();
    assert_eq!(
        lean_ctx_shell_policy::shell_tokenize(&checked),
        argv,
        "the checked text reads back as the argv: {checked}"
    );
}

thread_local! {
    static STREAMED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn streamed(data: &str) {
    STREAMED.with(|s| s.borrow_mut().push(data.to_owned()));
}

fn take_streamed() -> Vec<String> {
    STREAMED.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// A private-key header and END marker, built from parts so this source
/// file holds no key block.
fn key_markers() -> (String, String) {
    let kind = ["PRIV", "ATE KEY-----"].concat();
    (format!("-----BEGIN {kind}"), format!("-----END {kind}"))
}

/// Body lines of a key block (not a key): two base64 lines and the short
/// last line a real key ends with.
const BODY: [&str; 3] = [
    "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7VJTUt9Us8cKj",
    "ZmFrZWJvZHlmYWtlYm9keWZha2Vib2R5ZmFrZWJvZHlmYWtlYm9keWZha2Vib2R5",
    "c2hvcnQ=",
];

fn redacted(data: &str) {
    streamed(&lean_ctx_server::server::policy_guard::redact_model_text(
        data,
    ));
}

/// What follows a key block in the probes.
#[derive(Clone, Copy, Debug)]
enum After {
    Nothing,
    /// A certificate: its BEGIN/END must not close the key block (B1).
    Certificate,
    /// A complete second key (B2).
    CompleteKey,
}

/// A key block whose every line carries `prefix` (as `grep -A`, `grep -n`,
/// a diff, YAML, a log with timestamps print it), a stderr line inside, the
/// END marker only when `with_end`, then `after`.
fn prefixed_block(prefix: &str, with_end: bool, after: After) -> String {
    let (begin, end) = key_markers();
    let [k1, k2, k3] = BODY;
    let mut text = format!("before\n{prefix}{begin}\n{prefix}{k1}\nwarning: stderr line\n");
    text.push_str(&format!("{prefix}{k2}\n{prefix}{k3}\n"));
    if with_end {
        text.push_str(&format!("{prefix}{end}\n"));
    }
    let cert = ["CERTIFICATE", "-----"].concat();
    // A body of its own: a certificate is public and may stream.
    let cert_body = "Y2VydGJvZHljZXJ0Ym9keWNlcnRib2R5Y2VydGJvZHljZXJ0Ym9keWNlcnRib2R5";
    match after {
        After::Nothing => {}
        After::Certificate => text.push_str(&format!(
            "{prefix}-----BEGIN {cert}\n{prefix}{cert_body}\n{prefix}-----END {cert}\n"
        )),
        After::CompleteKey => {
            text.push_str(&format!("{prefix}{begin}\n{prefix}{k1}\n{prefix}{end}\n"));
        }
    }
    text.push_str("after\n");
    text
}

/// C3/R1/B1/B2: a key block never shows in the redacted stream or the
/// redacted final document — 10 prefix shapes, with and without END,
/// followed by nothing, a certificate or a complete key, fed at 1, 7, 64
/// bytes and whole, with output limits from 40 bytes to 1 MiB. The short
/// last key line of a block without END is released by decision
/// (client-calls-pem-bound-2026-11-05: only lines with a 40-char base64
/// run are withheld once the hold is given up).
#[test]
fn exec_stream_never_shows_key_material_in_any_shape_or_split() {
    let prefixes = [
        "",
        "+",
        " ",
        "    ",
        "> ",
        "# ",
        "k.pem-",
        "k.pem:12:",
        "2026-11-05T10:00:00Z ",
        "| ",
    ];
    for prefix in prefixes {
        for with_end in [true, false] {
            for after in [After::Nothing, After::Certificate, After::CompleteKey] {
                let text = prefixed_block(prefix, with_end, after);
                let secret: &[&str] = if with_end { &BODY } else { &BODY[..2] };
                for step in [1, 7, 64, text.len()] {
                    for limit in [40, 100, 128, 160, 1 << 20] {
                        let _ = take_streamed();
                        let mut output = exec::Output::new(limit, redacted);
                        for piece in text.as_bytes().chunks(step) {
                            output.push(std::str::from_utf8(piece).unwrap());
                        }
                        let (final_text, _) = output.finish();
                        let stream = take_streamed().concat();
                        let shown =
                            lean_ctx_server::server::policy_guard::redact_model_text(&final_text);
                        let case = format!(
                            "{prefix:?} end={with_end} {after:?} step={step} limit={limit}"
                        );
                        for body in secret {
                            assert!(!stream.contains(body), "{case}: streamed: {stream:?}");
                            assert!(!shown.contains(body), "{case}: final: {shown:?}");
                        }
                        assert!(stream.starts_with("before\n"), "{case}: {stream:?}");
                        assert!(stream.ends_with("after\n"), "{case}: {stream:?}");
                    }
                }
            }
        }
    }
}

/// The held block streams as ONE chunk once its END comes; a block longer
/// than the hold bound is withheld through its END and the output after it
/// streams; L1: a cut tail of one line keeps it.
#[test]
fn exec_stream_holds_a_block_to_its_end_and_withholds_one_over_the_bound() {
    let (begin, end) = key_markers();
    let [k1, k2, _] = BODY;
    let _ = take_streamed();
    let mut output = exec::Output::new(1 << 20, streamed);
    output.push(&format!("before\n{begin}\n{k1}\n"));
    assert_eq!(take_streamed(), vec!["before\n".to_owned()]);
    output.push(&format!("{k2}\n{end}\nafter\n"));
    let chunks = take_streamed();
    assert_eq!(chunks.len(), 2, "{chunks:?}");
    assert!(chunks[0].starts_with(&begin) && chunks[0].contains(&end));
    assert_eq!(chunks[1], "after\n");
    let _ = output.finish();

    let mut long = exec::Output::new(64, streamed);
    long.push(&format!("{begin}\n"));
    long.push(&format!("{k2}\n").repeat(17_000));
    long.push(&format!("{k2}\n{end}\nafter\n"));
    let (text, _) = long.finish();
    let chunks = take_streamed();
    assert_eq!(chunks.len(), 3, "{chunks:?}");
    assert!(
        chunks[0].contains("[REDACTED") && !chunks[0].contains(&begin),
        "{chunks:?}"
    );
    assert_eq!(chunks[1], exec::WITHHELD_CHUNK);
    assert_eq!(chunks[2], "after\n");
    assert_eq!(text, "after\n");

    let mut one = Tail::new(2);
    one.push("a\nb\n");
    assert_eq!(one.finish(), ("b\n".to_owned(), true));
}

/// B3: grep hits that only mention a key header hold at most the bound;
/// each hit stays readable up to its header (`file:line:` kept, the header
/// redacted), and the 2000 ordinary lines after them all stream and stay in
/// the final document.
#[test]
fn exec_stream_releases_ordinary_lines_after_a_mere_mention() {
    let (begin, _) = key_markers();
    let hits = [
        "src/a.rs:3: const H: &str = \"",
        "src/b.rs:9: let h = \"",
        "src/c.rs:1: \"",
    ];
    let lines: Vec<String> = (0..2000)
        .map(|n| format!("line {n}: ordinary text\n"))
        .collect();
    for (with_lines, feed_whole) in [(true, false), (false, false), (false, true)] {
        let mut input = String::new();
        for hit in hits {
            input.push_str(&format!("{hit}{begin}\";\n"));
        }
        if with_lines {
            input.extend(lines.iter().cloned());
        }
        let _ = take_streamed();
        let mut output = exec::Output::new(1 << 20, redacted);
        if feed_whole {
            output.push(&input);
        } else {
            for line in input.split_inclusive('\n') {
                output.push(line);
            }
        }
        let (text, _) = output.finish();
        let stream = take_streamed().concat();
        let case = format!("lines={with_lines} whole={feed_whole}");
        for hit in hits {
            assert!(
                stream.contains(hit),
                "{case}: {hit:?} not streamed: {stream:?}"
            );
            assert!(
                text.contains(hit),
                "{case}: {hit:?} not in the final document"
            );
        }
        assert!(!stream.contains(&begin), "{case}: {stream:?}");
        if !with_lines {
            continue;
        }
        for line in &lines {
            assert!(stream.contains(line.as_str()), "{line:?} not streamed");
            assert!(
                text.contains(line.as_str()),
                "{line:?} not in the final document"
            );
        }
    }
}

/// The command runs: the tail within the limit, the exit code, the record.
#[cfg(unix)]
#[test]
fn exec_run_keeps_the_tail_reports_the_exit_and_stops_on_a_signal() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let (_tmp, root) = project();
    let request = |argv: &[&str], limit: usize| exec::ExecRequest {
        argv: args(argv),
        checked: argv.join(" "),
        cwd: root.clone(),
        env: Vec::new(),
        output_limit: limit,
    };

    let printed = exec::run(
        &request(&["printf", "abcdef"], 3),
        &caller_in(&root),
        streamed,
    )
    .unwrap();
    assert_eq!(printed.output, "def");
    assert!(printed.truncated);
    assert_eq!(printed.exit_code, Some(0));
    assert_eq!(
        take_streamed().concat(),
        "abcdef",
        "the stream is raw and complete"
    );

    let failed = exec::run(
        &request(&["sh", "-c", "exit 3"], 64),
        &caller_in(&root),
        streamed,
    )
    .unwrap();
    assert_eq!(
        (failed.exit_code, failed.signal.as_deref()),
        (Some(3), None)
    );

    exec::stop::simulate(libc::SIGTERM);
    let stopped = exec::run(&request(&["sleep", "30"], 64), &caller_in(&root), streamed);
    exec::stop::simulate(0);
    let stopped = stopped.unwrap();
    assert_eq!(
        (stopped.exit_code, stopped.signal.as_deref()),
        (None, Some("SIGTERM"))
    );

    let outcomes: Vec<_> = client_entries()
        .into_iter()
        .filter(|e| e.tool == "client.exec")
        .filter_map(|e| e.action)
        .collect();
    for outcome in ["done", "failed", "cancelled"] {
        let action = format!("session=sess-1 outcome={outcome}");
        assert!(outcomes.contains(&action), "{action} in {outcomes:?}");
    }
    let texts: Vec<_> = lean_ctx::core::activity::store()
        .unwrap()
        .list(&lean_ctx::core::activity::Filter {
            session: Some("sess-1".into()),
            project_root: Some(lean_ctx::core::activity::project_root(&root)),
            ..Default::default()
        })
        .unwrap()
        .events
        .into_iter()
        .filter_map(|e| e.text)
        .collect();
    assert!(texts.contains(&"exit 3".to_owned()), "{texts:?}");
}

/// C7: links out of the project, secret paths and read-only roots.
#[cfg(unix)]
#[test]
fn fs_calls_refuse_links_out_secret_paths_and_read_only_roots() {
    if cfg!(feature = "no-jail") {
        return;
    }
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let (tmp, root) = project();
    let outside_dir = tmp.path().canonicalize().unwrap();
    std::os::unix::fs::symlink(outside_dir.join("outside.txt"), format!("{root}/link.txt"))
        .unwrap();
    std::os::unix::fs::symlink(&outside_dir, format!("{root}/linkdir")).unwrap();
    let caller = caller_in(&root);

    let through_link = format!("{root}/link.txt");
    assert_eq!(
        read(&args(&["--path", &through_link]), &root, &caller)
            .unwrap_err()
            .code,
        Code::Forbidden
    );
    assert_eq!(
        write(&args(&["--path", &through_link]), &root, &caller, "x")
            .unwrap_err()
            .code,
        Code::Forbidden
    );
    assert_eq!(
        std::fs::read_to_string(outside_dir.join("outside.txt")).unwrap(),
        "out"
    );
    let via_dir = format!("{root}/linkdir/created.txt");
    assert_eq!(
        write(&args(&["--path", &via_dir]), &root, &caller, "x")
            .unwrap_err()
            .code,
        Code::Forbidden
    );
    assert!(!outside_dir.join("created.txt").exists());

    std::fs::write(format!("{root}/.env"), "TOKEN=x").unwrap();
    lean_ctx::test_env::set_var("LEAN_CTX_IO_BOUNDARY_MODE", "enforce");
    let secret = read(&args(&["--path", &format!("{root}/.env")]), &root, &caller);
    lean_ctx::test_env::remove_var("LEAN_CTX_IO_BOUNDARY_MODE");
    assert_eq!(secret.unwrap_err().code, Code::Forbidden);

    let read_only = format!("{root}/ro");
    std::fs::create_dir_all(&read_only).unwrap();
    lean_ctx::test_env::set_var("LEAN_CTX_READ_ONLY_ROOTS", &read_only);
    let into_ro = write(
        &args(&["--path", &format!("{read_only}/f.txt")]),
        &root,
        &caller,
        "x",
    );
    lean_ctx::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");
    assert_eq!(into_ro.unwrap_err().code, Code::Forbidden);
    assert!(!std::path::Path::new(&format!("{read_only}/f.txt")).exists());

    let refusals = client_entries()
        .into_iter()
        .filter(|e| {
            e.action
                .as_deref()
                .is_some_and(|a| a.ends_with("jailed") || a.ends_with("denied"))
        })
        .count();
    assert_eq!(refusals, 5, "every refusal is recorded");
}

#[test]
fn exec_output_keeps_a_char_safe_tail_across_split_reads() {
    let mut tail = Tail::new(2);
    tail.push("éa"); // 3 bytes: a cut at 1 would split "é"
    let (text, truncated) = tail.finish();
    assert_eq!(text, "a");
    assert!(truncated);
    let mut kept = Tail::new(8);
    kept.push("short");
    assert_eq!(kept.finish(), ("short".to_owned(), false));

    let mut stream = Utf8Stream::default();
    let bytes = "é✓".as_bytes();
    assert_eq!(stream.push(&bytes[..1]), "");
    assert_eq!(stream.push(&bytes[1..3]), "é");
    assert_eq!(stream.push(&bytes[3..]), "✓");
    assert_eq!(stream.push(&[0xff, b'a']), "\u{fffd}a");
    assert_eq!(stream.push(&[0xe2]), "");
    assert_eq!(stream.finish(), "\u{fffd}");
}
