// SPDX-License-Identifier: Apache-2.0
//! GH #2005: `ctx_expand(id=<jobId>)` without a prior status poll.

use super::*;

fn background_args(command: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut args = serde_json::Map::new();
    args.insert("command".to_string(), serde_json::json!(command));
    args.insert("run_in_background".to_string(), serde_json::json!(true));
    args
}

#[tokio::test(flavor = "multi_thread")]
async fn finished_job_expands_before_any_status_poll() {
    let _data_dir = crate::core::data_dir::isolated_data_dir();
    // The authorised path: no policy, so a parallel test's scoped policy
    // cannot withhold this job's output.
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
    let result = call_shell(
        &background_args("printf GH2005_UNPOLLED_OUTPUT"),
        &shell_context(),
    );
    let job = launched_job_guard(&result);
    wait_for_completed(&job.job_id);

    let expanded = crate::tools::ctx_expand::handle(&serde_json::json!({"id": &job.job_id}));
    assert!(
        expanded.contains("GH2005_UNPOLLED_OUTPUT"),
        "a finished job must expand without a status poll: {expanded}"
    );

    // The later status poll reports the archive the expand created.
    let terminal = pipeline_background_status(&job.job_id, false, false, false).await;
    let archive_id = structured_of(&terminal)["archiveId"]
        .as_str()
        .expect("finished output must be archived")
        .to_string();
    assert!(
        expanded.contains(&format!("Archive {archive_id}")),
        "{expanded}"
    );
}

/// The expand takes the status path's checks: output the job record holds back
/// is refused with that reason — not reported as a job that printed nothing —
/// and nothing is archived for it.
#[tokio::test(flavor = "multi_thread")]
async fn withheld_job_output_is_refused_and_not_archived() {
    use crate::server::background_shell::{self, JobState};
    let _data_dir = crate::core::data_dir::isolated_data_dir();
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
    let result = call_shell(
        &background_args("printf GH2005_WITHHELD_OUTPUT"),
        &shell_context(),
    );
    let job = launched_job_guard(&result);
    wait_for_completed(&job.job_id);

    // What a storage refusal leaves behind: the retained text is gone and the
    // record says it was withheld.
    let snapshot = background_shell::snapshot(&job.job_id).expect("finished job");
    let (JobState::Completed { output, .. } | JobState::Cancelled { output }) = &snapshot.state
    else {
        panic!("job must be finished: {:?}", snapshot.state);
    };
    assert!(background_shell::replace_retained_output_if_generation(
        &job.job_id,
        snapshot.generation,
        output,
        crate::core::context_admission::stores::current_epoch(),
        None,
    ));

    let refused = crate::tools::ctx_expand::handle(&serde_json::json!({"id": &job.job_id}));
    assert!(
        refused.contains("retained output withheld by storage policy"),
        "{refused}"
    );
    assert!(!refused.contains("without output"), "{refused}");
    assert!(!refused.contains("GH2005_WITHHELD_OUTPUT"), "{refused}");
    assert_eq!(crate::core::archive::resolve_alias(&job.job_id), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn running_and_unknown_jobs_are_named_not_called_expired() {
    let _data_dir = crate::core::data_dir::isolated_data_dir();
    let _archive = ScopedEnvVar::set("LEAN_CTX_ARCHIVE", "1");
    let result = call_shell(
        &background_args("sleep 5; printf GH2005_LATE"),
        &shell_context(),
    );
    let job = launched_job_guard(&result);

    let running = crate::tools::ctx_expand::handle(&serde_json::json!({"id": &job.job_id}));
    assert!(running.contains("is still running"), "{running}");
    assert!(!running.contains("expired"), "{running}");

    let unknown =
        crate::tools::ctx_expand::handle(&serde_json::json!({"id": "shell_00000000deadbeef"}));
    assert!(unknown.contains("does not exist"), "{unknown}");
}
