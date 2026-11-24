// SPDX-License-Identifier: Apache-2.0
use super::*;

fn script(body: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(body);
    command
}

#[test]
fn handshake_reads_the_port_keeps_stdin_open_and_the_cockpit_ends_at_eof() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().display();
    let mut process = handshake(
        script(&format!(
            "IFS= read -r line; printf '%s' \"$line\" > '{path}/start'; \
             echo '{{\"listening\":4242}}'; cat > /dev/null; touch '{path}/eof'"
        )),
        b"{\"cockpit_token\":\"t\"}\n",
        Vec::new(),
        Duration::from_secs(10),
    )
    .unwrap();
    assert_eq!(process.port, 4242);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("start")).unwrap(),
        "{\"cockpit_token\":\"t\"}"
    );
    // Alive while the supervisor holds stdin, not merely not yet ended.
    std::thread::sleep(Duration::from_millis(300));
    assert!(process.running());
    assert!(!directory.path().join("eof").exists());
    assert!(
        process.stop(Duration::from_secs(5)),
        "ended by itself at EOF"
    );
    assert!(directory.path().join("eof").exists());
}

fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (path, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, *bytes).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

#[test]
fn a_bundle_is_read_from_its_directory_or_tarball_and_nothing_else() {
    let directory = tempfile::tempdir().unwrap();
    let files: Vec<(&str, Vec<u8>)> = BUNDLE_FILES
        .iter()
        .map(|(name, _)| (*name, name.as_bytes().to_vec()))
        .collect();
    let bundle = directory.path().join("lean-ctx-cockpit");
    std::fs::create_dir(&bundle).unwrap();
    for (name, bytes) in &files {
        std::fs::write(bundle.join(name), bytes).unwrap();
    }
    let from_dir = read_bundle(&bundle).unwrap();
    assert_eq!(
        (from_dir.catalog.as_slice(), from_dir.archive.as_slice()),
        (b"channel.json".as_slice(), b"archive.tar.gz".as_slice())
    );

    let tarball = directory.path().join("bundle.tar.gz");
    let nested: Vec<(String, &[u8])> = files
        .iter()
        .map(|(name, bytes)| (format!("lean-ctx-cockpit/{name}"), bytes.as_slice()))
        .collect();
    let entries: Vec<(&str, &[u8])> = nested.iter().map(|(p, b)| (p.as_str(), *b)).collect();
    std::fs::write(&tarball, tar_gz(&entries)).unwrap();
    let from_tar = read_bundle(&tarball).unwrap();
    assert_eq!(from_tar.manifest, b"manifest.json");

    // A missing, unknown, duplicated, oversized or deeper member is refused.
    let refused: [Vec<(&str, &[u8])>; 5] = [
        entries[..4].to_vec(),
        [
            entries.clone(),
            vec![("lean-ctx-cockpit/extra", b"x".as_slice())],
        ]
        .concat(),
        [
            entries.clone(),
            vec![("other/channel.json", b"y".as_slice())],
        ]
        .concat(),
        [
            vec![entries[0]],
            entries[2..].to_vec(),
            vec![("lean-ctx-cockpit/channel.sig", [0_u8; 65].as_slice())],
        ]
        .concat(),
        [
            entries[1..].to_vec(),
            vec![("a/b/channel.json", b"z".as_slice())],
        ]
        .concat(),
    ];
    for members in refused {
        std::fs::write(&tarball, tar_gz(&members)).unwrap();
        assert!(read_bundle(&tarball).is_err());
    }
}

#[test]
fn a_start_that_never_names_its_port_fails_within_the_deadline() {
    for body in ["exec sleep 30", "echo 'listening on 4242'; exec sleep 30"] {
        let begun = Instant::now();
        let refused = handshake(
            script(body),
            b"{}\n",
            Vec::new(),
            Duration::from_millis(300),
        );
        assert_eq!(refused.err(), Some(CockpitUnavailable::StartFailed));
        assert!(begun.elapsed() < Duration::from_secs(5), "{body}");
    }
}

