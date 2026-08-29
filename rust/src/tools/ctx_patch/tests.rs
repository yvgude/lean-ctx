//! End-to-end tests for `ctx_patch` through the real read→guard→atomic-write
//! path (epic #1008). The pure line-model is tested in `apply.rs`; here we
//! verify on-disk behaviour, staleness rejection and batch atomicity.

use super::*;
use crate::core::anchor::line_hash;
use std::io::Write;
use tempfile::NamedTempFile;

fn make_temp(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

/// A temp file with a real `.rs` suffix so the tree-sitter syntax gate engages
/// (the plain `make_temp` files have no recognized extension → gate skipped).
fn make_temp_rs(content: &str) -> NamedTempFile {
    let mut f = tempfile::Builder::new().suffix(".rs").tempfile().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

fn params(path: &std::path::Path, ops: Vec<AnchorOp>) -> PatchParams {
    PatchParams {
        path: path.to_string_lossy().to_string(),
        ops,
        expected_blake3: None,
        backup: false,
        backup_path: None,
        evidence: false,
        diff_max_lines: 200,
        allow_lossy_utf8: false,
        validate_syntax: true,
    }
}

#[test]
fn set_line_applies_and_invalidates() {
    let f = make_temp("fn main() {\n    let x = 1;\n}\n");
    let (text, effect) = run_io(
        &params(
            f.path(),
            vec![AnchorOp::SetLine {
                line: 2,
                hash: line_hash("    let x = 1;"),
                new_text: "    let x = 2;".to_string(),
            }],
        ),
        "",
    );
    assert!(text.contains('✓'), "expected success: {text}");
    assert!(matches!(effect, CacheEffect::Invalidate));
    assert_eq!(
        std::fs::read_to_string(f.path()).unwrap(),
        "fn main() {\n    let x = 2;\n}\n"
    );
}

// BOM parity (GH #683 follow-up): `ctx_read` strips the UTF-8 BOM, so the
// anchor hash the model holds for line 1 of a BOM file is over the BOM-less
// text. The edit side must validate against the same view — and preserve the
// BOM on disk — or every line-1 edit of a BOM file conflicts forever.
#[test]
fn bom_file_line_one_edit_matches_read_side_hash_and_keeps_bom() {
    let f = make_temp("\u{feff}hello\nworld\n");
    let (text, effect) = run_io(
        &params(
            f.path(),
            vec![AnchorOp::SetLine {
                line: 1,
                // The hash the model was shown: over "hello", not "\u{feff}hello".
                hash: line_hash("hello"),
                new_text: "HELLO".to_string(),
            }],
        ),
        "",
    );
    assert!(text.contains('✓'), "expected success: {text}");
    assert!(matches!(effect, CacheEffect::Invalidate));
    assert_eq!(
        std::fs::read_to_string(f.path()).unwrap(),
        "\u{feff}HELLO\nworld\n",
        "BOM must survive the edit"
    );
}

#[test]
fn stale_anchor_rejects_without_writing() {
    let original = "alpha\nbeta\ngamma\n";
    let f = make_temp(original);
    let (text, effect) = run_io(
        &params(
            f.path(),
            vec![AnchorOp::SetLine {
                line: 2,
                hash: "dead".to_string(), // wrong
                new_text: "BETA".to_string(),
            }],
        ),
        "",
    );
    assert!(text.starts_with("CONFLICT:"), "expected conflict: {text}");
    assert!(text.contains("fresh anchors:"));
    assert!(
        text.contains(&format!("2:{}|beta", line_hash("beta"))),
        "fresh anchor for the drifted line must be returned: {text}"
    );
    assert!(matches!(effect, CacheEffect::None));
    assert_eq!(
        std::fs::read_to_string(f.path()).unwrap(),
        original,
        "a stale anchor must never write"
    );
}

#[test]
fn delete_line_via_empty_new_text() {
    let f = make_temp("keep1\ndrop\nkeep2\n");
    let (text, _) = run_io(
        &params(
            f.path(),
            vec![AnchorOp::SetLine {
                line: 2,
                hash: line_hash("drop"),
                new_text: String::new(),
            }],
        ),
        "",
    );
    assert!(text.contains('✓'), "{text}");
    assert_eq!(std::fs::read_to_string(f.path()).unwrap(), "keep1\nkeep2\n");
}

#[test]
fn insert_after_top_prepends() {
    let f = make_temp("first\nsecond\n");
    let (text, _) = run_io(
        &params(
            f.path(),
            vec![AnchorOp::InsertAfter {
                line: 0,
                hash: None,
                new_text: "// header".to_string(),
            }],
        ),
        "",
    );
    assert!(text.contains('✓'), "{text}");
    assert_eq!(
        std::fs::read_to_string(f.path()).unwrap(),
        "// header\nfirst\nsecond\n"
    );
}

#[test]
fn replace_lines_range() {
    let f = make_temp("a\nb\nc\nd\n");
    let (text, _) = run_io(
        &params(
            f.path(),
            vec![AnchorOp::ReplaceLines {
                start_line: 2,
                start_hash: line_hash("b"),
                end_line: 3,
                end_hash: line_hash("c"),
                new_text: "X\nY".to_string(),
            }],
        ),
        "",
    );
    assert!(text.contains('✓'), "{text}");
    assert_eq!(std::fs::read_to_string(f.path()).unwrap(), "a\nX\nY\nd\n");
}

#[test]
fn batch_atomic_all_or_nothing_on_one_stale_anchor() {
    // Two edits: the first is valid, the second is stale. The whole batch must
    // abort with NO partial write (premium correctness over single-op tools).
    let original = "one\ntwo\nthree\n";
    let f = make_temp(original);
    let (text, effect) = run_io(
        &params(
            f.path(),
            vec![
                AnchorOp::SetLine {
                    line: 1,
                    hash: line_hash("one"),
                    new_text: "ONE".to_string(),
                },
                AnchorOp::SetLine {
                    line: 3,
                    hash: "badd".to_string(), // stale
                    new_text: "THREE".to_string(),
                },
            ],
        ),
        "",
    );
    assert!(text.starts_with("CONFLICT:"), "{text}");
    assert!(matches!(effect, CacheEffect::None));
    assert_eq!(
        std::fs::read_to_string(f.path()).unwrap(),
        original,
        "a single stale anchor must abort the entire batch"
    );
}

#[test]
fn batch_atomic_applies_all_valid_edits_bottom_up() {
    let f = make_temp("1\n2\n3\n4\n5\n");
    let (text, _) = run_io(
        &params(
            f.path(),
            vec![
                AnchorOp::ReplaceLines {
                    start_line: 1,
                    start_hash: line_hash("1"),
                    end_line: 2,
                    end_hash: line_hash("2"),
                    new_text: "A".to_string(),
                },
                AnchorOp::SetLine {
                    line: 5,
                    hash: line_hash("5"),
                    new_text: "E".to_string(),
                },
            ],
        ),
        "",
    );
    assert!(text.contains('✓'), "{text}");
    assert!(text.contains("2 anchored edits"), "{text}");
    assert_eq!(std::fs::read_to_string(f.path()).unwrap(), "A\n3\n4\nE\n");
}

#[test]
fn batch_application_is_byte_deterministic() {
    // Determinism (#498): the same batch on byte-identical inputs must produce a
    // byte-identical result on disk — the core guarantee that makes anchored
    // edits independently verifiable and replayable.
    let original = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n";
    let make_ops = || {
        vec![
            AnchorOp::SetLine {
                line: 1,
                hash: line_hash("fn a() {}"),
                new_text: "fn a() { /* x */ }".to_string(),
            },
            AnchorOp::ReplaceLines {
                start_line: 3,
                start_hash: line_hash("fn c() {}"),
                end_line: 4,
                end_hash: line_hash("fn d() {}"),
                new_text: "fn cd() {}".to_string(),
            },
        ]
    };

    let f1 = make_temp(original);
    let f2 = make_temp(original);
    let (t1, _) = run_io(&params(f1.path(), make_ops()), "");
    let (t2, _) = run_io(&params(f2.path(), make_ops()), "");
    assert!(t1.contains('✓') && t2.contains('✓'));
    let out1 = std::fs::read(f1.path()).unwrap();
    let out2 = std::fs::read(f2.path()).unwrap();
    assert_eq!(out1, out2, "identical batch must yield identical bytes");
    assert_eq!(
        String::from_utf8(out1).unwrap(),
        "fn a() { /* x */ }\nfn b() {}\nfn cd() {}\n"
    );
}

#[test]
fn overlapping_batch_is_rejected() {
    let f = make_temp("a\nb\nc\n");
    let (text, effect) = run_io(
        &params(
            f.path(),
            vec![
                AnchorOp::SetLine {
                    line: 2,
                    hash: line_hash("b"),
                    new_text: "B".to_string(),
                },
                AnchorOp::ReplaceLines {
                    start_line: 1,
                    start_hash: line_hash("a"),
                    end_line: 2,
                    end_hash: line_hash("b"),
                    new_text: "X".to_string(),
                },
            ],
        ),
        "",
    );
    assert!(text.contains("overlapping"), "{text}");
    assert!(matches!(effect, CacheEffect::None));
    assert_eq!(std::fs::read_to_string(f.path()).unwrap(), "a\nb\nc\n");
}

#[test]
fn expected_blake3_guard_blocks_mismatch() {
    let f = make_temp("aaa\n");
    let mut p = params(
        f.path(),
        vec![AnchorOp::SetLine {
            line: 1,
            hash: line_hash("aaa"),
            new_text: "bbb".to_string(),
        }],
    );
    p.expected_blake3 = Some("deadbeef".to_string());
    let (text, effect) = run_io(&p, "");
    assert!(text.contains("preimage mismatch"), "{text}");
    assert!(matches!(effect, CacheEffect::None));
    assert_eq!(std::fs::read_to_string(f.path()).unwrap(), "aaa\n");
}

#[test]
fn crlf_file_keeps_crlf() {
    let f = make_temp("a\r\nb\r\nc\r\n");
    let (text, _) = run_io(
        &params(
            f.path(),
            vec![AnchorOp::SetLine {
                line: 2,
                hash: line_hash("b"),
                new_text: "B".to_string(),
            }],
        ),
        "",
    );
    assert!(text.contains('✓'), "{text}");
    assert_eq!(
        std::fs::read_to_string(f.path()).unwrap(),
        "a\r\nB\r\nc\r\n",
        "CRLF endings must be preserved"
    );
}

#[test]
fn no_change_edit_is_rejected() {
    let f = make_temp("same\n");
    let (text, effect) = run_io(
        &params(
            f.path(),
            vec![AnchorOp::SetLine {
                line: 1,
                hash: line_hash("same"),
                new_text: "same".to_string(),
            }],
        ),
        "",
    );
    assert!(text.contains("no change"), "{text}");
    assert!(matches!(effect, CacheEffect::None));
}

#[test]
fn backup_is_written_when_enabled() {
    let f = make_temp("orig\n");
    let mut p = params(
        f.path(),
        vec![AnchorOp::SetLine {
            line: 1,
            hash: line_hash("orig"),
            new_text: "new".to_string(),
        }],
    );
    p.backup = true;
    let (text, _) = run_io(&p, "");
    let bp = text
        .lines()
        .find_map(|l| l.strip_prefix("backup: "))
        .expect("backup line");
    assert_eq!(std::fs::read_to_string(bp).unwrap(), "orig\n");
    assert_eq!(std::fs::read_to_string(f.path()).unwrap(), "new\n");
}

#[test]
fn evidence_diff_emitted_when_enabled() {
    let f = make_temp("line1\nline2\n");
    let mut p = params(
        f.path(),
        vec![AnchorOp::SetLine {
            line: 2,
            hash: line_hash("line2"),
            new_text: "changed2".to_string(),
        }],
    );
    p.evidence = true;
    let (text, _) = run_io(&p, "");
    assert!(text.contains("```diff"), "{text}");
    assert!(text.contains("postimage:"), "{text}");
}

#[test]
fn handle_applies_cache_effect() {
    let f = make_temp("v1\n");
    let mut cache = SessionCache::new();
    cache.store(&f.path().to_string_lossy(), "v1\n");
    let out = handle(
        &mut cache,
        &params(
            f.path(),
            vec![AnchorOp::SetLine {
                line: 1,
                hash: line_hash("v1"),
                new_text: "v2".to_string(),
            }],
        ),
    );
    assert!(out.contains('✓'), "{out}");
    assert!(
        cache.get(&f.path().to_string_lossy()).is_none(),
        "a successful patch must invalidate the cache entry"
    );
}

#[test]
fn missing_file_reports_error() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.rs");
    let (text, effect) = run_io(
        &params(
            &missing,
            vec![AnchorOp::SetLine {
                line: 1,
                hash: "aa".to_string(),
                new_text: "x".to_string(),
            }],
        ),
        "",
    );
    assert!(text.contains("ERROR"), "{text}");
    assert!(matches!(effect, CacheEffect::None));
}

#[cfg(feature = "tree-sitter")]
#[test]
fn syntax_gate_rejects_edit_that_breaks_a_clean_file() {
    // Deleting the closing brace turns valid Rust into a parse error; the gate
    // must reject and leave the file untouched.
    let original = "fn main() {\n    let x = 1;\n}\n";
    let f = make_temp_rs(original);
    let (text, effect) = run_io(
        &params(
            f.path(),
            vec![AnchorOp::SetLine {
                line: 3,
                hash: line_hash("}"),
                new_text: String::new(),
            }],
        ),
        "",
    );
    assert!(text.contains("syntax error"), "{text}");
    assert!(matches!(effect, CacheEffect::None));
    assert_eq!(
        std::fs::read_to_string(f.path()).unwrap(),
        original,
        "a syntax-breaking edit must not write"
    );
}

#[cfg(feature = "tree-sitter")]
#[test]
fn validate_syntax_false_overrides_the_gate() {
    let original = "fn main() {\n    let x = 1;\n}\n";
    let f = make_temp_rs(original);
    let mut p = params(
        f.path(),
        vec![AnchorOp::SetLine {
            line: 3,
            hash: line_hash("}"),
            new_text: String::new(),
        }],
    );
    p.validate_syntax = false;
    let (text, effect) = run_io(&p, "");
    assert!(text.contains('✓'), "override must allow the write: {text}");
    assert!(matches!(effect, CacheEffect::Invalidate));
    assert_eq!(
        std::fs::read_to_string(f.path()).unwrap(),
        "fn main() {\n    let x = 1;\n"
    );
}

#[cfg(feature = "tree-sitter")]
#[test]
fn syntax_gate_allows_valid_edit() {
    let f = make_temp_rs("fn main() {\n    let x = 1;\n}\n");
    let (text, effect) = run_io(
        &params(
            f.path(),
            vec![AnchorOp::SetLine {
                line: 2,
                hash: line_hash("    let x = 1;"),
                new_text: "    let x = 2;".to_string(),
            }],
        ),
        "",
    );
    assert!(text.contains('✓'), "{text}");
    assert!(matches!(effect, CacheEffect::Invalidate));
}

#[test]
fn create_writes_new_file_and_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("nested/sub/new_file.rs");
    let (text, effect) = run_io(
        &params(
            &target,
            vec![AnchorOp::Create {
                new_text: "fn hello() {}\n".to_string(),
            }],
        ),
        "",
    );
    assert!(text.contains("✓ created"), "{text}");
    assert!(text.contains("postimage:"), "{text}");
    assert!(matches!(effect, CacheEffect::Invalidate));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn hello() {}\n");
}

