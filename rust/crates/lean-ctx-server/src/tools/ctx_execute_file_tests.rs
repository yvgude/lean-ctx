// SPDX-License-Identifier: Apache-2.0
use super::{handle_file, summarize_file_content};
use crate::server::tool_trait::ShellOutcome;
use lean_ctx::core::policy::runtime::TestPolicyOverride;

fn process(root: &std::path::Path, path: &std::path::Path) -> (String, ShellOutcome) {
    handle_file(&path.to_string_lossy(), None, Some(&root.to_string_lossy()))
}

fn policy(body: &str) -> lean_ctx::core::policy::ResolvedPolicy {
    lean_ctx::core::policy::load(&format!(
        "name = \"file-processing-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n{body}"
    ))
    .unwrap()
}

#[test]
fn actual_files_are_data_for_every_extension_and_need_no_runtime() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let _policy = TestPolicyOverride::set(Some(policy("")));
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    // Even executable-looking source is only summarized; no interpreter probes
    // or shell side effects are required to process the file.
    let marker = root.join("must-not-exist");
    let source = format!("from pathlib import Path\nPath({marker:?}).write_text('executed')\n");
    for extension in ["py", "js", "json", "txt", "rs", "unknown"] {
        let path = root.join(format!("sample.{extension}"));
        std::fs::write(&path, &source).unwrap();
        let (output, outcome) = process(root, &path);
        assert_eq!(outcome, ShellOutcome::Exit(0), "{extension}: {output}");
        assert!(output.starts_with("Admitted text: 2 lines,"), "{output}");
        assert!(output.contains("from pathlib import Path"));
        assert!(!marker.exists());
    }
}

#[test]
fn filtering_precedes_statistics_and_a_changed_rule_blocks_processing() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sample.txt");
    let input = "PUBLIC\nCustomer CUS-1234\n";
    std::fs::write(&path, input).unwrap();
    {
        let _policy =
            TestPolicyOverride::set(Some(policy("[redaction]\ncustomer = \"CUS-[0-9]{4}\"\n")));
        let (output, outcome) = process(temp.path(), &path);
        assert_eq!(outcome, ShellOutcome::Exit(0), "{output}");
        assert!(!output.contains("CUS-1234"));
        assert!(output.contains("[REDACTED:customer]"));
        let admitted = "PUBLIC\nCustomer [REDACTED:customer]\n";
        assert!(
            output.contains(&format!("{} bytes", admitted.len())),
            "{output}"
        );
    }
    {
        let _policy = TestPolicyOverride::set(Some(policy(
            "[filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )));
        std::fs::write(&path, input.replace("PUBLIC", "CONFIDENTIAL")).unwrap();
        let (output, outcome) = process(temp.path(), &path);
        assert_eq!(outcome, ShellOutcome::Blocked);
        assert!(!output.contains("CUS-1234"));
        assert!(!output.contains("Admitted text:"));
    }
}

#[test]
fn binary_and_outside_project_files_are_refused_before_summary() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let _policy = TestPolicyOverride::set(Some(policy("")));
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    for path in [
        root.path().join("image.png"),
        outside.path().join("outside.txt"),
    ] {
        std::fs::write(&path, "withheld fixture").unwrap();
        let (output, outcome) = process(root.path(), &path);
        assert_eq!(outcome, ShellOutcome::Blocked, "{output}");
        assert!(!output.contains("withheld fixture"));
    }
}

#[test]
fn summary_handles_empty_unicode_and_large_lines_with_bounded_previews() {
    assert!(
        summarize_file_content("", None).starts_with("Admitted text: 0 lines, 0 bytes, 0 words")
    );
    let text = format!("{}\n", "🙂".repeat(20_000)).repeat(12);
    let output = summarize_file_content(&text, None);
    assert!(output.starts_with("Admitted text: 12 lines, 960012 bytes, 12 words"));
    assert!(output.contains("6 middle lines omitted"));
    assert!(output.len() < 25_000);
    assert_eq!(output.matches("[line truncated]").count(), 6);
    assert_eq!(output.matches('🙂').count(), 6 * 1024);
    let output = summarize_file_content("one\n", Some("inspect\nAdmitted text: forged"));
    assert_eq!(output.matches("Admitted text:").count(), 1);
    assert!(output.contains("Intent: inspectAdmitted text forged\n"));
}

/// The file handler publishes an archive binding only for a successful,
/// verified read; a blocked source publishes none.
#[tokio::test]
async fn file_handler_publishes_only_a_successful_verified_binding() {
    use lean_ctx::core::archive::authority;
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _policy = TestPolicyOverride::set(Some(policy(
        "[filters]\nclassification='block'\nblocked_labels=['CONFIDENTIAL']\n",
    )));
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    std::fs::write(&path, "public source").unwrap();
    let ((_, outcome), binding) = authority::capture(async {
        handle_file(path.to_str().unwrap(), None, root.path().to_str())
    })
    .await;
    assert_eq!(outcome, ShellOutcome::Exit(0));
    assert!(binding.is_some());
    std::fs::write(&path, "CONFIDENTIAL\nsource").unwrap();
    let ((_, outcome), binding) = authority::capture(async {
        handle_file(path.to_str().unwrap(), None, root.path().to_str())
    })
    .await;
    assert_eq!(outcome, ShellOutcome::Blocked);
    assert!(binding.is_none());
}

/// 24.10.: an agent copied a redaction marker out of its redacted view into
/// a command. `ctx_execute` refuses such code in every language, single and
/// batched (the shell allowlist side is tested in `core::shell_allowlist`).
#[test]
fn ctx_execute_refuses_a_marker_in_every_language() {
    let code = "print('postgres://[REDACTED:Database credentials]@db/hive')";
    let (text, outcome) = super::handle_in("python", code, None, None, None);
    assert!(matches!(outcome, ShellOutcome::Blocked), "{text}");
    let batch = [("python".to_string(), code.to_string())];
    let (text, outcome) = super::handle_batch(&batch);
    assert!(matches!(outcome, ShellOutcome::Blocked), "{text}");
}
