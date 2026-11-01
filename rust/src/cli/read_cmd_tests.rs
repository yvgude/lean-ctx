// SPDX-License-Identifier: Apache-2.0
//! Unit tests of `read_cmd.rs`, moved out of that file unchanged (file-size
//! split). Each module keeps its name; `use super::*` re-exports the parent
//! module's items so that the modules' own `use super::…` lines still hold.

#[allow(unused_imports)]
use super::*;

#[cfg(all(test, unix))]
mod empty_daemon_tests {
    use super::is_failed_daemon_result;

    #[test]
    fn detects_tcc_empty_stub() {
        assert!(is_failed_daemon_result(
            "file.rs 0L
"
        ));
        assert!(is_failed_daemon_result(
            "F1=file.rs 0L
[lean-ctx: 0 tok saved]"
        ));
    }

    #[test]
    fn accepts_real_content() {
        assert!(!is_failed_daemon_result(
            "file.rs 10L
fn main() {}
"
        ));
    }

    #[test]
    fn accepts_nonzero_line_count() {
        assert!(!is_failed_daemon_result(
            "file.rs 1L
"
        ));
    }

    #[test]
    fn ignores_structure_markers_in_body() {
        assert!(is_failed_daemon_result(
            "file.rs 0L
[no extractable structure for .rs]
"
        ));
    }

    #[test]
    fn detects_tcc_restricted_marker() {
        assert!(is_failed_daemon_result(
            "ERROR:TCC_RESTRICTED: daemon cannot access /Users/me/Documents/proj (sandboxed)"
        ));
    }

    #[test]
    fn detects_daemon_project_root_denial() {
        assert!(is_failed_daemon_result(
            "ERROR: path escapes project root: /work/current/scripts (root: /work/stale)"
        ));
    }
}

#[cfg(test)]
mod cap_tests {
    use super::{cap_cli_to_raw, count_tokens};

    #[test]
    fn caps_to_raw_when_framing_inflates() {
        // A tiny file: the `path [NL]` header (+ any footer) pushes the framed
        // payload past the bare content, so the cap must ship the content
        // verbatim — the additive CLI default must never inflate a read (#361).
        let raw = "x = 1\n";
        let raw_tokens = count_tokens(raw);
        let framed = format!("some/very/long/path/header.rs [1L]\n{raw}\n[lean-ctx: 0 tok saved]");
        assert!(count_tokens(&framed) > raw_tokens, "fixture must inflate");
        assert_eq!(cap_cli_to_raw(framed, raw, raw_tokens), raw);
    }

    #[test]
    fn keeps_framing_when_it_saves() {
        // A genuinely compressed payload (fewer tokens than raw) is kept as-is.
        let raw = "fn a() {}\n".repeat(300);
        let raw_tokens = count_tokens(&raw);
        let framed = "f.rs [300L]\nfn a() {} …".to_string();
        assert!(count_tokens(&framed) < raw_tokens);
        assert_eq!(cap_cli_to_raw(framed.clone(), &raw, raw_tokens), framed);
    }

    #[test]
    fn keeps_framing_for_empty_file() {
        // raw_tokens == 0 disables the cap so an empty file still gets a signal.
        let framed = "empty.rs [0L]\n".to_string();
        assert_eq!(cap_cli_to_raw(framed.clone(), "", 0), framed);
    }

    #[test]
    fn break_even_is_not_inflation() {
        // Equal token counts use strict `>`, so framing is preserved at break-even.
        let raw = "alpha beta gamma delta";
        let raw_tokens = count_tokens(raw);
        let framed = raw.to_string();
        assert_eq!(count_tokens(&framed), raw_tokens);
        assert_eq!(cap_cli_to_raw(framed.clone(), raw, raw_tokens), framed);
    }

    #[test]
    fn emitted_never_exceeds_raw_across_sizes() {
        // The invariant itself: for any bloated framing over a non-empty file the
        // emitted token count is ≤ the raw token count.
        for n in [1usize, 5, 50, 500] {
            let raw = "data line here\n".repeat(n);
            let raw_tokens = count_tokens(&raw);
            let framed = format!("a/b/c/path.txt [{n}L]\n{raw}\n[lean-ctx: {n} tok saved ({n}%)]");
            let out = cap_cli_to_raw(framed, &raw, raw_tokens);
            assert!(
                count_tokens(&out) <= raw_tokens,
                "n={n}: emitted {} tok exceeds raw {raw_tokens}",
                count_tokens(&out)
            );
        }
    }
}

#[cfg(test)]
mod fresh_tests {
    use super::{cmd_read, should_force_fresh};

