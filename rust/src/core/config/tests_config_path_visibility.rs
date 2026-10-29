use super::*;

// #540: `missing_config_path()` is the visible signal that the runtime
// resolved a `config.toml` that doesn't exist — i.e. an edit landed in a
// different file (XDG vs legacy dir, or a sandboxed HOME). The block messages
// turn this into an over-MCP-visible note.
#[test]
fn missing_config_path_flags_absent_then_clears_when_present() {
    let _lock = crate::core::data_dir::test_env_lock();
    let saved = std::env::var("LEAN_CTX_CONFIG_DIR").ok();
    let dir = tempfile::tempdir().unwrap();
    crate::test_env::set_var("LEAN_CTX_CONFIG_DIR", dir.path());

    // No config.toml in the resolved dir → flagged as missing (on defaults).
    assert_eq!(
        Config::missing_config_path(),
        Some(dir.path().join("config.toml"))
    );

    // Create the file → the runtime now has a real config, so no flag.
    std::fs::write(dir.path().join("config.toml"), "ultra_compact = true\n").unwrap();
    assert!(Config::missing_config_path().is_none());

    crate::test_env::remove_var("LEAN_CTX_CONFIG_DIR");
    if let Some(v) = saved {
        crate::test_env::set_var("LEAN_CTX_CONFIG_DIR", v);
    }
}

/// A global config that exists but cannot be used must mark the very snapshot
/// that stands in for it. The process-wide parse-error diagnostic is cleared by
/// the next good load; a decision made on the earlier snapshot must not be.
#[test]
fn unusable_global_config_marks_its_own_snapshot() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    let _lock = crate::core::data_dir::test_env_lock();
    let saved_dir = std::env::var("LEAN_CTX_CONFIG_DIR").ok();
    let saved_profile = std::env::var("LEAN_CTX_CONFIG_PROFILE").ok();
    let dir = tempfile::tempdir().unwrap();
    crate::test_env::set_var("LEAN_CTX_CONFIG_DIR", dir.path());
    crate::test_env::remove_var("LEAN_CTX_CONFIG_PROFILE");
    let file = dir.path().join("config.toml");
    // A plain setting whose default is `false` (not one of the legacy keys the
    // loader migrates away, which would not read back).
    let good = "shell_strict_mode = true\n[profiles.relaxed]\nshell_strict_mode = false\n";

    // Clean install: no file is not a failure.
    assert_eq!(Config::load_arc().load_failure, None);

    std::fs::write(&file, "[broken").unwrap();
    let broken = Config::load_arc();
    assert_eq!(broken.load_failure, Some(ConfigLoadFailure::Invalid));
    // Same bytes again: the cached snapshot carries the marker too.
    assert_eq!(
        Config::load_arc().load_failure,
        Some(ConfigLoadFailure::Invalid)
    );

    std::fs::write(&file, good).unwrap();
    let loaded = Config::load_arc();
    assert_eq!(loaded.load_failure, None);
    assert!(loaded.shell_strict_mode);
    assert!(last_config_parse_error().is_none());
    assert_eq!(
        broken.load_failure,
        Some(ConfigLoadFailure::Invalid),
        "the earlier failed snapshot stays unusable after a later good load"
    );

    // Loads racing with edits: every snapshot is consistent in itself — the
    // operator's setting is applied exactly when no failure is recorded.
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let reads: Vec<_> = (0..4)
        .map(|_| std::sync::Arc::new(AtomicU64::new(0)))
        .collect();
    let readers: Vec<_> = reads
        .iter()
        .map(|count| {
            let (stop, count) = (std::sync::Arc::clone(&stop), std::sync::Arc::clone(count));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let snapshot = Config::load_arc();
                    assert_eq!(snapshot.load_failure.is_none(), snapshot.shell_strict_mode);
                    count.fetch_add(1, Ordering::Release);
                }
            })
        })
        .collect();
    // No timing assumption: the writer only goes on once EVERY reader has
    // completed a further read, so each edit is provably followed by reads on
    // all four threads. A reader that died is noticed instead of waited for.
    let every_reader_read_again = |readers: &[std::thread::JoinHandle<()>]| {
        let before: Vec<u64> = reads
            .iter()
            .map(|count| count.load(Ordering::Acquire))
            .collect();
        while reads
            .iter()
            .zip(&before)
            .any(|(count, seen)| count.load(Ordering::Acquire) == *seen)
        {
            assert!(
                readers.iter().all(|reader| !reader.is_finished()),
                "a reader thread ended early"
            );
            std::thread::yield_now();
        }
    };
    every_reader_read_again(&readers);
    for round in 0..40 {
        // Atomic replacement, so a reader never sees a half-written file.
        let next = dir.path().join("next.toml");
        std::fs::write(&next, if round % 2 == 0 { "[broken" } else { good }).unwrap();
        std::fs::rename(&next, &file).unwrap();
        every_reader_read_again(&readers);
    }
    stop.store(true, Ordering::Release);
    for reader in readers {
        reader.join().expect("reader thread");
    }

    // Read failures are not "no config".
    std::fs::write(&file, [b'u', b'=', 0xff, 0xfe, b'\n']).unwrap();
    assert_eq!(
        Config::load_arc().load_failure,
        Some(ConfigLoadFailure::Unreadable),
        "invalid UTF-8"
    );
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    assert_eq!(
        Config::load_arc().load_failure,
        Some(ConfigLoadFailure::Unreadable),
        "a directory in place of the file"
    );
    std::fs::remove_dir(&file).unwrap();

    // Profiles keep their meaning: a defined one applies, an undefined one fails.
    std::fs::write(&file, good).unwrap();
    crate::test_env::set_var("LEAN_CTX_CONFIG_PROFILE", "relaxed");
    let relaxed = Config::load_arc();
    assert_eq!(relaxed.load_failure, None);
    assert!(!relaxed.shell_strict_mode);
    crate::test_env::set_var("LEAN_CTX_CONFIG_PROFILE", "undefined-profile");
    assert_eq!(
        Config::load_arc().load_failure,
        Some(ConfigLoadFailure::Invalid)
    );

    match saved_profile {
        Some(value) => crate::test_env::set_var("LEAN_CTX_CONFIG_PROFILE", value),
        None => crate::test_env::remove_var("LEAN_CTX_CONFIG_PROFILE"),
    }
    match saved_dir {
        Some(value) => crate::test_env::set_var("LEAN_CTX_CONFIG_DIR", value),
        None => crate::test_env::remove_var("LEAN_CTX_CONFIG_DIR"),
    }
}
