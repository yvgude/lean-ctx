// SPDX-License-Identifier: Apache-2.0
use super::*;

// -- Regression: GitHub Issue #775 --
// After a full-file read is cached, a subsequent ranged read with `lines:N-M`
// must return only the requested window — not the full file content again.

/// Helper: create a test file with `n` numbered lines ("line 1\nline 2\n…").
fn write_numbered_file(dir: &std::path::Path, name: &str, n: usize) -> String {
    let path = dir.join(name);
    let content: String = (1..=n)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, &content).unwrap();
    path.to_string_lossy().to_string()
}

#[test]
fn gh775_full_then_ranged_returns_only_window() {
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    lean_ctx::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "0");

    let dir = tempfile::tempdir().unwrap();
    let p = write_numbered_file(dir.path(), "big.ts", 2000);

    let mut cache = SessionCache::new();

    // 1. Full read — delivers all 2000 lines, marks as fully delivered.
    let full = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    assert!(
        full.content.contains("line 1"),
        "full read must include first line"
    );
    assert!(
        full.content.contains("line 2000"),
        "full read must include last line"
    );

    // 2. Ranged read — must return ONLY lines 1480–1489.
    let ranged =
        handle_fresh_with_task_resolved(&mut cache, &p, "lines:1480-1489", CrpMode::Off, None);
    assert!(
        ranged.content.contains("line 1480"),
        "ranged read must contain the first requested line:\n{}",
        &ranged.content[..ranged.content.len().min(300)]
    );
    assert!(
        ranged.content.contains("line 1489"),
        "ranged read must contain the last requested line"
    );
    assert!(
        !ranged.content.contains("line 1\n") && !ranged.content.contains("line 2000"),
        "ranged read must NOT contain lines outside the window:\n{}",
        &ranged.content[..ranged.content.len().min(500)]
    );

    lean_ctx::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
}

#[test]
fn gh775_full_then_ranged_with_fresh_returns_only_window() {
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    lean_ctx::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "0");

    let dir = tempfile::tempdir().unwrap();
    let p = write_numbered_file(dir.path(), "big2.ts", 2000);

    let mut cache = SessionCache::new();

    // 1. Full read.
    handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);

    // 2. Fresh ranged read (simulates `fresh:true` in the tool args).
    let ranged =
        handle_fresh_with_task_resolved(&mut cache, &p, "lines:1480-1489", CrpMode::Off, None);
    assert!(
        ranged.content.contains("line 1480"),
        "fresh ranged read must contain requested start line"
    );
    assert!(
        ranged.content.contains("line 1489"),
        "fresh ranged read must contain requested end line"
    );
    assert!(
        !ranged.content.contains("line 1\n") && !ranged.content.contains("line 2000"),
        "fresh ranged read must NOT contain lines outside the window"
    );

    lean_ctx::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
}

#[test]
fn gh775_ranged_response_starts_at_requested_line() {
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    lean_ctx::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "0");

    let dir = tempfile::tempdir().unwrap();
    let p = write_numbered_file(dir.path(), "big3.ts", 2000);

    let mut cache = SessionCache::new();

    // Full read to warm cache.
    handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);

    // Ranged read.
    let ranged =
        handle_fresh_with_task_resolved(&mut cache, &p, "lines:500-504", CrpMode::Off, None);

    // The first numbered output line must be line 500.
    // extract_line_range formats as " 500| line 500".
    let body_lines: Vec<&str> = ranged
        .content
        .lines()
        .filter(|l| l.contains("| line "))
        .collect();
    assert!(
        !body_lines.is_empty(),
        "ranged read must contain numbered output lines"
    );
    assert!(
        body_lines[0].contains("500| line 500"),
        "first body line must be line 500, got: {}",
        body_lines[0]
    );
    assert_eq!(
        body_lines.len(),
        5,
        "lines:500-504 must return exactly 5 lines, got {}",
        body_lines.len()
    );

    lean_ctx::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
}