    #[test]
    fn invalid_request_returns_control_to_lifecycle_caller() {
        assert!(!cmd_read(&[]));
    }

    #[test]
    fn hook_child_forces_fresh_even_without_flags() {
        // #1037: a hook child must always read verbatim (no `cached … [NL]` stub),
        // even when the caller passed no `--fresh`/`--no-cache` flag.
        assert!(should_force_fresh(&[], true));
        assert!(!should_force_fresh(&[], false));
    }

    #[test]
    fn explicit_flags_force_fresh() {
        assert!(should_force_fresh(&["--fresh".to_string()], false));
        assert!(should_force_fresh(&["--no-cache".to_string()], false));
        assert!(!should_force_fresh(&["file.rs".to_string()], false));
    }
}

#[cfg(test)]
mod mode_arg_tests {
    use super::resolve_cli_read_mode;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    // `lean-ctx read f.ps1 map` used to silently IGNORE the positional mode and
    // serve the `auto` default — reads must honour it like `--mode map`
    // (limitations audit 2026-07-03).
    #[test]
    fn positional_mode_is_honoured() {
        assert_eq!(resolve_cli_read_mode(&args(&["f.ps1", "map"])), "map");
        assert_eq!(
            resolve_cli_read_mode(&args(&["f.rs", "lines:5-10"])),
            "lines:5-10"
        );
    }

    #[test]
    fn mode_flag_wins_over_positional() {
        assert_eq!(
            resolve_cli_read_mode(&args(&["f.rs", "map", "--mode", "signatures"])),
            "signatures"
        );
        assert_eq!(
            resolve_cli_read_mode(&args(&["f.rs", "-m", "full"])),
            "full"
        );
    }

    #[test]
    fn defaults_to_auto_and_skips_flags_and_junk() {
        assert_eq!(resolve_cli_read_mode(&args(&["f.rs"])), "auto");
        assert_eq!(resolve_cli_read_mode(&args(&["f.rs", "--fresh"])), "auto");
        // An unknown positional is not silently treated as a mode.
        assert_eq!(resolve_cli_read_mode(&args(&["f.rs", "banana"])), "auto");
    }
}

/// #1889: the CLI and the MCP server must render the same symbol lines (and
/// the same TDD legend) for map/signatures under every CRP mode.
#[cfg(test)]
mod crp_parity_tests {
    use super::push_signature_lines;
    use crate::core::cache::SessionCache;
    use crate::core::signatures;
    use crate::tools::CrpMode;

    /// Bodies are long so map/signatures are clearly smaller than the file —
    /// otherwise the MCP never-inflate guard serves the raw file instead.
    fn src() -> String {
        use std::fmt::Write as _;
        let body = (0..60).fold(String::new(), |mut acc, i| {
            let _ = writeln!(acc, "    let v{i} = source.len() as u64 + {i};");
            acc
        });
        format!(
            "pub struct Totals {{ pub n: u64 }}\n\n\
             pub fn count_source(source: &'static str) -> u64 {{\n{body}    0\n}}\n\n\
             pub fn source_counts() -> Vec<(&'static str, u64)> {{\n    let source = \"\";\n{body}    Vec::new()\n}}\n\n\
             fn private_helper(x: i32) -> i32 {{\n    let source = \"\";\n{body}    x + 1\n}}\n"
        )
    }

    fn mcp(mode: &str, crp: CrpMode) -> String {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("parity.rs");
        std::fs::write(&file, src()).unwrap();
        let mut cache = SessionCache::new();
        crate::tools::ctx_read::handle_fresh(&mut cache, &file.to_string_lossy(), mode, crp)
    }

    fn assert_lines_in(cli: &str, mcp: &str, what: &str) {
        for line in cli.lines().map(str::trim).filter(|l| !l.is_empty()) {
            assert!(
                mcp.lines()
                    .any(|m| m.trim_start().starts_with(line) || m.contains(line)),
                "{what}: CLI line `{line}` missing from MCP output:\n{mcp}"
            );
        }
    }

    #[test]
    fn signatures_match_mcp_for_every_crp_mode() {
        let sigs = signatures::extract_signatures(&src(), "rs");
        let refs: Vec<&signatures::Signature> = sigs.iter().collect();
        for crp in [CrpMode::Off, CrpMode::Compact, CrpMode::Tdd] {
            let mut cli = String::new();
            push_signature_lines(&mut cli, &refs, crp, "", false);
            assert_lines_in(&cli, &mcp("signatures", crp), crp.as_str());
            assert_eq!(
                cli.contains("λ=fn"),
                crp.is_tdd(),
                "legend iff tdd ({crp:?})"
            );
        }
    }

