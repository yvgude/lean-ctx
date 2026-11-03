#[allow(unused_imports)]
use super::*;

/// #1286: `ls -la` long-format output must reach the structural ls compressor
/// (name + size per entry) instead of degrading to the generic terse fallback.
/// Regression guard for two independent breakages: the zsh spawn preamble
/// leaking into the classified command, and the terse stage displacing a
/// smaller pattern result.
#[cfg(test)]
mod ls_structural_compression {
    #[test]
    fn ls_long_listing_compresses_structurally() {
        let mut output = String::from("total 11168\n");
        for i in 0..400 {
            output.push_str(&format!(
                "-rw-r--r--@   1 yvesgugger  staff  {} Aug  9 21:51 file_{i}.rs\n",
                1000 + i
            ));
        }
        for i in 0..60 {
            output.push_str(&format!(
                "drwxr-xr-x@   5 yvesgugger  staff  160 Aug  9 21:51 dir_{i}\n"
            ));
        }
        let compressed =
            super::super::engine::compress_if_beneficial("ls -la rust/src/core", &output);
        assert!(
            compressed.len() < output.len() / 2,
            "structural ls compression must at least halve the payload \
             ({} -> {} bytes)",
            output.len(),
            compressed.len()
        );
        assert!(
            compressed.contains("400 files, 60 dirs"),
            "summary line from the ls compressor must be present"
        );
        assert!(
            compressed.contains("file_0.rs"),
            "every entry name must survive"
        );
    }
}

/// #342: already-compact TOON output must be preserved verbatim (not recompressed)
/// regardless of the command, because re-compressing it destroys the exact
/// line/field shape agents use to validate CLI output contracts.
#[cfg(test)]
mod toon_passthrough_tests {
    use super::super::engine::compress_if_beneficial;

    /// Builds a TOON tabular block large enough to clear the 30-token floor that
    /// otherwise returns small outputs unchanged anyway.
    fn toon_task_list(rows: usize) -> String {
        let mut out = String::from("task_count: ");
        out.push_str(&rows.to_string());
        out.push_str("\ntasks[");
        out.push_str(&rows.to_string());
        out.push_str("]{id,status,priority,title}:\n");
        for i in 0..rows {
            out.push_str(&format!(
                "  task-{i},open,p{},Implement feature number {i} end to end\n",
                i % 3
            ));
        }
        out
    }

    #[test]
    fn toon_output_is_preserved_verbatim() {
        let toon = toon_task_list(12);
        let result = compress_if_beneficial("vida task list", &toon);
        assert_eq!(
            result, toon,
            "TOON output must pass through unchanged, got: {result}"
        );
    }

    #[test]
    fn toon_passthrough_is_command_agnostic() {
        // A command lean-ctx would normally compress still passes TOON through,
        // because the decision is output-shape based, not command based.
        let toon = toon_task_list(20);
        let result = compress_if_beneficial("my-tool report --format toon", &toon);
        assert_eq!(result, toon, "TOON must pass through for any command");
    }

    #[test]
    fn non_toon_output_is_still_compressible() {
        // A noisy, repetitive log that is NOT TOON should not be forced verbatim
        // by the passthrough — the normal pipeline still applies.
        let mut log = String::new();
        for i in 0..200 {
            log.push_str(&format!(
                "2026-06-04T10:00:{:02}Z INFO worker processed item {i} successfully in 12ms\n",
                i % 60
            ));
        }
        let result = compress_if_beneficial("./run-batch.sh", &log);
        assert_ne!(
            result, log,
            "non-TOON noisy log should not be forced verbatim by the TOON passthrough"
        );
    }
}

/// Exit-code-aware compression (#809 / #810): a command that actually FAILED must
/// never be lossily compressed, so the agent always sees the real error and never
/// re-runs the command without lean-ctx.
#[cfg(test)]
mod outcome_aware_tests {
    use super::super::engine::{compress_and_measure, compress_for_outcome};

    /// A noisy, repetitive log a successful run WOULD compress.
    fn noisy_log() -> String {
        let mut log = String::new();
        for i in 0..200 {
            log.push_str(&format!(
                "2026-06-04T10:00:{:02}Z INFO worker processed item {i} successfully in 12ms\n",
                i % 60
            ));
        }
        log
    }

    fn small_noisy_log() -> String {
        let mut log = String::new();
        for i in 0..80 {
            log.push_str(&format!(
                "2026-06-04T10:00:{:02}Z INFO worker processed item {i} in 12ms\n",
                i % 60
            ));
        }
        log
    }

    #[test]
    fn failed_small_command_is_verbatim() {
        let log = small_noisy_log();
        let failed = compress_for_outcome("./run-batch.sh", &log, 1);
        assert_eq!(
            failed, log,
            "a failed command ≤2000 tokens must be preserved verbatim"
        );
    }

    #[test]
    fn failed_large_command_gets_compressed() {
        let log = noisy_log();
        let failed = compress_for_outcome("./run-batch.sh", &log, 1);
        assert_ne!(
            failed, log,
            "a failed command >2000 tokens should attempt pattern compression"
        );
    }