/// #1759: `lines:-N` — the mode the Bash-hook rewrite emits for `tail -N` —
/// must return the LAST N lines. Before the fix it did not parse at all; and
/// had the string reached the renderer, `split_once('-')` would have read `-3`
/// as the span `""-"3"` and returned the FIRST three lines.
#[test]
fn gh1759_tail_window_returns_last_n_lines() {
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    lean_ctx::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "0");

    let dir = tempfile::tempdir().unwrap();
    let p = write_numbered_file(dir.path(), "tail.ts", 40);
    let mut cache = SessionCache::new();

    let tail = handle_fresh_with_task_resolved(&mut cache, &p, "lines:-3", CrpMode::Off, None);
    let body_lines: Vec<&str> = tail
        .content
        .lines()
        .filter(|l| l.contains("| line "))
        .collect();
    assert_eq!(
        body_lines.len(),
        3,
        "lines:-3 must return exactly 3 lines, got {}",
        body_lines.len()
    );
    assert!(
        body_lines[0].contains("38| line 38"),
        "the window must start at line 38, got: {}",
        body_lines[0]
    );
    assert!(
        body_lines[2].contains("40| line 40"),
        "the window must end at the last line, got: {}",
        body_lines[2]
    );
    assert!(
        !tail.content.contains("invalid read mode"),
        "the tail form must parse: {}",
        tail.content
    );

    // A window larger than the file is the whole file, not an error.
    let all = handle_fresh_with_task_resolved(&mut cache, &p, "lines:-100", CrpMode::Off, None);
    assert_eq!(
        all.content
            .lines()
            .filter(|l| l.contains("| line "))
            .count(),
        40
    );

    lean_ctx::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
}

#[test]
fn gh1759_tail_part_inside_multi_select() {
    // The renderer resolves `-N` parts of a comma multi-select the same way.
    let content = (1..=10)
        .map(|i| format!("line {i}\n"))
        .collect::<Vec<_>>()
        .concat();
    let out = crate::tools::ctx_read::render::extract_line_range(&content, "1-2,-2");
    let picked: Vec<&str> = out.lines().collect();
    assert_eq!(picked.len(), 4, "{out}");
    assert!(picked[0].contains("1| line 1"));
    assert!(picked[1].contains("2| line 2"));
    assert!(picked[2].contains("9| line 9"));
    assert!(picked[3].contains("10| line 10"));
}

#[test]
fn gh775_cold_ranged_read_returns_only_window() {
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    lean_ctx::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "0");

    let dir = tempfile::tempdir().unwrap();
    let p = write_numbered_file(dir.path(), "cold.ts", 2000);

    let mut cache = SessionCache::new();

    // No prior full read — cold ranged read must still return only the window.
    let ranged = handle_with_task_resolved(&mut cache, &p, "lines:100-109", CrpMode::Off, None);
    assert!(
        ranged.content.contains("line 100"),
        "cold ranged read must contain start line"
    );
    assert!(
        ranged.content.contains("line 109"),
        "cold ranged read must contain end line"
    );
    assert!(
        !ranged.content.contains("line 2000"),
        "cold ranged read must NOT contain lines outside window"
    );

    lean_ctx::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
}

// ---------------------------------------------------------------------------
// #811: the `anchored:N-M` disk-streaming short-circuit. A fresh, explicitly
// windowed anchored read must never materialize the whole file — these tests
// exercise `read_line_window`/`parse_disk_anchor_range` directly, and the
// end-to-end case that motivated it: a file bigger than `LCTX_MAX_READ_BYTES`
// must still serve a small anchored window instead of erroring "file too
// large" (the bug that `read_file_lossy`'s own error message told the caller
// to route around, but couldn't actually satisfy before this fix).
// ---------------------------------------------------------------------------

#[test]
fn parse_disk_anchor_range_accepts_dash_form_only() {
    assert_eq!(parse_disk_anchor_range("5-10"), Some((5, 10)));
    assert_eq!(parse_disk_anchor_range("1-999999"), Some((1, 999_999)));
    // Bare "N" (meaning "to EOF") needs a known total to resolve — the
    // streaming path doesn't have one up front, so it declines rather than
    // guess, and the caller falls back to the full-read path.
    assert_eq!(parse_disk_anchor_range("5"), None);
    assert_eq!(parse_disk_anchor_range("not-a-range"), None);
}

#[test]
fn read_line_window_streams_only_the_requested_span() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("window.txt");
    let body = (1..=50)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, format!("{body}\n")).unwrap();
    let p = path.to_string_lossy().to_string();

    let window = read_line_window(&p, 10, 12).expect("streamed read must succeed");
    assert_eq!(window.total_lines, 50);
    assert_eq!(window.start, 10);
    assert_eq!(window.end, 12);
    assert_eq!(window.body, "line 10\nline 11\nline 12");
}