    #[test]
    fn map_api_matches_mcp_for_every_crp_mode() {
        let sigs = signatures::extract_signatures(&src(), "rs");
        let key: Vec<&signatures::Signature> = sigs
            .iter()
            .filter(|s| s.is_exported || s.indent == 0)
            .collect();
        for crp in [CrpMode::Off, CrpMode::Compact, CrpMode::Tdd] {
            let mut cli = String::from("  API:");
            push_signature_lines(&mut cli, &key, crp, "    ", true);
            let out = mcp("map", crp);
            let api_header = cli.lines().next().unwrap();
            assert!(
                out.contains(api_header),
                "{crp:?}: header `{api_header}` in\n{out}"
            );
            assert_lines_in(&cli, &out, crp.as_str());
        }
    }

    #[test]
    fn tdd_uses_symbol_notation_not_compact() {
        let sigs = signatures::extract_signatures(&src(), "rs");
        let refs: Vec<&signatures::Signature> = sigs.iter().collect();
        let mut tdd = String::new();
        push_signature_lines(&mut tdd, &refs, CrpMode::Tdd, "", false);
        assert!(tdd.contains("λ+count_source"), "{tdd}");
        assert!(!tdd.contains("fn pub count_source"), "{tdd}");
    }
}

/// #1917: `lean-ctx grep` must not report "not found" (exit 1) when it never
/// looked at part of the corpus.
#[cfg(test)]
mod grep_exit_status_tests {
    use super::grep_exit_status;

    #[test]
    fn matches_exit_zero() {
        assert_eq!(grep_exit_status("3 matches in 1 files:\nsrc/a.rs:1 x"), 0);
    }

    #[test]
    fn complete_miss_exits_one() {
        assert_eq!(grep_exit_status("0 matches for 'x' (scanned 12 files)"), 1);
    }

    #[test]
    fn incomplete_miss_exits_two() {
        for out in [
            "0 matches for 'x' (scanned 1 files) (1 large files skipped: big.log)",
            "0 matches for 'x' (scanned 4 files)\n(2 files skipped: binary/encoding)",
            "0 matches for 'x' (scanned 9 files) (search stopped at the time budget)",
        ] {
            assert_eq!(grep_exit_status(out), 2, "{out}");
        }
    }

    #[test]
    fn errors_exit_two() {
        assert_eq!(grep_exit_status("ERROR: /nope does not exist"), 2);
        assert_eq!(grep_exit_status("  ERROR: refusing to scan /"), 2);
    }
}

#[cfg(test)]
mod diff_admission_tests {
    use super::admitted_diff_sides;

    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    /// G01: `lean-ctx diff` read both files with the unscoped reader, which
    /// outside a request scope is a raw local read — the diff printed a key
    /// that `lean-ctx read` masks. Both sides are now admitted first; clean
    /// sides keep their bytes.
    #[test]
    fn both_sides_of_a_diff_are_admitted_and_clean_sides_keep_their_bytes() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old.rs");
        let new = dir.path().join("new.rs");
        let keyed = dir.path().join("keyed.rs");
        std::fs::write(&old, "fn a() {}\n").unwrap();
        std::fs::write(&new, "fn a() {}\nfn b() {}\n").unwrap();
        std::fs::write(&keyed, format!("let aws_key = \"{AWS_KEY}\";\n")).unwrap();

        let (left, right) =
            admitted_diff_sides(&old.to_string_lossy(), &new.to_string_lossy()).unwrap();
        assert_eq!(left, "fn a() {}\n");
        assert_eq!(right, "fn a() {}\nfn b() {}\n");

        let (_, right) =
            admitted_diff_sides(&old.to_string_lossy(), &keyed.to_string_lossy()).unwrap();
        assert!(!right.contains(AWS_KEY), "{right}");
        assert!(right.starts_with("let "), "{right}");
    }

    #[test]
    fn a_withheld_side_ends_the_diff_with_a_content_free_reason() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let policy = crate::core::policy::load(
            "name = \"cli-diff-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let dir = tempfile::tempdir().unwrap();
        let open = dir.path().join("open.txt");
        let plan = dir.path().join("plan.txt");
        std::fs::write(&open, "public notes\n").unwrap();
        std::fs::write(&plan, "CONFIDENTIAL\nroadmap-marker\n").unwrap();
        let message = admitted_diff_sides(&open.to_string_lossy(), &plan.to_string_lossy())
            .expect_err("a blocked side is withheld");
        assert!(message.starts_with("Error reading "), "{message}");
        assert!(!message.contains("roadmap-marker"), "{message}");
    }
}

