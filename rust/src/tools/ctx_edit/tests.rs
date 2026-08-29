#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    fn mk_params(path: &Path, old: &str, new: &str, replace_all: bool, create: bool) -> EditParams {
        EditParams {
            path: path.to_string_lossy().to_string(),
            old_string: old.to_string(),
            new_string: new.to_string(),
            replace_all,
            create,
            expected_blake3: None,
            expected_size: None,
            expected_mtime_ms: None,
            backup: false,
            backup_path: None,
            evidence: false,
            diff_max_lines: 200,
            allow_lossy_utf8: false,
        }
    }

    #[test]
    fn replace_single_occurrence() {
        let f = make_temp("fn hello() {\n    println!(\"hello\");\n}\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(f.path(), "hello", "world", false, false),
        );
        assert!(result.contains("ERROR"), "should fail: 'hello' appears 2x");
    }

    #[test]
    fn replace_all() {
        let f = make_temp("aaa bbb aaa\n");
        let mut cache = SessionCache::new();
        let result = handle(&mut cache, &mk_params(f.path(), "aaa", "ccc", true, false));
        assert!(result.contains("2 replacements"));
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "ccc bbb ccc\n");
    }

    #[test]
    fn not_found_error() {
        let f = make_temp("some content\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(f.path(), "nonexistent", "x", false, false),
        );
        assert!(result.contains("ERROR: old_string not found"));
    }

    #[test]
    fn create_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/new_file.txt");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(&path, "", "line1\nline2\nline3\n", false, true),
        );
        assert!(result.contains("created new_file.txt"));
        assert!(result.contains("3 lines"));
        assert!(path.exists());
    }

    /// #475: creating a file inside a read-only root is refused before the
    /// directory is even materialised (guard in `handle_create`).
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn create_denied_in_read_only_root() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("refrepo");
        std::fs::create_dir_all(&ro).unwrap();
        let path = ro.join("sub/new_file.txt");

        let ro_canon = crate::core::pathjail::canonicalize_or_self(&ro);
        crate::test_env::set_var(
            "LEAN_CTX_READ_ONLY_ROOTS",
            ro_canon.to_string_lossy().as_ref(),
        );
        let mut cache = SessionCache::new();
        let result = handle(&mut cache, &mk_params(&path, "", "x\n", false, true));
        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        assert!(
            result.contains("read-only"),
            "create in a read-only root must be refused: {result}"
        );
        assert!(!path.exists(), "no file may be created in a read-only root");
        assert!(
            !ro.join("sub").exists(),
            "no directory may be created in a read-only root"
        );
    }

    /// #475: editing an existing file inside a read-only root is refused at the
    /// atomic-write choke point (`write_atomic_bytes_with_permissions`), leaving
    /// the original bytes intact.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn edit_denied_in_read_only_root() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("refrepo");
        std::fs::create_dir_all(&ro).unwrap();
        let path = ro.join("a.txt");
        std::fs::write(&path, "alpha beta\n").unwrap();

        let ro_canon = crate::core::pathjail::canonicalize_or_self(&ro);
        crate::test_env::set_var(
            "LEAN_CTX_READ_ONLY_ROOTS",
            ro_canon.to_string_lossy().as_ref(),
        );
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(&path, "alpha", "OMEGA", false, false),
        );
        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        assert!(
            result.contains("read-only"),
            "edit in a read-only root must be refused: {result}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha beta\n",
            "the file must be left untouched"
        );
    }

    /// #475 (the exact #464 regression): a caller-supplied `backup_path` must
    /// not be a side door into a read-only root. Even when the *target* file is
    /// writable, redirecting the pre-edit backup into a read-only root is denied
    /// — and because the backup is written first, the denial is fail-closed: the
    /// target keeps its original bytes and no backup is dropped in the root.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn backup_path_cannot_smuggle_writes_into_read_only_root() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("refrepo");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let target = work.join("a.txt"); // writable target, outside the RO root
        std::fs::write(&target, "alpha beta\n").unwrap();
        let smuggled = ro.join("leak.bak"); // attacker-chosen backup inside RO root

        let ro_canon = crate::core::pathjail::canonicalize_or_self(&ro);
        crate::test_env::set_var(
            "LEAN_CTX_READ_ONLY_ROOTS",
            ro_canon.to_string_lossy().as_ref(),
        );
        let mut params = mk_params(&target, "alpha", "OMEGA", false, false);
        params.backup = true;
        params.backup_path = Some(smuggled.to_string_lossy().to_string());
        let mut cache = SessionCache::new();
        let result = handle(&mut cache, &params);
        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        assert!(
            result.contains("read-only"),
            "a backup_path into a read-only root must be refused: {result}"
        );
        assert!(
            !smuggled.exists(),
            "no backup may be smuggled into a read-only root"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "alpha beta\n",
            "fail-closed: the writable target must be untouched when the backup is denied"
        );
    }

    /// #475 end-to-end via the *real* config mechanism a user would use:
    /// `read_only_roots` declared in `config.toml` (not the env var) must make
    /// `ctx_edit` refuse the write. Exercises the `Config::load()` → predicate →
    /// tool-denial chain.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn edit_denied_via_config_read_only_roots() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("refrepo");
        std::fs::create_dir_all(&ro).unwrap();
        let path = ro.join("a.txt");
        std::fs::write(&path, "alpha beta\n").unwrap();

        // Write the user-facing config.toml into the isolated config dir.
        let cfg_path = crate::core::config::Config::path().unwrap();
        if let Some(parent) = cfg_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        // TOML literal string ('...') — no escaping of the temp path needed.
        std::fs::write(
            &cfg_path,
            format!("read_only_roots = ['{}']\n", ro.to_string_lossy()),
        )
        .unwrap();

        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(&path, "alpha", "OMEGA", false, false),
        );

        assert!(
            result.contains("read-only"),
            "config-declared read_only_roots must deny the edit: {result}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha beta\n",
            "the file must be left untouched"
        );
    }

    // GH #459: parent dir read-only, file inode writable (the bind-mount
    // sandbox shape). The atomic tempfile + rename needs *directory* write
    // permission and fails; the in-place fallback overwrites the existing inode
    // and succeeds. Skipped under root, which bypasses the directory permission
    // check (the atomic path would then succeed and the fallback never runs —
    // the write still lands correctly either way).
    #[cfg(unix)]
    #[test]
    fn write_falls_back_on_readonly_parent_dir() {
        use std::os::unix::fs::PermissionsExt;

        // SAFETY: geteuid() takes no arguments and only reads the caller's uid.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        std::fs::write(&path, b"hello").unwrap();

        // r-x parent: create_new tempfile + rename fail with EACCES, but the
        // existing file mode (0o644) still allows O_WRONLY|O_TRUNC.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let res = write_atomic_bytes_with_permissions(&path, b"world", None);

        // Restore so tempdir cleanup can remove the directory.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(res.is_ok(), "in-place fallback should succeed: {res:?}");
        assert_eq!(std::fs::read(&path).unwrap(), b"world");
    }

    // GH #459 end-to-end: the full ctx_edit flow (read -> preimage -> write)
    // must succeed when the parent dir is read-only but the file is writable.
    #[cfg(unix)]
    #[test]
    fn handle_edit_succeeds_on_readonly_parent_dir() {
        use std::os::unix::fs::PermissionsExt;

        // SAFETY: geteuid() takes no arguments and only reads the caller's uid.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.jsonc");
        std::fs::write(&path, "hello world\n").unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(&path, "hello", "goodbye", false, false),
        );

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            result.contains('✓'),
            "edit should succeed via in-place fallback: {result}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "goodbye world\n");
    }

    #[test]
    fn unique_match_succeeds() {
        let f = make_temp("fn main() {\n    let x = 42;\n}\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(f.path(), "let x = 42", "let x = 99", false, false),
        );
        assert!(result.contains("✓"));
        assert!(result.contains("1 replacement"));
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(content.contains("let x = 99"));
    }

    #[test]
    fn crlf_file_with_lf_search() {
        let f = make_temp("line1\r\nline2\r\nline3\r\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(f.path(), "line1\nline2", "changed1\nchanged2", false, false),
        );
        assert!(result.contains("✓"), "CRLF fallback should work: {result}");
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(
            content.contains("changed1\r\nchanged2"),
            "new_string should be adapted to CRLF: {content:?}"
        );
        assert!(
            content.contains("\r\nline3\r\n"),
            "rest of file should keep CRLF: {content:?}"
        );
    }

    #[test]
    fn lf_file_with_crlf_search() {
        let f = make_temp("line1\nline2\nline3\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(f.path(), "line1\r\nline2", "a\r\nb", false, false),
        );
        assert!(result.contains("✓"), "LF fallback should work: {result}");
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(
            content.contains("a\nb"),
            "new_string should be adapted to LF: {content:?}"
        );
    }

    #[test]
    fn trailing_whitespace_tolerance() {
        let f = make_temp("  let x = 1;  \n  let y = 2;\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(
                f.path(),
                "  let x = 1;\n  let y = 2;",
                "  let x = 10;\n  let y = 20;",
                false,
                false,
            ),
        );
        assert!(
            result.contains("✓"),
            "trailing whitespace tolerance should work: {result}"
        );
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(content.contains("let x = 10;"));
        assert!(content.contains("let y = 20;"));
    }

    #[test]
    fn crlf_with_trailing_whitespace() {
        let f = make_temp("  const a = 1;  \r\n  const b = 2;\r\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(
                f.path(),
                "  const a = 1;\n  const b = 2;",
                "  const a = 10;\n  const b = 20;",
                false,
                false,
            ),
        );
        assert!(
            result.contains("✓"),
            "CRLF + trailing whitespace should work: {result}"
        );
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(content.contains("const a = 10;"));
        assert!(content.contains("const b = 20;"));
    }

    #[test]
    fn rejects_invalid_utf8_by_default() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&[0xff, 0xfe, 0xfd]).unwrap();
        let mut cache = SessionCache::new();
        let result = handle(&mut cache, &mk_params(f.path(), "a", "b", false, false));
        assert!(
            result.contains("not valid UTF-8"),
            "expected utf8 rejection, got: {result}"
        );
    }

    #[test]
    fn allows_lossy_utf8_only_when_enabled() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&[0xff, 0xfe, 0xfd]).unwrap();
        let mut cache = SessionCache::new();
        let mut p = mk_params(f.path(), "a", "b", false, false);
        p.allow_lossy_utf8 = true;
        let result = handle(&mut cache, &p);
        assert!(
            !result.contains("not valid UTF-8"),
            "lossy mode should avoid utf8 hard error, got: {result}"
        );
    }

    #[test]
    fn expected_blake3_mismatch_fails_without_writing() {
        let f = make_temp("aaa\n");
        let mut cache = SessionCache::new();
        let mut p = mk_params(f.path(), "aaa", "bbb", false, false);
        p.expected_blake3 = Some("deadbeef".to_string());
        let result = handle(&mut cache, &p);
        assert!(
            result.contains("preimage mismatch"),
            "expected preimage mismatch, got: {result}"
        );
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "aaa\n");
    }

    /// #1592: the receipt used to label its 64-hex digest `md5=`, sending a
    /// user off to verify a hash against three algorithms none of which could
    /// ever match. The digest is BLAKE3; the receipt now says so, and the
    /// mismatch error names the same algorithm.
    #[test]
    fn receipt_labels_the_digest_by_its_actual_algorithm() {
        let f = make_temp("aaa\n");
        let mut cache = SessionCache::new();
        let out = handle(&mut cache, &mk_params(f.path(), "aaa", "bbb", false, false));

        assert!(out.contains("blake3="), "receipt must name BLAKE3: {out}");
        assert!(
            !out.contains("md5="),
            "no `md5=` label may survive on a BLAKE3 digest: {out}"
        );

        let digest = out
            .lines()
            .find(|l| l.starts_with("postimage:"))
            .and_then(|l| l.split("blake3=").nth(1))
            .expect("receipt carries a postimage blake3 field");
        assert_eq!(digest.len(), 64, "BLAKE3 is 64 hex chars, got {digest:?}");
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            digest,
            crate::core::hasher::hash_hex(b"bbb\n"),
            "the postimage digest must be BLAKE3 of the bytes on disk"
        );

        let mut p = mk_params(f.path(), "bbb", "ccc", false, false);
        p.expected_blake3 = Some("deadbeef".to_string());
        let err = handle(&mut cache, &p);
        assert!(err.contains("expected_blake3="), "{err}");
        assert!(err.contains("actual_blake3="), "{err}");
    }

    #[test]
    fn backup_is_created_when_enabled() {
        let f = make_temp("aaa\n");
        let mut cache = SessionCache::new();
        let mut p = mk_params(f.path(), "aaa", "bbb", false, false);
        p.backup = true;
        let out = handle(&mut cache, &p);
        assert!(out.contains("backup:"), "expected backup path, got: {out}");
        let bp = out
            .lines()
            .find_map(|l| l.strip_prefix("backup: "))
            .expect("backup line");
        let backup_content = std::fs::read_to_string(bp).unwrap();
        assert_eq!(backup_content, "aaa\n");
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(content, "bbb\n");
    }

    #[test]
    fn evidence_diff_is_emitted_when_enabled() {
        let f = make_temp("line1\nline2\n");
        let mut cache = SessionCache::new();
        let mut p = mk_params(f.path(), "line2", "changed2", false, false);
        p.evidence = true;
        p.diff_max_lines = 50;
        let out = handle(&mut cache, &p);
        assert!(out.contains("```diff"), "expected diff fence, got: {out}");
        assert!(
            out.contains("preimage:"),
            "expected preimage metadata, got: {out}"
        );
        assert!(
            out.contains("postimage:"),
            "expected postimage metadata, got: {out}"
        );
    }

    /// Issue #320: run_io performs the full edit without any cache handle, so the
    /// MCP layer can avoid holding the global cache write-lock across disk I/O.
    /// A successful edit reports an Invalidate effect.
    #[test]
    fn run_io_success_reports_invalidate_effect() {
        let f = make_temp("fn main() {\n    let x = 42;\n}\n");
        let (text, effect) = run_io(
            &mk_params(f.path(), "let x = 42", "let x = 99", false, false),
            "",
        );
        assert!(text.contains("✓"), "expected success: {text}");
        assert!(
            matches!(effect, CacheEffect::Invalidate),
            "successful edit must invalidate the cache entry"
        );
        let content = std::fs::read_to_string(f.path()).unwrap();
        assert!(content.contains("let x = 99"));
    }

    #[test]
    fn run_io_failure_reports_no_cache_effect() {
        let f = make_temp("some content\n");
        let (text, effect) = run_io(&mk_params(f.path(), "nonexistent", "x", false, false), "");
        assert!(text.contains("ERROR: old_string not found"));
        assert!(
            matches!(effect, CacheEffect::None),
            "a failed edit must not mutate the cache"
        );
    }

    /// Issue #320: concurrent edits to *different* files must all succeed without
    /// serializing on any shared lock — run_io takes no cache, so there is nothing
    /// global to contend on.
    #[test]
    fn run_io_concurrent_edits_to_different_files_all_succeed() {
        use std::sync::Arc;
        let dir = Arc::new(tempfile::tempdir().unwrap());
        let n = 16;
        let mut paths = Vec::new();
        for i in 0..n {
            let p = dir.path().join(format!("file_{i}.txt"));
            std::fs::write(&p, format!("value = {i}\n")).unwrap();
            paths.push(p);
        }
        let barrier = Arc::new(std::sync::Barrier::new(n));
        let mut handles = Vec::new();
        for (i, p) in paths.into_iter().enumerate() {
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let (text, effect) = run_io(
                    &mk_params(
                        &p,
                        &format!("value = {i}"),
                        &format!("value = {}", i + 1000),
                        false,
                        false,
                    ),
                    "",
                );
                assert!(text.contains("✓"), "edit {i} failed: {text}");
                assert!(matches!(effect, CacheEffect::Invalidate));
                (p, i)
            }));
        }
        for h in handles {
            let (p, i) = h.join().unwrap();
            let content = std::fs::read_to_string(&p).unwrap();
            assert_eq!(content, format!("value = {}\n", i + 1000));
        }
    }

    #[test]
    fn run_io_escalation_reports_store_full_effect() {
        // A file previously read in a compressed mode ("signatures") triggers
        // auto-escalation when old_string is not found: the full content is
        // returned for re-store.
        let f = make_temp("line a\nline b\nline c\n");
        let (text, effect) = run_io(
            &mk_params(f.path(), "definitely-not-present", "x", false, false),
            "signatures",
        );
        assert!(
            text.contains("[auto-escalation]"),
            "expected escalation: {text}"
        );
        match effect {
            CacheEffect::StoreFull(content) => {
                assert!(content.contains("line a") && content.contains("line c"));
            }
            _ => panic!("escalation must report a StoreFull cache effect"),
        }
    }

    #[test]
    fn apply_cache_effect_invalidate_and_store() {
        let f = make_temp("hello\n");
        let mut cache = SessionCache::new();
        cache.store(&f.path().to_string_lossy(), "hello\n");
        apply_cache_effect(
            &mut cache,
            &f.path().to_string_lossy(),
            CacheEffect::Invalidate,
        );
        assert!(
            cache.get(&f.path().to_string_lossy()).is_none(),
            "Invalidate must drop the entry"
        );
        apply_cache_effect(
            &mut cache,
            &f.path().to_string_lossy(),
            CacheEffect::StoreFull("fresh\n".to_string()),
        );
        assert!(
            cache.get(&f.path().to_string_lossy()).is_some(),
            "StoreFull must re-populate the entry"
        );
    }

    #[test]
    fn identical_old_new_rejected() {
        let f = make_temp("fn main() {}\n");
        let mut cache = SessionCache::new();
        let result = handle(
            &mut cache,
            &mk_params(f.path(), "fn main() {}", "fn main() {}", false, false),
        );
        assert!(result.contains("identical"));
    }

    #[test]
    fn edit_already_applied_detected() {
        let f = make_temp("fn updated() {}\n");
        let (text, effect) = run_io(
            &mk_params(
                f.path(),
                "fn original() {}",
                "fn updated() {}",
                false,
                false,
            ),
            "",
        );
        assert!(text.contains("already exists"));
        assert!(text.contains("already applied"));
        assert!(matches!(effect, CacheEffect::None));
    }

    #[test]
    fn closest_line_hint_shown() {
        let f = make_temp("  fn hello() {\n    println!(\"hi\");\n  }\n");
        let (text, _) = run_io(
            &mk_params(f.path(), "fn hello(){", "fn hello_world(){", false, false),
            "",
        );
        assert!(text.contains("Closest match at line"));
    }

    #[test]
    fn missing_file_suggests_relocated_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/new")).unwrap();
        std::fs::write(dir.path().join("src/new/gizmo.rs"), "fn gizmo() {}\n").unwrap();

        let (text, effect) = run_io(
            &mk_params(
                &dir.path().join("src/old/gizmo.rs"),
                "fn gizmo() {}",
                "fn gizmo2() {}",
                false,
                false,
            ),
            "",
        );
        assert!(text.contains("same-named file was found"), "got: {text}");
        assert!(text.contains("gizmo.rs"), "got: {text}");
        assert!(matches!(effect, CacheEffect::None));
    }

    #[test]
    fn old_string_in_other_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let target = dir.path().join("a.rs");
        std::fs::write(&target, "fn unrelated_a() {}\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn the_target_symbol() {}\n").unwrap();

        let (text, _) = run_io(
            &mk_params(
                &target,
                "fn the_target_symbol() {}",
                "fn renamed() {}",
                false,
                false,
            ),
            "",
        );
        assert!(text.contains("matching line exists in"), "got: {text}");
        assert!(text.contains("b.rs"), "got: {text}");
    }

    // P0-6 (#418): a symlink at the edit path must be rejected on the read side —
    // a link planted inside the jail could otherwise read/overwrite outside it.
    #[cfg(unix)]
    #[test]
    fn editing_through_a_symlink_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.rs");
        std::fs::write(&real, "fn old() {}\n").unwrap();
        let link = dir.path().join("link.rs");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let (text, effect) = run_io(
            &mk_params(&link, "fn old() {}", "fn new() {}", false, false),
            "",
        );
        assert!(text.contains("symlink"), "got: {text}");
        assert!(matches!(effect, CacheEffect::None));
        // Target untouched.
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "fn old() {}\n");
    }

    // P0-6 (#418): the write side must also reject a symlink destination
    // (defense in depth for create-mode and backup paths).
    #[cfg(unix)]
    #[test]
    fn creating_over_a_symlink_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("victim.txt");
        std::fs::write(&real, "precious").unwrap();
        let link = dir.path().join("innocent.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let (text, _) = run_io(&mk_params(&link, "", "overwritten", false, true), "");
        assert!(
            text.contains("symlink") || text.contains("ERROR"),
            "got: {text}"
        );
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "precious",
            "symlink target must not be modified"
        );
    }

    #[test]
    fn regular_file_edit_still_works_after_symlink_guard() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("normal.rs");
        std::fs::write(&file, "fn old() {}\n").unwrap();

        let (text, _) = run_io(
            &mk_params(&file, "fn old() {}", "fn new() {}", false, false),
            "",
        );
        assert!(
            text.contains("Edit applied") || !text.starts_with("ERROR"),
            "got: {text}"
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "fn new() {}\n");
    }
}
