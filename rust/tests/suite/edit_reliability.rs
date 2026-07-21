//! Edit-reliability benchmark (#1008 / GL#1015) — a deterministic regression
//! guard for anchored editing's core promise.
//!
//! ## What this proves
//!
//! `ctx_edit` is a `str_replace` tool: to change a line the agent must hand it an
//! `old_string` that (a) matches the bytes on disk and (b) is **unique**. The
//! epic's thesis is that requirement (b) — positional ambiguity — is a
//! model-independent failure: the line the agent wants to fix is frequently not
//! unique (`acc += 1;`, `return a - b`, `}` …), so a minimal, natural edit
//! attempt is rejected and the agent must recall extra surrounding context.
//! `ctx_patch` removes that tax: it targets a line by `(number, content-hash)`,
//! so a duplicated line is no obstacle.
//!
//! ## Why it is not a live multi-model run
//!
//! Live success-rate measurement across models needs API keys and a non-hermetic
//! harness; it lives offline. The *mechanism* under test is model-independent
//! (every model pays the same `str_replace` ambiguity tax and the same zero tax
//! to anchors), so this hermetic benchmark measures the **tool**, with real files
//! and real tool calls — no mocks, no perturb-to-fail rigging.
//!
//! ## Honesty controls
//!
//! `ctx_edit` already tolerates trailing-whitespace/CRLF drift, so this does NOT
//! claim a win there. Three measurements per language keep the comparison fair:
//!   1. **control** (unique line, exact recall): both tools succeed.
//!   2. **ambiguity / minimal recall** (duplicated line, bare `old_string`):
//!      `ctx_edit` is rejected as non-unique; `ctx_patch` succeeds positionally.
//!   3. **ambiguity / full recall** (duplicated line, `old_string` widened with
//!      the recalled neighbouring line): `ctx_edit` now succeeds — proving the
//!      gap in (2) is the recall tax, not a broken tool.

use std::fs;
use std::path::Path;

use lean_ctx::core::anchor::{annotate, line_hash};
use lean_ctx::core::tokens::count_tokens;
use lean_ctx::tools::ctx_edit::{self, CacheEffect, EditParams};
use lean_ctx::tools::ctx_patch::{self, AnchorOp, PatchParams};

/// One language's two source variants plus the lines that carry the mechanical
/// bug. Sources are complete, syntactically valid units so the `ctx_patch`
/// tree-sitter gate sees a clean→clean transition (the bug is *semantic*).
struct Lang {
    name: &'static str,
    ext: &'static str,

    /// Unique-line variant (control).
    ctrl_src: &'static str,
    ctrl_line: usize,
    ctrl_buggy: &'static str,
    ctrl_fixed: &'static str,

    /// Duplicated-line variant (ambiguity). `amb_line` is fixed; `amb_dup_line`
    /// holds the identical sibling that must stay untouched.
    amb_src: &'static str,
    amb_line: usize,
    amb_dup_line: usize,
    amb_buggy: &'static str,
    amb_fixed: &'static str,
}

