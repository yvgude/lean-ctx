// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx doctor --fix` support for invalid `*`-placement allowlist
//! entries (design doc §4: bare `*`, embedded `*`, or more than one `*`).
//!
//! Comments out the offending entry directly in the raw `shell_allowlist` /
//! `shell_allowlist_extra` arrays of the global `config.toml`, using a
//! format-preserving `toml_edit` edit — never deletes the text outright,
//! since a dotfile config isn't typically under version control and
//! deletion would destroy the only record of what was there and why.
//!
//! Naturally idempotent: a fixed entry is removed from the *live* array, so
//! a later pass can never see it as invalid again — there is no separate
//! marker-comment detection step to get out of sync with the fix itself.

use toml_edit::{Array, DocumentMut};

const ALLOWLIST_KEYS: [&str; 2] = ["shell_allowlist", "shell_allowlist_extra"];

/// Applies the fix to an in-memory document (pure, no I/O — the part that's
/// actually worth unit testing in isolation).
pub(crate) fn fix_document(doc: &mut DocumentMut) -> Vec<String> {
    let mut results = Vec::new();
    for key in ALLOWLIST_KEYS {
        let Some(item) = doc.get_mut(key) else {
            continue;
        };
        let Some(array) = item.as_array_mut() else {
            continue;
        };
        results.extend(fix_array(array, key));
    }
    results
}

/// One array's own pass: find invalid string entries, then remove them in
/// reverse index order (so removing one never shifts the index of another
/// not-yet-processed entry), attaching an explanatory comment at the point
/// each one used to occupy.
fn fix_array(array: &mut Array, key: &str) -> Vec<String> {
    let invalid: Vec<(usize, String)> = array
        .iter()
        .enumerate()
        .filter_map(|(i, v)| {
            let s = v.as_str()?;
            (!crate::core::shell_allowlist::is_valid_allowlist_entry(s)).then(|| (i, s.to_string()))
        })
        .collect();

    let mut results = Vec::new();
    for (idx, entry_text) in invalid.into_iter().rev() {
        array.remove(idx);
        attach_disabled_comment(array, idx, &entry_text);
        results.push(format!(
            "disabled  {key} entry '{entry_text}' (invalid `*` placement)"
        ));
    }
    results
}

/// Writes the marker comment at the position `idx` used to occupy (now
/// either the next surviving element, if any, or the array's own trailing
/// whitespace/comment area if the removed entry was last). Always ends the
/// comment block in a real newline — a `#` comment runs to end of line, so
/// whatever follows on the same source line would otherwise be silently
/// swallowed into it.
fn attach_disabled_comment(array: &mut Array, idx: usize, entry_text: &str) {
    let comment = format!(
        "\n# lean-ctx doctor --fix: disabled — \"*\" must be a trailing token, not\n\
         # embedded (see `lean-ctx allow --help`)\n\
         # \"{entry_text}\"\n"
    );
    if idx < array.len() {
        if let Some(next) = array.get_mut(idx) {
            let existing = next
                .decor()
                .prefix()
                .and_then(|r| r.as_str())
                .unwrap_or_default()
                .to_string();
            next.decor_mut().set_prefix(format!("{comment}{existing}"));
        }
    } else {
        let existing = array.trailing().as_str().unwrap_or_default().to_string();
        array.set_trailing(format!("{comment}{existing}"));
    }
}

