use super::*;

// ---------------------------------------------------------------------------
// #361 anti-inflation invariant: a `ctx_read` must never cost more tokens than
// the raw file. The framing header only earns its keep on large files and
// cached re-reads; on a cold read of a small file it is pure overhead, so the
// guard ships bare content (break-even, never a loss). The guard now applies to
// every mode — auto-resolved AND explicitly requested — so no view can ever
// cost more tokens than reading the file raw.
// ---------------------------------------------------------------------------

#[test]
fn cap_to_raw_falls_back_when_framing_inflates() {
    let raw = "pub fn a() {}\n";
    let framed = format!("F1=x.rs 1L\n deps foo,bar\n{raw}");
    let raw_tokens = count_tokens(raw);
    let framed_tokens = count_tokens(&framed);
    assert!(
        framed_tokens > raw_tokens,
        "fixture must inflate to exercise the guard"
    );
    assert_eq!(
        cap_to_raw(framed, framed_tokens, raw, raw_tokens, "signatures"),
        raw,
        "framing larger than raw must fall back to bare content — and on a file\n         this small the notice is suppressed so the #361 cap still holds"
    );
}

#[test]
fn cap_to_raw_keeps_framing_when_not_larger() {
    let raw = "a long original body that compresses well";
    let framed = "sig summary".to_string();
    let framed_tokens = count_tokens(&framed);
    assert_eq!(
        cap_to_raw(framed.clone(), framed_tokens, raw, 100, "signatures"),
        framed,
        "output at or below raw must be returned untouched"
    );
}

#[test]
fn cap_to_raw_keeps_framing_for_empty_file() {
    // An empty file has zero content tokens; keep the framing so the reader
    // still gets an "empty / 0L" signal rather than a blank payload.
    let framed = "F1=empty.rs 0L".to_string();
    let framed_tokens = count_tokens(&framed);
    assert_eq!(
        cap_to_raw(framed.clone(), framed_tokens, "", 0, "signatures"),
        framed,
        "empty files keep their framing signal"
    );
}

// ---------------------------------------------------------------------------
// #1587: the cap must not degrade *silently*. When a compressed view collapses
// to full content on a file large enough for that to cost real tokens, the
// caller is told; a verbatim `full` request is never labelled (nothing was
// withheld), and small files stay bare so #361 above is not undone.
// ---------------------------------------------------------------------------

/// Raw body comfortably above `NO_COMPRESSION_BANNER_MIN_TOKENS`.
fn big_raw() -> String {
    "pub fn item(value: u32) -> u32 { value.saturating_add(1) }\n".repeat(120)
}

#[test]
fn cap_to_raw_labels_silent_fallback_for_compressed_request() {
    let raw = big_raw();
    let raw_tokens = count_tokens(&raw);
    assert!(
        raw_tokens >= crate::tools::ctx_read::render::NO_COMPRESSION_BANNER_MIN_TOKENS,
        "fixture must exceed the banner threshold"
    );
    let framed = format!("F1=big.rs 120L\n{raw}");
    let framed_tokens = count_tokens(&framed);
    let out = cap_to_raw(framed, framed_tokens, &raw, raw_tokens, "signatures");
    assert!(
        out.starts_with("[lean-ctx] no compression applied (mode=signatures)"),
        "a summary request answered with the whole file must say so: {}",
        out.lines().next().unwrap_or_default()
    );
    assert!(
        out.ends_with(&raw),
        "the full content still follows the notice"
    );
}

#[test]
fn cap_to_raw_stays_silent_for_verbatim_full_request() {
    let raw = big_raw();
    let raw_tokens = count_tokens(&raw);
    let framed = format!("F1=big.rs 120L\n{raw}");
    let framed_tokens = count_tokens(&framed);
    assert_eq!(
        cap_to_raw(framed, framed_tokens, &raw, raw_tokens, "full"),
        raw,
        "`full` asked for the whole file — stripping framing is not a degradation"
    );
}

#[test]
fn cap_to_raw_notice_never_breaks_the_361_cap() {
    // Small file: the notice would push the read above the raw cost it just
    // protected, so it is suppressed.
    let raw = "pub fn a() {}\n";
    let raw_tokens = count_tokens(raw);
    let framed = format!("F1=x.rs 1L\n deps foo,bar\n{raw}");
    let framed_tokens = count_tokens(&framed);
    let out = cap_to_raw(framed, framed_tokens, raw, raw_tokens, "map");
    assert_eq!(out, raw);
    assert!(count_tokens(&out) <= raw_tokens);
}

#[test]
fn auto_read_never_inflates_small_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small.rs");
    let p = path.to_string_lossy().to_string();
    let content =
        "use std::io;\n\npub fn greet(name: &str) -> String {\n    format!(\"hi {name}\")\n}\n";
    std::fs::write(&path, content).unwrap();

    let mut cache = SessionCache::new();
    let out = handle_with_task_resolved(&mut cache, &p, "auto", CrpMode::Off, None);
    assert!(
        out.output_tokens <= count_tokens(content),
        "auto cold read inflated a small file: {} output tok > {} raw tok\n{}",
        out.output_tokens,
        count_tokens(content),
        out.content
    );
}

#[test]
fn full_read_never_inflates_tiny_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tiny.rs");
    let p = path.to_string_lossy().to_string();
    let content = "pub fn a() {}\n";
    std::fs::write(&path, content).unwrap();

    let mut cache = SessionCache::new();
    let out = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    assert!(
        out.output_tokens <= count_tokens(content),
        "full cold read inflated a tiny file: {} > {}\n{}",
        out.output_tokens,
        count_tokens(content),
        out.content
    );
}