fn langs() -> Vec<Lang> {
    vec![
        Lang {
            name: "rust",
            ext: "rs",
            ctrl_src: "fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n",
            ctrl_line: 2,
            ctrl_buggy: "    a - b",
            ctrl_fixed: "    a + b",
            amb_src: "fn main() {\n    let mut acc = 0;\n    acc += 1;\n    acc += 1;\n    println!(\"{acc}\");\n}\n",
            amb_line: 3,
            amb_dup_line: 4,
            amb_buggy: "    acc += 1;",
            amb_fixed: "    acc += 2;",
        },
        Lang {
            name: "python",
            ext: "py",
            ctrl_src: "def add(a, b):\n    return a - b\n",
            ctrl_line: 2,
            ctrl_buggy: "    return a - b",
            ctrl_fixed: "    return a + b",
            amb_src: "def main():\n    acc = 0\n    acc += 1\n    acc += 1\n    print(acc)\n",
            amb_line: 3,
            amb_dup_line: 4,
            amb_buggy: "    acc += 1",
            amb_fixed: "    acc += 2",
        },
        Lang {
            name: "javascript",
            ext: "js",
            ctrl_src: "function add(a, b) {\n  return a - b;\n}\n",
            ctrl_line: 2,
            ctrl_buggy: "  return a - b;",
            ctrl_fixed: "  return a + b;",
            amb_src: "function main() {\n  let acc = 0;\n  acc += 1;\n  acc += 1;\n  console.log(acc);\n}\n",
            amb_line: 3,
            amb_dup_line: 4,
            amb_buggy: "  acc += 1;",
            amb_fixed: "  acc += 2;",
        },
        Lang {
            name: "typescript",
            ext: "ts",
            ctrl_src: "function add(a: number, b: number): number {\n  return a - b;\n}\n",
            ctrl_line: 2,
            ctrl_buggy: "  return a - b;",
            ctrl_fixed: "  return a + b;",
            amb_src: "function main(): void {\n  let acc: number = 0;\n  acc += 1;\n  acc += 1;\n  console.log(acc);\n}\n",
            amb_line: 3,
            amb_dup_line: 4,
            amb_buggy: "  acc += 1;",
            amb_fixed: "  acc += 2;",
        },
        Lang {
            name: "go",
            ext: "go",
            ctrl_src: "package main\n\nfunc add(a int, b int) int {\n\treturn a - b\n}\n",
            ctrl_line: 4,
            ctrl_buggy: "\treturn a - b",
            ctrl_fixed: "\treturn a + b",
            amb_src: "package main\n\nfunc main() {\n\tacc := 0\n\tacc += 1\n\tacc += 1\n\t_ = acc\n}\n",
            amb_line: 5,
            amb_dup_line: 6,
            amb_buggy: "\tacc += 1",
            amb_fixed: "\tacc += 2",
        },
    ]
}

fn edit_params(path: &Path, old: &str, new: &str) -> EditParams {
    EditParams {
        path: path.to_string_lossy().into_owned(),
        old_string: old.to_string(),
        new_string: new.to_string(),
        replace_all: false,
        create: false,
        expected_md5: None,
        expected_size: None,
        expected_mtime_ms: None,
        backup: false,
        backup_path: None,
        evidence: false,
        diff_max_lines: 0,
        allow_lossy_utf8: false,
    }
}

fn patch_params(path: &Path, ops: Vec<AnchorOp>) -> PatchParams {
    PatchParams {
        path: path.to_string_lossy().into_owned(),
        ops,
        expected_md5: None,
        backup: false,
        backup_path: None,
        evidence: false,
        diff_max_lines: 0,
        allow_lossy_utf8: false,
        validate_syntax: true,
    }
}

fn succeeded(effect: &CacheEffect) -> bool {
    matches!(effect, CacheEffect::Invalidate)
}

fn nth_line(content: &str, line_1based: usize) -> &str {
    content.lines().nth(line_1based - 1).unwrap_or("")
}

/// Fix one line by anchor on a fresh copy; returns (success, resulting bytes).
/// Also asserts the read→edit roundtrip: the hash fed to `ctx_patch` is exactly
/// what `ctx_read(mode="anchored")` (`annotate`) would have shown for that line.
fn fix_anchored(
    dir: &Path,
    tag: &str,
    ext: &str,
    src: &str,
    line: usize,
    buggy: &str,
    fixed: &str,
) -> (bool, String) {
    let path = dir.join(format!("{tag}.{ext}"));
    fs::write(&path, src).unwrap();

    let hash = line_hash(buggy);
    let annotated = annotate(src, 1);
    let shown = nth_line(&annotated, line);
    assert!(
        shown.starts_with(&format!("{line}:{hash}|")),
        "anchor roundtrip mismatch for {tag}: ctx_read would show {shown:?}"
    );

    let ops = vec![AnchorOp::SetLine {
        line,
        hash,
        new_text: fixed.to_string(),
    }];
    let (_text, effect) = ctx_patch::run_io(&patch_params(&path, ops), "");
    (succeeded(&effect), fs::read_to_string(&path).unwrap())
}