#[test]
fn a_crashed_cockpit_reports_its_status_and_redacted_stderr() {
    let token = "c0ffee".repeat(10);
    let mut process = handshake(
        script(&format!(
            "IFS= read -r line; echo '{{\"listening\":4243}}'; \
             printf 'boot\\033[2J ok\\n' >&2; \
             echo 'fatal: memory allocation of 42949672960 bytes failed (token {token})' >&2; \
             exit 3"
        )),
        b"{}\n",
        vec![token.clone(), String::new()],
        Duration::from_secs(10),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while process.running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let report = process.exit_report();
    assert!(report.contains("exit status: 3"), "{report}");
    assert!(
        report.contains("memory allocation of 42949672960 bytes failed"),
        "{report}"
    );
    assert!(report.contains("[REDACTED:token]"), "{report}");
    assert!(!report.contains(&token), "{report}");
    assert!(!report.contains('\u{1b}'), "{report}");
}

#[test]
fn a_cockpit_the_supervisor_killed_is_reported_as_stopped_not_crashed() {
    let mut process = handshake(
        script("IFS= read -r line; echo '{\"listening\":4244}'; sleep 30"),
        b"{}\n",
        Vec::new(),
        Duration::from_secs(10),
    )
    .unwrap();
    let mut ended = handshake(
        script("IFS= read -r line; echo '{\"listening\":4245}'; exit 3"),
        b"{}\n",
        Vec::new(),
        Duration::from_secs(10),
    )
    .unwrap();
    assert!(!process.stop(Duration::ZERO), "the supervisor killed it");
    let report = process.exit_report();
    assert!(report.contains("(stopped by the supervisor)"), "{report}");
    assert!(!report.contains("may have failed"), "{report}");

    let deadline = Instant::now() + Duration::from_secs(10);
    while ended.running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let report = ended.exit_report();
    assert!(report.contains("exit status: 3"), "{report}");
    assert!(!report.contains("stopped by the supervisor"), "{report}");
}

#[test]
fn the_stderr_tail_is_bounded_and_the_memory_budget_has_a_floor() {
    let mut tail = StderrTail::default();
    for _ in 0..10 {
        keep_tail(&mut tail, &[b'a'; 1000], STDERR_TAIL);
    }
    keep_tail(&mut tail, b"end", STDERR_TAIL);
    assert_eq!(tail.bytes.len(), STDERR_TAIL);
    assert!(tail.bytes.ends_with(b"aend"));
    assert!(tail.cut);

    let mib = 1024 * 1024;
    assert_eq!(memory_budget(None), Some(MEMORY_BUDGET_DEFAULT_MIB * mib));
    assert_eq!(memory_budget(Some("0")), None);
    assert_eq!(memory_budget(Some(" 8192 ")), Some(8192 * mib));
    assert_eq!(memory_budget(Some("12")), Some(MEMORY_BUDGET_MIN_MIB * mib));
    assert_eq!(
        memory_budget(Some("lots")),
        Some(MEMORY_BUDGET_DEFAULT_MIB * mib)
    );
    assert_eq!(memory_budget(Some(&u64::MAX.to_string())), Some(u64::MAX));
}

#[test]
fn a_token_cut_by_the_tail_bound_or_still_being_written_is_never_shown_in_part() {
    let token = "c0ffee".repeat(10);
    let secrets = vec![token.clone()];
    // The bound cuts the token's line: its first characters are gone.
    let mut tail = StderrTail::default();
    keep_tail(
        &mut tail,
        format!("token {token}\nsafe line\n").as_bytes(),
        token.len() + 2,
    );
    assert!(tail.cut && !tail.bytes.starts_with(b"token"));
    let report = stderr_report(&tail.bytes, tail.cut, false, &secrets);
    assert_eq!(report, "safe line");
    // The last line is still being written: the token's head is there.
    let partial = format!("safe line\nprefix {}", &token[..20]);
    assert_eq!(
        stderr_report(partial.as_bytes(), false, true, &secrets),
        "safe line"
    );
    // A piece of the token left at an edge is redacted all the same.
    let edges = format!("{} middle {}", &token[50..], &token[..8]);
    let report = stderr_report(edges.as_bytes(), false, false, &secrets);
    assert_eq!(report, "[REDACTED:token] middle [REDACTED:token]");
}

#[test]
fn a_failed_automatic_install_is_retried_after_a_day() {
    let now = std::time::SystemTime::now();
    assert!(!retry_blocked(None, now));
    assert!(retry_blocked(Some(now - Duration::from_secs(60)), now));
    assert!(retry_blocked(
        Some(now - AUTO_RETRY + Duration::from_secs(1)),
        now
    ));
    assert!(!retry_blocked(Some(now - AUTO_RETRY), now));
    // A clock that went backwards never blocks.
    assert!(!retry_blocked(Some(now + Duration::from_secs(60)), now));
}

#[test]
fn no_cockpit_opt_out_and_a_source_build_install_nothing() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _off = crate::test_env::VarGuard::set("LEAN_CTX_NO_COCKPIT", "1");
    assert!(installs_opted_out());
    assert!(!install_if_missing(true));
    drop(_off);
    let _on = crate::test_env::VarGuard::set("LEAN_CTX_NO_COCKPIT", "0");
    assert!(!installs_opted_out());
    // A test build carries no Cockpit channel: nothing to install from.
    assert!(!install_if_missing(true));
}

/// #2270: a failed download was reported as "not installed" and named the
/// very install command that had just failed.
#[test]
fn a_failed_download_is_not_reported_as_not_installed() {
    let reason = install_failure(&InstallError::Download);
    assert_eq!(reason, CockpitUnavailable::DownloadFailed);
    assert_eq!(reason.as_str(), "download_failed");
    let message = reason.message();
    assert!(!message.contains("not installed"), "{message}");
    assert!(message.contains("download failed"), "{message}");
    assert!(
        message.contains("lean-ctx cockpit install --from"),
        "{message}"
    );
    assert_eq!(
        install_failure(&InstallError::Signature),
        CockpitUnavailable::VerificationFailed
    );
    assert_eq!(
        install_failure(&InstallError::Policy),
        CockpitUnavailable::NotInstalled
    );
}