#[test]
fn create_rejects_existing_file() {
    // Strict by design: unlike `ctx_edit create=true` (which overwrites), a
    // ctx_patch create on an existing file is an error — modification must go
    // through anchors so the preimage is always verified.
    let f = make_temp("already here\n");
    let (text, effect) = run_io(
        &params(
            f.path(),
            vec![AnchorOp::Create {
                new_text: "overwrite attempt".to_string(),
            }],
        ),
        "",
    );
    assert!(text.contains("already exists"), "{text}");
    assert!(matches!(effect, CacheEffect::None));
    assert_eq!(
        std::fs::read_to_string(f.path()).unwrap(),
        "already here\n",
        "create must never overwrite"
    );
}

#[test]
fn create_cannot_be_batched_with_anchored_ops() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("mixed.rs");
    let (text, effect) = run_io(
        &params(
            &target,
            vec![
                AnchorOp::Create {
                    new_text: "content".to_string(),
                },
                AnchorOp::SetLine {
                    line: 1,
                    hash: "aaaa".to_string(),
                    new_text: "x".to_string(),
                },
            ],
        ),
        "",
    );
    assert!(text.contains("cannot be batched"), "{text}");
    assert!(matches!(effect, CacheEffect::None));
    assert!(!target.exists(), "no partial write on a rejected batch");
}

#[test]
fn create_empty_content_makes_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("empty.txt");
    let (text, effect) = run_io(
        &params(
            &target,
            vec![AnchorOp::Create {
                new_text: String::new(),
            }],
        ),
        "",
    );
    assert!(text.contains("✓ created"), "{text}");
    assert!(matches!(effect, CacheEffect::Invalidate));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "");
}

// Symlink rejection is inherited from the shared edit_io boundary.
#[cfg(unix)]
#[test]
fn editing_through_symlink_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.rs");
    std::fs::write(&real, "fn old() {}\n").unwrap();
    let link = dir.path().join("link.rs");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let (text, effect) = run_io(
        &params(
            &link,
            vec![AnchorOp::SetLine {
                line: 1,
                hash: line_hash("fn old() {}"),
                new_text: "fn new() {}".to_string(),
            }],
        ),
        "",
    );
    assert!(text.contains("symlink"), "{text}");
    assert!(matches!(effect, CacheEffect::None));
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "fn old() {}\n");
}