/// Fix one line by string-replace on a fresh copy; returns (success, bytes).
fn fix_str_replace(
    dir: &Path,
    tag: &str,
    ext: &str,
    src: &str,
    old: &str,
    new: &str,
) -> (bool, String) {
    let path = dir.join(format!("{tag}.{ext}"));
    fs::write(&path, src).unwrap();
    let (_text, effect) = ctx_edit::run_io(&edit_params(&path, old, new), "");
    (succeeded(&effect), fs::read_to_string(&path).unwrap())
}

#[test]
fn anchored_editing_beats_str_replace_on_ambiguity_across_languages() {
    let dir = tempfile::tempdir().unwrap();
    let langs = langs();

    // Denominators: each language contributes a control and an ambiguity case.
    let attempts = langs.len() * 2;
    let mut anchored_ok = 0usize;
    let mut str_replace_minimal_ok = 0usize;
    let mut str_replace_full_recall_ok = 0usize; // fairness: control + widened ambiguity

    for lang in &langs {
        // 1. Control — unique line, exact recall. Both tools must succeed.
        let (a_ctrl, a_ctrl_out) = fix_anchored(
            dir.path(),
            &format!("{}_ctrl_anchored", lang.name),
            lang.ext,
            lang.ctrl_src,
            lang.ctrl_line,
            lang.ctrl_buggy,
            lang.ctrl_fixed,
        );
        let (e_ctrl, e_ctrl_out) = fix_str_replace(
            dir.path(),
            &format!("{}_ctrl_edit", lang.name),
            lang.ext,
            lang.ctrl_src,
            lang.ctrl_buggy,
            lang.ctrl_fixed,
        );
        assert!(a_ctrl, "[{}] ctx_patch must fix a unique line", lang.name);
        assert!(
            e_ctrl,
            "[{}] ctx_edit must fix a unique line (exact recall)",
            lang.name
        );
        assert_eq!(nth_line(&a_ctrl_out, lang.ctrl_line), lang.ctrl_fixed);
        assert_eq!(nth_line(&e_ctrl_out, lang.ctrl_line), lang.ctrl_fixed);
        anchored_ok += usize::from(a_ctrl);
        str_replace_minimal_ok += usize::from(e_ctrl);
        str_replace_full_recall_ok += usize::from(e_ctrl);

        // 2. Ambiguity, minimal recall — duplicated line, bare old_string.
        let (a_amb, a_amb_out) = fix_anchored(
            dir.path(),
            &format!("{}_amb_anchored", lang.name),
            lang.ext,
            lang.amb_src,
            lang.amb_line,
            lang.amb_buggy,
            lang.amb_fixed,
        );
        let (e_amb, _e_amb_out) = fix_str_replace(
            dir.path(),
            &format!("{}_amb_edit", lang.name),
            lang.ext,
            lang.amb_src,
            lang.amb_buggy,
            lang.amb_fixed,
        );
        assert!(
            a_amb,
            "[{}] ctx_patch must fix one of two identical lines positionally",
            lang.name
        );
        // Anchored edit touched ONLY the targeted line.
        assert_eq!(
            nth_line(&a_amb_out, lang.amb_line),
            lang.amb_fixed,
            "[{}] anchored target line",
            lang.name
        );
        assert_eq!(
            nth_line(&a_amb_out, lang.amb_dup_line),
            lang.amb_buggy,
            "[{}] anchored must NOT touch the duplicate sibling",
            lang.name
        );
        assert!(
            !e_amb,
            "[{}] ctx_edit must be rejected on a non-unique bare old_string (the recall tax)",
            lang.name
        );
        anchored_ok += usize::from(a_amb);
        str_replace_minimal_ok += usize::from(e_amb);

        // 3. Ambiguity, full recall — fairness: widen old_string with the
        //    recalled neighbouring (duplicate) line so it is unique again.
        let widened_old = format!("{}\n{}", lang.amb_buggy, lang.amb_buggy);
        let widened_new = format!("{}\n{}", lang.amb_fixed, lang.amb_buggy);
        let (e_amb_full, e_amb_full_out) = fix_str_replace(
            dir.path(),
            &format!("{}_amb_edit_full", lang.name),
            lang.ext,
            lang.amb_src,
            &widened_old,
            &widened_new,
        );
        assert!(
            e_amb_full,
            "[{}] ctx_edit should succeed once the agent recalls extra context",
            lang.name
        );
        assert_eq!(nth_line(&e_amb_full_out, lang.amb_line), lang.amb_fixed);
        assert_eq!(nth_line(&e_amb_full_out, lang.amb_dup_line), lang.amb_buggy);
        str_replace_full_recall_ok += usize::from(e_amb_full);
    }

    let pct = |n: usize| (n as f64 / attempts as f64) * 100.0;
    println!(
        "\nEdit-reliability benchmark ({} languages, {attempts} fixes):",
        langs.len()
    );
    println!(
        "  ctx_patch (anchored)              : {anchored_ok}/{attempts}  ({:.0}%)",
        pct(anchored_ok)
    );
    println!(
        "  ctx_edit  (str_replace, minimal)  : {str_replace_minimal_ok}/{attempts}  ({:.0}%)",
        pct(str_replace_minimal_ok)
    );
    println!(
        "  ctx_edit  (str_replace, +recall)  : {str_replace_full_recall_ok}/{attempts}  ({:.0}%)",
        pct(str_replace_full_recall_ok)
    );

    // Anchored editing fixes every mechanical bug regardless of uniqueness.
    assert_eq!(
        anchored_ok, attempts,
        "ctx_patch must achieve 100% — anchors are immune to the ambiguity tax"
    );
    // The minimal (natural) str_replace attempt is strictly worse: it loses
    // every ambiguity case. This is the regression guard for the epic's claim.
    assert!(
        str_replace_minimal_ok < anchored_ok,
        "ctx_edit minimal recall ({str_replace_minimal_ok}) must trail ctx_patch ({anchored_ok})"
    );
    assert_eq!(
        str_replace_minimal_ok,
        langs.len(),
        "ctx_edit minimal recall should pass exactly the control cases"
    );
    // Fairness: the gap is the recall tax, not a broken tool — with the extra
    // recalled context str_replace also reaches 100%.
    assert_eq!(
        str_replace_full_recall_ok, attempts,
        "ctx_edit recovers to 100% only by paying the extra-context recall tax"
    );
}

