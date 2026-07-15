//! Windowed-read tests extracted from tests.rs (#660 LOC gate, frozen limit).
use super::*;

#[test]
fn read_line_window_clamps_end_to_eof() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("short.txt");
    std::fs::write(&path, "a\nb\nc\n").unwrap();
    let p = path.to_string_lossy().to_string();

    let window = read_line_window(&p, 2, 999_999).expect("streamed read must succeed");
    assert_eq!(window.total_lines, 3);
    assert_eq!(
        window.end, 3,
        "end must clamp to the real EOF, not the sentinel"
    );
    assert_eq!(window.body, "b\nc");
}

/// The end-to-end regression case: a file over `LCTX_MAX_READ_BYTES` must
/// still serve a bounded `anchored:N-M` read. Before #811's disk-streaming
/// short-circuit, `handle_with_options_inner` always called `read_file_lossy`
/// first regardless of mode, so a window request on an oversized file failed
/// with the same "file too large" error as a `full` read — even though
/// `read_file_lossy`'s own error message recommends a line-range read as the
/// escape hatch.
#[test]
fn disk_windowed_anchored_read_serves_file_over_the_size_cap() {
    let _lock = crate::core::data_dir::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.rs");
    let body = (1..=500)
        .map(|i| format!("fn function_number_{i}() {{}}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, format!("{body}\n")).unwrap();
    let p = path.to_string_lossy().to_string();
    let real_size = std::fs::metadata(&path).unwrap().len();

    crate::test_env::set_var("LCTX_MAX_READ_BYTES", "512");
    assert!(
        real_size > 512,
        "fixture must exceed the test cap to exercise the regression"
    );

    // Sanity: an ordinary full read of the oversized file is rejected.
    let full_err = read_file_lossy(&p);
    assert!(full_err.is_err(), "full read must hit the size cap");

    let mut cache = SessionCache::new();
    let out = handle_with_options_inner(
        &mut cache,
        &p,
        "anchored:5-7",
        /* fresh */ true,
        CrpMode::Off,
        None,
        ReadTuning::default(),
        None,
    );
    crate::test_env::remove_var("LCTX_MAX_READ_BYTES");

    assert_eq!(out.resolved_mode, "anchored:5-7");
    assert!(
        out.content.contains("function_number_5") && out.content.contains("function_number_7"),
        "must contain the requested window: {}",
        out.content
    );
    assert!(
        !out.content.contains("function_number_1()")
            && !out.content.contains("function_number_500"),
        "must NOT contain lines outside the window: {}",
        out.content
    );
    assert!(
        out.content.contains("500L"),
        "header must report the file's true total line count: {}",
        out.content
    );
    assert!(
        !out.content.to_lowercase().contains("too large")
            && !out.content.to_lowercase().contains("error"),
        "must not surface the size-cap error for a bounded window: {}",
        out.content
    );
}

/// #875: a windowed `anchored:N-M` read served through the two-phase
/// (`preread`-supplied) slow path must still return only that window with its
/// `N:hh|` hash anchors — never fall through to a full-file dump. The
/// disk-streaming short-circuit (#811) only fires on the fast path, where no
/// preread is in hand; the slow path (spawned-thread read under lock
/// contention) hands `handle_with_options_inner` the whole file as `preread`,
/// so `try_disk_anchored_window` bows out and the window must instead be cut
/// and anchored by `process_mode_tuned`. Before the anchored:N-M arm existed
/// there, this path dumped the entire file with no anchors — the exact symptom
/// #875 reported after the first windowed read.
#[test]
fn windowed_anchored_read_via_preread_slow_path_stays_windowed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.rs");
    let body = (1..=200)
        .map(|i| format!("fn function_number_{i}() {{}}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, format!("{body}\n")).unwrap();
    let p = path.to_string_lossy().to_string();
    let preread = std::fs::read_to_string(&path).unwrap();

    let mut cache = SessionCache::new();
    let out = handle_with_options_inner(
        &mut cache,
        &p,
        "anchored:109-118",
        /* fresh */ true,
        CrpMode::Off,
        None,
        ReadTuning::default(),
        Some(preread),
    );

    assert_eq!(out.resolved_mode, "anchored:109-118");
    assert!(
        out.content.contains("function_number_109") && out.content.contains("function_number_118"),
        "must contain the requested window: {}",
        out.content
    );
    assert!(
        !out.content.contains("function_number_50") && !out.content.contains("function_number_200"),
        "must NOT dump lines outside the window: {}",
        out.content
    );
    // Each served line keeps its `N:hh|` hash anchor (edit-ready for ctx_patch).
    assert!(
        out.content.contains("\n109:") && out.content.contains("118:"),
        "windowed lines must keep N:hh| anchors: {}",
        out.content
    );
    assert!(
        out.content.contains("200L"),
        "header must report the file's true total line count: {}",
        out.content
    );
}