/// Non-interactive auto-heal used by `doctor --fix` (mirrors
/// `rules_dedup::auto_apply`'s shape: one result line per fixed entry).
/// Reads and writes the global `config.toml` only — project-local overrides
/// are out of scope, same reasoning as the rest of `--fix`'s mechanics: the
/// global config is the one dotfile with no other undo path.
pub(crate) fn auto_apply() -> Vec<String> {
    let Some(path) = crate::core::config::Config::path() else {
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(mut doc) = content.parse::<DocumentMut>() else {
        return vec![format!(
            "FAILED    {} (invalid TOML, left untouched)",
            path.display()
        )];
    };

    let results = fix_document(&mut doc);
    if results.is_empty() {
        return results;
    }

    match std::fs::write(&path, doc.to_string()) {
        Ok(()) => results,
        Err(e) => vec![format!("FAILED    {} ({e})", path.display())],
    }
}

#[cfg(test)]
mod tests {
    use super::fix_document;
    use toml_edit::DocumentMut;

    /// Parses `toml`, runs the fix, and returns (result lines, rendered
    /// output). Asserts the output is itself valid, re-parseable TOML —
    /// every test implicitly checks this, since a broken edit would fail
    /// this parse before any of the test's own assertions run.
    fn run(toml: &str) -> (Vec<String>, String) {
        let mut doc = toml
            .parse::<DocumentMut>()
            .expect("input must be valid TOML");
        let results = fix_document(&mut doc);
        let out = doc.to_string();
        out.parse::<DocumentMut>()
            .unwrap_or_else(|e| panic!("fix produced invalid TOML: {e}\n---\n{out}"));
        (results, out)
    }

    fn live_values(out: &str, key: &str) -> Vec<String> {
        out.parse::<DocumentMut>()
            .unwrap()
            .get(key)
            .and_then(|i| i.as_array().cloned())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn no_invalid_entries_is_a_no_op() {
        let (results, out) = run(r#"shell_allowlist_extra = ["git", "cargo"]"#);
        assert!(results.is_empty());
        assert_eq!(
            live_values(&out, "shell_allowlist_extra"),
            vec!["git", "cargo"]
        );
    }

    #[test]
    fn single_item_array_invalid_entry_becomes_trailing_comment() {
        let (results, out) = run(r#"shell_allowlist_extra = ["git * push"]"#);
        assert_eq!(results.len(), 1);
        assert!(results[0].contains("git * push"));
        assert!(live_values(&out, "shell_allowlist_extra").is_empty());
        assert!(out.contains("lean-ctx doctor --fix: disabled"));
        assert!(out.contains(r#"# "git * push""#));
    }

    #[test]
    fn compact_array_middle_item_invalid() {
        let (results, out) = run(r#"shell_allowlist_extra = ["git", "git * push", "cargo"]"#);
        assert_eq!(results.len(), 1);
        assert_eq!(
            live_values(&out, "shell_allowlist_extra"),
            vec!["git", "cargo"],
            "the two valid neighbors must survive, in order: {out}"
        );
        assert!(out.contains(r#"# "git * push""#));
    }

    #[test]
    fn compact_array_first_item_invalid() {
        let (results, out) = run(r#"shell_allowlist_extra = ["*", "git", "cargo"]"#);
        assert_eq!(results.len(), 1);
        assert_eq!(
            live_values(&out, "shell_allowlist_extra"),
            vec!["git", "cargo"]
        );
        assert!(out.contains("# \"*\""));
    }

    #[test]
    fn compact_array_last_item_invalid() {
        let (results, out) = run(r#"shell_allowlist_extra = ["git", "cargo", "* *"]"#);
        assert_eq!(results.len(), 1);
        assert_eq!(
            live_values(&out, "shell_allowlist_extra"),
            vec!["git", "cargo"]
        );
        assert!(out.contains("# \"* *\""));
    }

    #[test]
    fn pretty_array_middle_item_invalid() {
        let input = "shell_allowlist_extra = [\n    \"git status\",\n    \"git * push\",\n    \"cargo\",\n]\n";
        let (results, out) = run(input);
        assert_eq!(results.len(), 1);
        assert_eq!(
            live_values(&out, "shell_allowlist_extra"),
            vec!["git status", "cargo"]
        );
        assert!(out.contains(r#"# "git * push""#));
        // The surviving entries' own multi-line formatting must be intact.
        assert!(out.contains("\"git status\""));
        assert!(out.contains("\"cargo\""));
    }

    #[test]
    fn pretty_array_last_item_invalid() {
        let input =
            "shell_allowlist_extra = [\n    \"git status\",\n    \"cargo\",\n    \"* extra\",\n]\n";
        let (results, out) = run(input);
        assert_eq!(results.len(), 1);
        assert_eq!(
            live_values(&out, "shell_allowlist_extra"),
            vec!["git status", "cargo"]
        );
        assert!(out.contains(r#"# "* extra""#));
    }

    #[test]
    fn pretty_array_first_item_invalid() {
        let input =
            "shell_allowlist_extra = [\n    \"* first\",\n    \"git status\",\n    \"cargo\",\n]\n";
        let (results, out) = run(input);
        assert_eq!(results.len(), 1);
        assert_eq!(
            live_values(&out, "shell_allowlist_extra"),
            vec!["git status", "cargo"]
        );
        assert!(out.contains(r#"# "* first""#));
    }

    #[test]
    fn multiple_invalid_entries_in_one_array_all_get_fixed() {
        let (results, out) =
            run(r#"shell_allowlist_extra = ["*", "git status", "git * push", "cargo", "* *"]"#);
        assert_eq!(results.len(), 3, "got: {results:?}");
        assert_eq!(
            live_values(&out, "shell_allowlist_extra"),
            vec!["git status", "cargo"],
            "all three invalid entries removed, both valid ones survive in order: {out}"
        );
        for needle in [r#"# "*""#, r#"# "git * push""#, r#"# "* *""#] {
            assert!(out.contains(needle), "missing {needle}: {out}");
        }
    }

    #[test]
    fn catches_bare_embedded_and_repeated_star() {
        let (results, out) =
            run(r#"shell_allowlist_extra = ["*", "git * push", "* *", "git status *"]"#);
        assert_eq!(results.len(), 3, "got: {results:?}");
        assert_eq!(
            live_values(&out, "shell_allowlist_extra"),
            vec!["git status *"],
            "only the validly-placed trailing '*' entry survives: {out}"
        );
    }

    #[test]
    fn both_allowlist_keys_are_fixed_independently() {
        let (results, out) = run("shell_allowlist = [\"git\", \"git * push\"]\n\
             shell_allowlist_extra = [\"cargo\", \"*\"]\n");
        assert_eq!(results.len(), 2, "got: {results:?}");
        assert_eq!(live_values(&out, "shell_allowlist"), vec!["git"]);
        assert_eq!(live_values(&out, "shell_allowlist_extra"), vec!["cargo"]);
    }

    #[test]
    fn second_run_is_a_no_op_idempotent() {
        let (first_results, first_out) =
            run(r#"shell_allowlist_extra = ["git", "git * push", "cargo"]"#);
        assert_eq!(first_results.len(), 1);

        let mut doc = first_out.parse::<DocumentMut>().unwrap();
        let second_results = fix_document(&mut doc);
        assert!(
            second_results.is_empty(),
            "re-running must not re-flag an already-fixed entry: {second_results:?}"
        );
        assert_eq!(
            doc.to_string(),
            first_out,
            "second run must not change the document"
        );
    }
}
