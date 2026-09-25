use super::*;

// --- grep/egrep rewrite (conservative: only safe patterns + flags) ---

#[test]
fn grep_simple_pattern_rewrites() {
    assert_eq!(
        rewrite_candidate("grep pattern src/", "lean-ctx"),
        Some("lean-ctx grep pattern src/".to_string())
    );
}

#[test]
fn grep_pattern_with_pipe_falls_through_to_native_grep() {
    // In BRE a bare `|` is a literal, but `lean-ctx grep` would read it as
    // alternation and report lines native grep never matches (#1827). The
    // quoting this test used to check now happens inside the `-c` wrap.
    assert_eq!(
        rewrite_candidate("grep -r \"TODO|FIXME\" .", "lean-ctx"),
        Some("lean-ctx -c 'grep -r \"TODO|FIXME\" .'".to_string())
    );
}

#[test]
fn egrep_pattern_with_pipe_still_rewrites() {
    // The same text under ERE really is alternation, and the Rust `regex`
    // crate agrees — so `egrep` keeps the direct rewrite.
    assert_eq!(
        rewrite_candidate("egrep -r \"TODO|FIXME\" .", "lean-ctx"),
        Some("lean-ctx grep 'TODO|FIXME' .".to_string())
    );
}

#[test]
fn grep_pattern_with_dollar_keeps_its_quoting_in_the_wrap() {
    // `"$HOME"` must still expand, so the command is not re-tokenized (#1862).
    assert_eq!(
        rewrite_candidate("grep -rn \"$HOME\" src/", "lean-ctx"),
        Some("lean-ctx -c 'grep -rn \"$HOME\" src/'".to_string())
    );
}

#[test]
fn grep_pattern_with_parens_falls_through_to_native_grep() {
    // BRE reads `(` and `)` as literals; `lean-ctx grep` reads them as a
    // group, so `func()` would match the text `func` (#1827).
    assert_eq!(
        rewrite_candidate("grep -n \"func()\" file.rs", "lean-ctx"),
        Some("lean-ctx -c 'grep -n \"func()\" file.rs'".to_string())
    );
}

#[test]
fn grep_pattern_with_star_is_quoted() {
    assert_eq!(
        rewrite_candidate("grep \"func.*Handler\" src/", "lean-ctx"),
        Some("lean-ctx grep 'func.*Handler' src/".to_string())
    );
}

#[test]
fn grep_n_flag_stripped() {
    assert_eq!(
        rewrite_candidate("grep -n pattern file.rs", "lean-ctx"),
        Some("lean-ctx grep pattern file.rs".to_string())
    );
}

#[test]
fn grep_rn_combined_safe_flags() {
    assert_eq!(
        rewrite_candidate("grep -rn pattern src/", "lean-ctx"),
        Some("lean-ctx grep pattern src/".to_string())
    );
}

#[test]
fn grep_rni_falls_through_because_i_semantic() {
    assert_eq!(
        rewrite_candidate("grep -rni pattern src/", "lean-ctx"),
        Some(expect_wrapped("grep -rni pattern src/", "lean-ctx"))
    );
}

#[test]
fn grep_no_path_rewrites() {
    assert_eq!(
        rewrite_candidate("grep -rn pattern", "lean-ctx"),
        Some("lean-ctx grep pattern".to_string())
    );
}

#[test]
fn egrep_rewrites_with_quoted_pattern() {
    assert_eq!(
        rewrite_candidate("egrep \"func|struct|impl\" src/", "lean-ctx"),
        Some("lean-ctx grep 'func|struct|impl' src/".to_string())
    );
}

#[test]
fn fgrep_always_falls_through() {
    assert_eq!(
        rewrite_candidate("fgrep literal_string file.rs", "lean-ctx"),
        Some(expect_wrapped("fgrep literal_string file.rs", "lean-ctx"))
    );
}

#[test]
fn grep_i_falls_through() {
    assert_eq!(
        rewrite_candidate("grep -i pattern file.rs", "lean-ctx"),
        Some(expect_wrapped("grep -i pattern file.rs", "lean-ctx"))
    );
}

#[test]
fn grep_w_falls_through() {
    assert_eq!(
        rewrite_candidate("grep -w pattern file.rs", "lean-ctx"),
        Some(expect_wrapped("grep -w pattern file.rs", "lean-ctx"))
    );
}

#[test]
fn grep_l_falls_through() {
    assert_eq!(
        rewrite_candidate("grep -l pattern src/", "lean-ctx"),
        Some(expect_wrapped("grep -l pattern src/", "lean-ctx"))
    );
}

#[test]
fn grep_include_falls_through() {
    assert_eq!(
        rewrite_candidate("grep -rn --include=*.rs pattern src/", "lean-ctx"),
        Some(expect_wrapped(
            "grep -rn --include=*.rs pattern src/",
            "lean-ctx"
        ))
    );
}

#[test]
fn grep_context_flags_fall_through() {
    assert_eq!(
        rewrite_candidate("grep -A5 pattern file.rs", "lean-ctx"),
        Some(expect_wrapped("grep -A5 pattern file.rs", "lean-ctx"))
    );
}

#[test]
fn grep_multiple_paths_falls_through() {
    assert_eq!(
        rewrite_candidate("grep -n pattern file1.rs file2.rs", "lean-ctx"),
        Some(expect_wrapped(
            "grep -n pattern file1.rs file2.rs",
            "lean-ctx"
        ))
    );
}

#[test]
fn grep_outside_project_falls_through() {
    assert_eq!(
        rewrite_candidate("grep pattern ~/Library/something", "lean-ctx"),
        Some(expect_wrapped(
            "grep pattern ~/Library/something",
            "lean-ctx"
        ))
    );
}

// --- rg: safe flags rewrite, semantic flags fall through ---

#[test]
fn rg_simple_rewrites() {
    assert_eq!(
        rewrite_candidate("rg pattern", "lean-ctx"),
        Some("lean-ctx grep pattern".to_string())
    );
    assert_eq!(
        rewrite_candidate("rg pattern src/", "lean-ctx"),
        Some("lean-ctx grep pattern src/".to_string())
    );
}

#[test]
fn rg_n_flag_rewrites() {
    assert_eq!(
        rewrite_candidate("rg -n pattern src/", "lean-ctx"),
        Some("lean-ctx grep pattern src/".to_string())
    );
}

#[test]
fn rg_hidden_flag_rewrites() {
    assert_eq!(
        rewrite_candidate("rg --hidden pattern src/", "lean-ctx"),
        Some("lean-ctx grep pattern src/".to_string())
    );
}

#[test]
fn rg_i_falls_through() {
    assert_eq!(
        rewrite_candidate("rg -i pattern src/", "lean-ctx"),
        Some(expect_wrapped("rg -i pattern src/", "lean-ctx"))
    );
    assert_eq!(
        rewrite_candidate("rg --ignore-case pattern src/", "lean-ctx"),
        Some(expect_wrapped("rg --ignore-case pattern src/", "lean-ctx"))
    );
}