/// A/B output-token cost on the exact same successful edits (#1008 metering).
///
/// Reliability (above) is one axis; the other is what each success *costs* in
/// output tokens — the argument bytes the model must emit per tool call:
///
/// * `ctx_patch`: `line`, 4-hex `hash`, `new_text` — the old span is referenced,
///   never reproduced.
/// * `ctx_edit`:  `old_string` + `new_string` — the old span is paid verbatim,
///   and on the ambiguity cases the *widened* variant (the one that actually
///   succeeds, see test above) pays the recalled neighbour line twice: once in
///   `old_string` and once again echoed in `new_string`.
///
/// Both sides are counted with the same tokenizer over the payload fields only
/// (path/booleans are identical overhead on both tools and cancel out). This is
/// the same preimage math the runtime meters per applied op via
/// `edit_metering` (`anchored_avoided_output_tokens`), and the benchmark keeps
/// the claim exactly as honest as the meter does: on a *tiny* span the 4-hex
/// anchor can cost a token or two more than quoting the line (the meter floors
/// that op at 0 rather than booking negative savings) — but every ambiguity
/// case and the batch total must be strictly cheaper anchored.
#[test]
fn anchored_args_cost_fewer_output_tokens_on_identical_fixes() {
    let langs = langs();
    let mut anchored_total = 0usize;
    let mut str_replace_total = 0usize;
    let mut cases = 0usize;
    let mut tiny_span_regressions = 0usize;

    for lang in &langs {
        // Control fix — both tools succeed with their minimal natural args.
        // No per-case assertion here: one-liner spans are where the anchor
        // overhead can outweigh the quote (the floor-at-0 case in metering).
        let anchored_ctrl = count_tokens(&format!(
            "{}:{} {}",
            lang.ctrl_line,
            line_hash(lang.ctrl_buggy),
            lang.ctrl_fixed
        ));
        let sr_ctrl = count_tokens(lang.ctrl_buggy) + count_tokens(lang.ctrl_fixed);
        tiny_span_regressions += usize::from(anchored_ctrl > sr_ctrl);

        // Ambiguity fix — compare what each tool needs to SUCCEED: anchored
        // stays minimal; str_replace must widen old_string with the duplicate
        // sibling and echo it back in new_string (cf. the reliability test).
        let anchored_amb = count_tokens(&format!(
            "{}:{} {}",
            lang.amb_line,
            line_hash(lang.amb_buggy),
            lang.amb_fixed
        ));
        let widened_old = format!("{}\n{}", lang.amb_buggy, lang.amb_buggy);
        let widened_new = format!("{}\n{}", lang.amb_fixed, lang.amb_buggy);
        let sr_amb = count_tokens(&widened_old) + count_tokens(&widened_new);
        assert!(
            anchored_amb < sr_amb,
            "[{}] ambiguity: anchored args ({anchored_amb} tok) must beat the \
             widened str_replace args ({sr_amb} tok)",
            lang.name
        );

        anchored_total += anchored_ctrl + anchored_amb;
        str_replace_total += sr_ctrl + sr_amb;
        cases += 2;
    }

    let saved = str_replace_total.saturating_sub(anchored_total);
    println!(
        "\nA/B output-token cost ({cases} successful fixes across {} languages):",
        langs.len()
    );
    println!("  ctx_patch (anchored) args : {anchored_total} tok");
    println!("  ctx_edit  (str_replace)   : {str_replace_total} tok");
    println!(
        "  avoided                   : {saved} tok ({:.0}%)  [{tiny_span_regressions} tiny-span cases where the anchor cost more]",
        (saved as f64 / str_replace_total as f64) * 100.0
    );

    assert!(
        anchored_total < str_replace_total,
        "anchored batch total ({anchored_total}) must undercut str_replace \
         ({str_replace_total}) despite per-op line:hash overhead"
    );
    // The anchor-overhead exception must stay the exception: at most the
    // one-liner control per language, never an ambiguity case.
    assert!(
        tiny_span_regressions <= langs.len(),
        "anchor overhead exceeded str_replace on {tiny_span_regressions} cases — \
         more than the tiny-span controls can explain"
    );
}