/// G03/G04: `read`, `grep`, `glob` and `ls` are the carriers host hooks
/// rewrite native reads to. Their output was admitted but printed from inside
/// the primitive: no source capture, no delivered record — unknown lineage
/// for the proxy. They now run through the shared CLI tool run.
#[cfg(test)]
mod carrier_lineage_tests {
    use super::*;

    struct Fixture {
        _data: crate::core::data_dir::IsolatedDataDir,
        _project: tempfile::TempDir,
        root: String,
    }

    /// Governed mode without a pack: protection is active, so admitted reads
    /// publish their sources. The daemon route is switched off.
    fn governed() -> Fixture {
        let data = crate::core::data_dir::isolated_data_dir();
        crate::test_env::set_var("__LEAN_CTX_NO_DAEMON", "1");
        let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::create_dir_all(&dir).expect("config dir");
        std::fs::write(
            dir.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("config");
        let project = tempfile::tempdir().expect("project");
        let root = std::fs::canonicalize(project.path())
            .expect("root")
            .to_string_lossy()
            .into_owned();
        Fixture {
            _data: data,
            _project: project,
            root,
        }
    }

    impl Fixture {
        fn file(&self, name: &str, content: &str) -> String {
            let path = Path::new(&self.root).join(name);
            std::fs::write(&path, content).expect("fixture");
            path.to_string_lossy().into_owned()
        }

        /// The lineage on record for exactly `printed`, judged inside the
        /// project scope as the proxy judges it.
        fn lineage(&self, printed: &str) -> Option<bool> {
            crate::tools::admitted_sources::as_agent_acquisition(&self.root, || {
                crate::core::archive::delivered::lineage_of(
                    &crate::core::archive::delivered::digest(printed),
                )
                .map(|lineage| lineage.still_admitted().is_ok())
            })
        }

        /// Run a primitive as a carrier and return the bytes it wrote to
        /// stdout, without the final newline of the closing `println!`.
        fn carried(
            &self,
            tool: &str,
            arguments: serde_json::Value,
            listing: bool,
            primitive: impl FnOnce(&mut Stdout) -> CommandOutput,
        ) -> (CommandOutput, String) {
            let arguments = tool_arguments(arguments);
            let mut written = String::new();
            let output = deliver_carrier(tool, &self.root, Some(&arguments), listing, |out| {
                let output = primitive(out);
                written = out.text.strip_suffix('\n').unwrap_or_default().to_string();
                output
            });
            (output, written)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            crate::test_env::remove_var("__LEAN_CTX_NO_DAEMON");
        }
    }

    #[test]
    fn a_read_is_registered_for_exactly_the_printed_bytes_and_follows_its_source() {
        let fixture = governed();
        let content = "pub fn sample() -> u32 { 7 }\n";
        let path = fixture.file("sample.rs", content);
        let root = fixture.root.clone();
        let (output, written) = fixture.carried(
            "ctx_read",
            serde_json::json!({ "path": path, "mode": "full" }),
            false,
            |out| local_read(&path, &root, "full", true, false, out),
        );
        assert_eq!(output.exit_code, 0);
        // Byte identity (#498): a fresh full read of a small file prints the
        // file itself — what the `println!` in the primitive printed before.
        assert_eq!(written, content);
        assert_eq!(fixture.lineage(&written), Some(true));
        assert_eq!(fixture.lineage(&format!("{written}\n")), Some(true));

        std::fs::remove_file(&path).expect("delete source");
        assert_eq!(
            fixture.lineage(&written),
            Some(false),
            "found, not admitted"
        );
    }

    #[test]
    fn output_with_a_hint_or_an_error_is_printed_but_not_registered() {
        let fixture = governed();
        let content = "line one\nline two\nline three\n";
        let path = fixture.file("notes.txt", content);
        let root = fixture.root.clone();

        // A comma multi-select appends the usage hint to the selection.
        let (output, written) = fixture.carried(
            "ctx_read",
            serde_json::json!({ "path": path, "mode": "lines:1-1,3-3" }),
            false,
            |out| local_read(&path, &root, "lines:1-1,3-3", true, false, out),
        );
        assert_eq!(output.exit_code, 0);
        assert!(written.contains("line one"), "{written}");
        assert_eq!(fixture.lineage(&written), None, "hinted output");

        // The same source without a hint is a delivery.
        let (_, plain) = fixture.carried(
            "ctx_read",
            serde_json::json!({ "path": path, "mode": "lines:1-2" }),
            false,
            |out| local_read(&path, &root, "lines:1-2", true, false, out),
        );
        assert_eq!(fixture.lineage(&plain), Some(true));

        // A primitive that fails after writing is never a delivery.
        let (_, failed) = fixture.carried(
            "ctx_read",
            serde_json::json!({ "path": path, "mode": "full" }),
            false,
            |out| {
                let _ = local_read(&path, &root, "full", true, false, out);
                out.line("trailing diagnostic");
                CommandOutput::local(1, None)
            },
        );
        assert_eq!(fixture.lineage(&failed), None);
    }

    #[test]
    fn grep_is_registered_for_the_matched_files_and_listings_are_not() {
        let fixture = governed();
        let hit = fixture.file("hit.rs", "pub fn needle_marker() {}\n");
        let miss = fixture.file("miss.rs", "pub fn unrelated() {}\n");
        let root = fixture.root.clone();

        let (output, written) = fixture.carried(
            "ctx_search",
            serde_json::json!({ "pattern": "needle_marker", "path": root }),
            false,
            |out| local_grep("needle_marker".to_string(), root.clone(), out),
        );
        assert_eq!(output.exit_code, 0, "{written}");
        assert!(written.contains("needle_marker"), "{written}");
        assert_eq!(fixture.lineage(&written), Some(true));
        // A file that did not match is no source of the result.
        std::fs::remove_file(&miss).expect("delete unmatched file");
        assert_eq!(fixture.lineage(&written), Some(true));
        std::fs::remove_file(&hit).expect("delete matched file");
        assert_eq!(fixture.lineage(&written), Some(false));

        // Listings are released and printed, never registered.
        fixture.file("listed.rs", "pub fn listed() {}\n");
        let (output, listing) = fixture.carried("ctx_tree", serde_json::json!({}), true, |out| {
            local_ls(&root, 2, false, true, out)
        });
        assert_eq!(output.exit_code, 0);
        assert!(listing.contains("listed.rs"), "{listing}");
        assert_eq!(fixture.lineage(&listing), None);
        let (_, globbed) = fixture.carried("ctx_glob", serde_json::json!({}), true, |out| {
            local_glob("*.rs", &root, out)
        });
        assert!(globbed.contains("listed.rs"), "{globbed}");
        assert_eq!(fixture.lineage(&globbed), None);
    }

    /// The lineage of a grep result is bound to the MATCHED files only. An
    /// admitted file without a match is no source of the result: changing or
    /// deleting it leaves the entry admitted. A change of the matched file
    /// ends it.
    #[test]
    fn grep_lineage_follows_the_matched_file_and_ignores_the_unmatched_one() {
        let fixture = governed();
        let hit = fixture.file("hit.rs", "pub fn needle_marker() {}\n");
        let miss = fixture.file("miss.rs", "pub fn unrelated() {}\n");
        let root = fixture.root.clone();

        let (output, written) = fixture.carried(
            "ctx_search",
            serde_json::json!({ "pattern": "needle_marker", "path": root }),
            false,
            |out| local_grep("needle_marker".to_string(), root.clone(), out),
        );
        assert_eq!(output.exit_code, 0, "{written}");
        assert!(written.contains("needle_marker"), "{written}");
        assert!(!written.contains("miss.rs"), "{written}");
        assert_eq!(fixture.lineage(&written), Some(true));

        std::fs::write(&miss, "pub fn unrelated_and_changed() {}\n").expect("change unmatched");
        assert_eq!(
            fixture.lineage(&written),
            Some(true),
            "a changed file without a match is no source of the result"
        );
        std::fs::remove_file(&miss).expect("delete unmatched");
        assert_eq!(fixture.lineage(&written), Some(true));

        std::fs::write(
            &hit,
            "pub fn needle_marker() {}\n// edited after the search\n",
        )
        .expect("change matched");
        assert_eq!(
            fixture.lineage(&written),
            Some(false),
            "the matched file changed: found, no longer admitted"
        );
    }

    /// The collected stdout is exactly what the `println!` calls wrote.
    #[test]
    fn collected_stdout_reproduces_println_bytes() {
        let mut out = Stdout::default();
        assert!(Stdout::default().into_printable(false).is_none());
        out.line("header [2L]");
        out.line("a\nb\n");
        let printable = out.into_printable(false).expect("text");
        // `println!` of this text gives back "header [2L]\na\nb\n\n".
        assert_eq!(printable.text, "header [2L]\na\nb\n");
        assert!(printable.source_only && !printable.failed);
    }
}