#[test]
fn auto_read_still_compresses_large_file() {
    // Isolate learning state so the resolver falls through to the size
    // heuristic (large code file → map), proving the guard never blocks a
    // genuine compression win.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.rs");
    let p = path.to_string_lossy().to_string();
    let mut content = String::new();
    for i in 0..400 {
        content.push_str(&format!(
            "pub fn function_number_{i}(x: i32, y: i32) -> i32 {{\n    let z = x + y + {i};\n    z * 2\n}}\n\n"
        ));
    }
    std::fs::write(&path, &content).unwrap();

    let mut cache = SessionCache::new();
    let out = handle_with_task_resolved(&mut cache, &p, "auto", CrpMode::Off, None);
    assert!(
        out.output_tokens < count_tokens(&content),
        "auto read of a large file must still compress: {} >= {} (mode={})",
        out.output_tokens,
        count_tokens(&content),
        out.resolved_mode
    );
}

#[test]
fn explicit_compressed_mode_capped_on_tiny_file() {
    // #361 now applies to explicit modes too: asking for `signatures` of a tiny
    // file must never cost more tokens than reading it raw. (On a tiny file the
    // capped result is the raw content, which still carries the symbols.)
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lib.rs");
    let p = path.to_string_lossy().to_string();
    let content = "pub fn alpha() {}\npub fn beta() {}\n";
    std::fs::write(&path, content).unwrap();

    let mut cache = SessionCache::new();
    let out = handle_with_task_resolved(&mut cache, &p, "signatures", CrpMode::Off, None);
    assert!(
        out.output_tokens <= count_tokens(content),
        "explicit signatures of a tiny file must not inflate past raw: {} > {}\n{}",
        out.output_tokens,
        count_tokens(content),
        out.content
    );
}

#[test]
fn explicit_signatures_still_compresses_large_file() {
    // Capping explicit modes must not break legitimate compression: signatures
    // of a large file are far smaller than raw, so the cap is a no-op and the
    // compressed view (not raw) is returned.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.rs");
    let p = path.to_string_lossy().to_string();
    let mut content = String::new();
    for i in 0..400 {
        content.push_str(&format!(
            "pub fn function_number_{i}(x: i32, y: i32) -> i32 {{\n    let z = x + y + {i};\n    z * 2\n}}\n\n"
        ));
    }
    std::fs::write(&path, &content).unwrap();

    let mut cache = SessionCache::new();
    let out = handle_with_task_resolved(&mut cache, &p, "signatures", CrpMode::Off, None);
    assert!(
        out.output_tokens < count_tokens(&content),
        "explicit signatures of a large file must compress: {} >= {}",
        out.output_tokens,
        count_tokens(&content)
    );
}

#[test]
fn cache_hit_stub_is_byte_stable_across_rereads() {
    // #498 determinism: re-reading an unchanged file must yield byte-identical
    // output (no read-count note, no rotating proof line) so provider prompt
    // caching applies to the repeated stub.
    // High threshold so full-delivery degradation (#E26-P3) doesn't fire.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let _env_lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LCTX_FULL_DEGRADATION_THRESHOLD", "100");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stable.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "pub fn alpha() {}\npub fn beta() {}\n").unwrap();

    let mut cache = SessionCache::new();
    // Prime: the first full read marks full content as delivered.
    let _ = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    let r2 = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    let r3 = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    let r4 = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    assert_eq!(
        r2.content, r3.content,
        "re-read drifted between reads 2 and 3"
    );
    assert_eq!(
        r3.content, r4.content,
        "re-read drifted between reads 3 and 4"
    );
    assert!(
        !r2.content.contains("(read"),
        "read-count note must not appear in the cache-hit body: {}",
        r2.content
    );
}

// ---------------------------------------------------------------------------

#[test]
fn compress_protect_glob_forces_full_verbatim_read() {
    // #1150: a path matching a `compress_protect` glob is returned verbatim even
    // when an aggressive mode is requested. Control + treatment in one test: the
    // unprotected read strips comments, the protected read keeps every byte.
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "0");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("protected.rs");
    let p = path.to_string_lossy().to_string();
    // Large enough that aggressive compression genuinely strips the comments
    // rather than falling back to raw via the anti-inflation cap.
    let mut content = String::new();
    for i in 0..60 {
        content.push_str(&format!(
            "// distinctive-comment-{i}\npub fn handler_{i}(x: u32) -> u32 {{ x + {i} }}\n"
        ));
    }
    std::fs::write(&path, &content).unwrap();

    // Control: with nothing protected, aggressive strips the comments.
    crate::core::config::Config::update_global(|c| c.proxy.compress_protect = None).unwrap();
    let mut cold = SessionCache::new();
    let stripped = handle_with_task_resolved(&mut cold, &p, "aggressive", CrpMode::Off, None);
    assert!(
        !stripped.content.contains("// distinctive-comment-0"),
        "control: aggressive must strip comments when the path is not protected"
    );

    // Treatment: protect *.rs → the same aggressive read returns the file in full.
    crate::core::config::Config::update_global(|c| {
        c.proxy.compress_protect = Some(vec!["*.rs".into()]);
    })
    .unwrap();
    let mut warm = SessionCache::new();
    let protected = handle_with_task_resolved(&mut warm, &p, "aggressive", CrpMode::Off, None);
    assert!(
        protected.content.contains("// distinctive-comment-0")
            && protected.content.contains("// distinctive-comment-59"),
        "a protected path must be returned verbatim with every comment intact"
    );

    crate::core::config::Config::update_global(|c| c.proxy.compress_protect = None).unwrap();
    crate::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
}