#[test]
fn anchored_reads_and_edits_are_deterministic_across_languages() {
    // Determinism contract (#498), analogous to
    // `process_mode_output_is_byte_stable_across_calls`: anchored reads are a
    // pure function of content, and an identical anchored edit yields identical
    // bytes — so provider prompt caching is never defeated by anchoring.
    let dir = tempfile::tempdir().unwrap();
    for lang in &langs() {
        // Read side: annotate is byte-stable across calls.
        assert_eq!(
            annotate(lang.amb_src, 1),
            annotate(lang.amb_src, 1),
            "[{}] anchored read must be byte-stable",
            lang.name
        );

        // Edit side: the same op against two copies produces identical files.
        let (ok_a, out_a) = fix_anchored(
            dir.path(),
            &format!("{}_det_a", lang.name),
            lang.ext,
            lang.amb_src,
            lang.amb_line,
            lang.amb_buggy,
            lang.amb_fixed,
        );
        let (ok_b, out_b) = fix_anchored(
            dir.path(),
            &format!("{}_det_b", lang.name),
            lang.ext,
            lang.amb_src,
            lang.amb_line,
            lang.amb_buggy,
            lang.amb_fixed,
        );
        assert!(ok_a && ok_b, "[{}] both anchored edits succeed", lang.name);
        assert_eq!(
            out_a, out_b,
            "[{}] identical edit → identical bytes",
            lang.name
        );
    }
}