    #[test]
    fn succeeding_command_still_compresses() {
        let log = noisy_log();
        let ok = compress_for_outcome("./run-batch.sh", &log, 0);
        assert_ne!(
            ok, log,
            "a succeeding command's compressible output should still compress"
        );
    }

    #[test]
    fn empty_failed_output_stays_empty() {
        assert_eq!(compress_for_outcome("flaky-check", "   \n  ", 1), "");
    }

    #[test]
    fn failed_command_keeps_buried_error_line() {
        let mut out = String::from("starting deploy\n");
        out.push_str("uploading artifact 1 of 3\n");
        out.push_str("ERR_PERMISSION: cannot write to /opt/app (needle-9b1e4d77)\n");
        out.push_str("rolling back\n");
        let failed = compress_for_outcome("./deploy.sh prod", &out, 13);
        assert!(
            failed.contains("needle-9b1e4d77"),
            "the actual error line must survive on a failed command: {failed}"
        );
    }

    #[test]
    fn measure_labels_stderr_only_on_failure() {
        let (failed, _) = compress_and_measure("./build.sh", "compiling main", "linker error", 1);
        assert!(
            failed.contains(crate::shell::STDERR_LABEL),
            "failure with both streams must label stderr: {failed}"
        );
        assert!(failed.contains("compiling main") && failed.contains("linker error"));

        let (ok, _) = compress_and_measure("./build.sh", "compiling main", "note: ok", 0);
        assert!(
            !ok.contains(crate::shell::STDERR_LABEL),
            "success output must not inject the stderr label: {ok}"
        );
    }
}
/// #848: MSBuild parallel diagnostics dedup
#[test]
fn msbuild_parallel_dedup_removes_duplicate_diagnostics() {
    let output = "Microsoft (R) Build Engine\n\
Build started\n\
src/Foo.cs(10,5): warning CS1591: Missing XML comment [node1]\n\
src/Bar.cs(20,3): warning CS0168: Variable is declared but never used [node1]\n\
src/Foo.cs(10,5): warning CS1591: Missing XML comment [node2]\n\
src/Bar.cs(20,3): warning CS0168: Variable is declared but never used [node2]\n\
src/Baz.cs(30,1): error CS1002: ; expected [node1]\n\
Build succeeded with warnings.\n\
    2 Warning(s)\n\
    1 Error(s)\n";

    let deduped = super::engine::dedup_build_diagnostics(output);
    // Each diagnostic should appear only once
    assert_eq!(
        deduped.matches("CS1591").count(),
        1,
        "CS1591 should appear once after dedup: {deduped}"
    );
    assert_eq!(
        deduped.matches("CS0168").count(),
        1,
        "CS0168 should appear once after dedup: {deduped}"
    );
    // Error line should still be there
    assert!(deduped.contains("CS1002"), "error line must be preserved");
    // Dedup note should be present
    assert!(
        deduped.contains("duplicate diagnostic"),
        "dedup note missing: {deduped}"
    );
}

// #1130: chained commands must not be pattern-compressed
#[test]
fn chained_command_detection() {
    use super::engine::is_chained_command;
    assert!(is_chained_command("git fetch && git worktree add /tmp/wt"));
    assert!(is_chained_command("git status; git log --oneline -3"));
    assert!(!is_chained_command("git status"));
    assert!(!is_chained_command("echo 'a && b'"));
    assert!(!is_chained_command("echo \"a; b\""));
}

#[test]
fn chained_git_fetch_preserves_worktree_output() {
    let cmd = "git fetch origin feat && git worktree add /tmp/wt -b feat origin/feat";
    let output = "From github.com:org/repo\n * branch feat -> FETCH_HEAD\nPreparing worktree (new branch 'feat')\nHEAD is now at abc1234 initial commit\n";
    let compressed = super::engine::compress_if_beneficial_pub(cmd, output);
    assert!(
        compressed.contains("worktree") || compressed.contains("Preparing"),
        "#1130: worktree output must survive chain compression, got: {compressed}"
    );
}

// #1129: small output should NOT be compressed — verbatim is cheaper
#[test]
fn small_output_stays_verbatim() {
    let cmd = "ls -la /tmp/templates";
    let output: String = (0..25)
        .map(|i| format!("template_{i:02}.yaml"))
        .collect::<Vec<_>>()
        .join("\n");
    let compressed = super::engine::compress_if_beneficial_pub(cmd, &output);
    assert_eq!(
        compressed, output,
        "#1129: output under 120 tokens must stay verbatim, got: {compressed}"
    );
}

#[test]
fn large_long_ls_uses_the_dedicated_listing_pattern() {
    let cmd = "ls -la /tmp/templates";
    let mut output = String::from("total 160\n");
    for i in 0..40 {
        output.push_str(&format!(
            "-rw-r--r--  1 user staff  4096 May 20 10:00 template_{i:02}.yaml\n"
        ));
    }

    let compressed = super::engine::compress_if_beneficial_pub(cmd, &output);
    assert!(compressed.contains("template_00.yaml  4.0K"));
    assert!(compressed.contains("40 files, 0 dirs"));
    assert_ne!(compressed, output, "large ls output should be summarized");
}
