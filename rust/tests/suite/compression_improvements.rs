use lean_ctx::core::compressor::aggressive_compress;
use lean_ctx::core::tokens::count_tokens;
use lean_ctx::shell::compress::compress_if_beneficial_pub;

#[test]
fn markdown_aggressive_keeps_headings_and_saves_tokens() {
    let mut doc =
        String::from("# Context Engine\n\nIntro text with stable overview.\n\n## Setup\n\n");
    for i in 0..60 {
        doc.push_str(&format!(
            "Repeated explanatory prose line {i} about background concepts and examples.\n"
        ));
    }
    doc.push_str("- `ctx_read` keeps exact source references for agent tasks.\n");
    doc.push_str("## Safety\n\nMUST preserve diagnostics, commands, and recovery notes.\n");
    for i in 0..60 {
        doc.push_str(&format!(
            "Additional explanatory prose line {i} about operational background.\n"
        ));
    }

    let compressed = aggressive_compress(&doc, Some("md"));
    assert!(count_tokens(&compressed) < count_tokens(&doc));
    assert!(compressed.contains("# Context Engine"));
    assert!(compressed.contains("## Setup"));
    assert!(compressed.contains("## Safety"));
    assert!(compressed.contains("ctx_read"));
    assert!(compressed.contains("MUST preserve"));
}

#[test]
fn cargo_warning_output_folds_progress_and_preserves_diagnostics() {
    let mut output = String::new();
    for i in 0..80 {
        output.push_str(&format!(
            "   Compiling crate_{i:03} v0.1.0 (/tmp/ws/crate_{i:03})\n"
        ));
    }
    output.push_str("warning: unused variable: `tmp`\n");
    output.push_str("  --> crates/demo/src/lib.rs:42:9\n");
    output.push_str("   |\n42 |     let tmp = 1;\n   |         ^^^\n");
    output.push_str("warning: `demo` generated 1 warning\n");
    output.push_str("    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.23s\n");

    let compressed = compress_if_beneficial_pub("cargo build", &output);
    assert!(count_tokens(&compressed) < count_tokens(&output));
    assert!(compressed.contains("cargo compile/check lines folded"));
    assert!(compressed.contains("warning: unused variable"));
    assert!(compressed.contains("crates/demo/src/lib.rs:42:9"));
    assert!(compressed.contains("generated 1 warning"));
}

#[test]
fn pytest_success_output_folds_passed_lines_and_preserves_summary() {
    let mut output = String::from(
        "============================= test session starts =============================\n",
    );
    for i in 0..80 {
        output.push_str(&format!(
            "tests/test_mod_{:02}.py::test_case_{i:03} PASSED [ {:02}%]\n",
            i / 10,
            i % 100
        ));
    }
    output.push_str(
        "======================= 80 passed, 0 warnings in 2.34s =======================\n",
    );

    let compressed = compress_if_beneficial_pub("pytest", &output);
    assert!(count_tokens(&compressed) < count_tokens(&output));
    assert!(compressed.contains("pytest PASSED lines folded"));
    assert!(compressed.contains("80 passed"));
}

#[test]
fn markdown_compaction_is_deterministic_across_calls() {
    // #498: aggressive compaction must be a pure function of the input bytes.
    // Mixed token frequencies engineer near-tied line scores on purpose — with
    // unordered per-line token sets the f64 summation order could flip the
    // selected lines between calls.
    let mut doc = String::from("# Stability\n\nIntro line for the document.\n");
    for section in 0..4 {
        doc.push_str(&format!("\n## Section {section}\n\n"));
        for i in 0..30 {
            doc.push_str(&format!(
                "candidate_{i} shared_token_{} another_token_{} overlapping detail item.\n",
                i % 3,
                i % 7,
            ));
        }
    }

    let first = aggressive_compress(&doc, Some("md"));
    for _ in 0..16 {
        assert_eq!(
            first,
            aggressive_compress(&doc, Some("md")),
            "markdown compaction must be byte-stable across calls"
        );
    }
}

#[test]
fn verbatim_token_cap_survives_progress_folding() {
    // Folding progress noise must compose with — not replace — the verbatim
    // token cap: a build log whose *diagnostics alone* exceed the budget is
    // still head/tail-truncated with safety-needle preservation.
    let mut output = String::new();
    for i in 0..200 {
        output.push_str(&format!(
            "   Compiling crate_{i:03} v0.1.0 (/tmp/ws/crate_{i:03})\n"
        ));
    }
    output.push_str("error[E0308]: mismatched types\n");
    output.push_str("  --> crates/demo/src/lib.rs:7:5\n");
    // A diagnostic body far above MAX_VERBATIM_TOKENS (8000) that is neither
    // foldable progress nor low-signal, so only the cap can bound it.
    for i in 0..4000 {
        output.push_str(&format!(
            "note: required because of the expansion trace frame {i} in `deeply::nested::module_{i}`\n"
        ));
    }
    output.push_str("error: aborting due to 1 previous error\n");

    let compressed = compress_if_beneficial_pub("cargo build", &output);
    assert!(
        compressed.contains("cargo compile/check lines folded"),
        "progress noise must still fold"
    );
    assert!(
        compressed.contains("lines omitted"),
        "oversized diagnostics must still hit the verbatim cap"
    );
    assert!(
        compressed.contains("error[E0308]: mismatched types"),
        "the head diagnostic must survive"
    );
    assert!(
        compressed.contains("aborting due to 1 previous error"),
        "the tail diagnostic must survive"
    );
    assert!(
        count_tokens(&compressed) < count_tokens(&output) / 4,
        "capped output must be a fraction of the raw log"
    );
}

#[test]
fn plain_txt_without_headings_is_not_structurally_compacted() {
    // A `.txt` with hyphen lists but no ATX heading is ordinary prose — the
    // lossy structural compactor must not touch it (no omitted-lines markers).
    let mut doc = String::from("Shopping notes\n\n");
    for i in 0..40 {
        doc.push_str(&format!("- item number {i} with a longer description\n"));
    }

    let compressed = aggressive_compress(&doc, Some("txt"));
    assert!(
        !compressed.contains("[lean-ctx: omitted"),
        "txt without headings must not be structurally compacted: {compressed}"
    );

    // The same content as `.md` (heading added) opts in.
    let md = format!("# Notes\n\n{doc}");
    let compacted = aggressive_compress(&md, Some("md"));
    assert!(compacted.contains("[lean-ctx: omitted"));
}

#[test]
fn markdown_compaction_never_splits_code_fences() {
    let mut doc = String::from("# Guide\n\nIntro line.\n\n## Usage\n\n");
    for i in 0..40 {
        doc.push_str(&format!("Filler prose sentence number {i} for volume.\n"));
    }
    doc.push_str("```bash\nlean-ctx read src/lib.rs\n\nlean-ctx search \"ctx_read\" src/\n```\n");
    for i in 0..40 {
        doc.push_str(&format!("More filler prose sentence number {i}.\n"));
    }

    let compressed = aggressive_compress(&doc, Some("md"));
    assert_eq!(
        compressed.matches("```").count() % 2,
        0,
        "code fences must stay balanced: {compressed}"
    );
    let mut in_fence = false;
    for line in compressed.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        } else if in_fence {
            assert!(
                !line.starts_with("... [lean-ctx:"),
                "omission marker inside a fenced block: {compressed}"
            );
        }
    }
}
