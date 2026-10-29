// SPDX-License-Identifier: Apache-2.0
//! Compute jobs and results as people and agents read them: a fixed order,
//! no clock, text from the job fenced as foreign data (X5).

use std::fmt::Write as _;

use super::{Fetched, Submitted};
use crate::core::hive::untrusted::{UntrustedKind, wrap};
use lean_ctx_protocol::hive::compute::{ComputeCostV1, ComputeJobV1, JobState};

fn cost_line(cost: &ComputeCostV1, budget: u64) -> String {
    let mut line = format!(
        "cost: {} credits reserved (budget {budget})",
        cost.credits_reserved
    );
    if let (Some(spent), Some(refunded)) = (cost.credits_spent, cost.credits_refunded) {
        let _ = write!(line, ", {spent} spent, {refunded} refunded");
    }
    if let Some(wall) = cost.wall_seconds {
        let _ = write!(line, ", {wall} s");
    }
    line.push_str(" (1 credit = 1 vCPU-minute)");
    line
}

/// A service time (E48 `*_ms`) as an absolute UTC timestamp: the same
/// answer always reads the same, never relative to now.
fn utc(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map_or_else(
            || format!("{ms} ms"),
            |t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
}

/// The job's times and deadlines (two lines, each ending in a newline):
/// created/started/finished with the terminal reason, then the queue and
/// run deadlines and how long the artifacts are kept.
fn timeline(job: &ComputeJobV1) -> String {
    let mut out = format!("  timeline: created {}", utc(job.created_ms));
    if let Some(started) = job.started_ms {
        let _ = write!(out, ", started {}", utc(started));
    }
    if let Some(finished) = job.finished_ms {
        let _ = write!(out, ", finished {}", utc(finished));
        let _ = write!(out, " ({}", job.state.as_str());
        if let Some(reason) = job.reason {
            let _ = write!(out, ": {}", reason.as_str());
        }
        out.push(')');
    }
    let _ = write!(
        out,
        "\n  deadlines: start by {} (queue)",
        utc(job.queue_deadline_ms)
    );
    match job.run_deadline_ms {
        Some(run) => {
            let _ = write!(out, ", end by {} (run)", utc(run));
        }
        None if matches!(job.state, JobState::Queued) => {
            let _ = write!(out, ", then at most {} s of run", job.max_runtime_s);
        }
        None => {}
    }
    if let Some(expire) = job.artifacts_expire_ms {
        let _ = write!(out, "; artifacts kept until {}", utc(expire));
    }
    out.push('\n');
    out
}

/// One job: state, inputs, resources, cost, its timeline and deadlines,
/// and what to do next.
pub fn render_job(job: &ComputeJobV1) -> String {
    let mut out = format!(
        "Compute job {} ({}, image {}): {}",
        job.job_id,
        job.job_type.as_str(),
        job.image,
        job.state.as_str()
    );
    if let Some(reason) = job.reason {
        let _ = write!(out, " ({}: {})", reason.as_str(), reason.describe());
    }
    if job.cancel_requested && !job.state.is_terminal() {
        out.push_str(", cancel requested");
    }
    out.push('\n');
    for bundle in &job.inputs.bundles {
        let _ = writeln!(
            out,
            "  input: {} {} ({} bytes)",
            bundle.name, bundle.digest, bundle.size_bytes
        );
    }
    let network = if job.network_allowlist.is_empty() {
        "none".to_owned()
    } else {
        job.network_allowlist.join(", ")
    };
    let _ = writeln!(
        out,
        "  resources: {} vCPU, {} MB memory, {} MB disk, at most {} s, network: {network}",
        job.resources.cpu, job.resources.memory_mb, job.resources.disk_mb, job.max_runtime_s
    );
    let _ = writeln!(out, "  {}", cost_line(&job.cost, job.budget.credits));
    out.push_str(&timeline(job));
    let next = match job.state {
        JobState::Queued | JobState::Running => {
            format!("  next: status {id} | cancel {id}", id = job.job_id)
        }
        _ if job.result_available => format!("  next: result {}", job.job_id),
        _ => "  no result (it never ran, or its artifacts expired)".to_owned(),
    };
    out.push_str(&next);
    out
}

/// A submitted job and what its bundle left out.
pub fn render_submitted(submitted: &Submitted) -> String {
    let b = &submitted.bundle;
    let mut out = render_job(&submitted.job);
    let _ = write!(
        out,
        "\n  bundle: {} files, {} ({}), {}",
        b.report.files,
        b.reference.digest,
        b.reference.size_bytes,
        if b.uploaded {
            "uploaded"
        } else {
            "already on Hive"
        }
    );
    if !b.report.excluded_by_policy.is_empty() {
        let _ = write!(
            out,
            "\n  left out by the workspace policy: {}",
            listed(&b.report.excluded_by_policy)
        );
    }
    if !b.report.secret_paths.is_empty() {
        let names: Vec<String> = b
            .report
            .secret_paths
            .iter()
            .map(|(path, why)| format!("{path} ({why})"))
            .collect();
        let _ = write!(out, "\n  left out as secret-like: {}", listed(&names));
    }
    if b.report.skipped > 0 {
        let _ = write!(
            out,
            "\n  skipped: {} symlink(s) or non-UTF-8 path(s)",
            b.report.skipped
        );
    }
    out
}

fn listed(items: &[String]) -> String {
    const SHOWN: usize = 10;
    let mut text = items
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > SHOWN {
        let _ = write!(text, " and {} more", items.len() - SHOWN);
    }
    text
}

/// A result: state, outcome and counts, artifacts, the files written and
/// the fenced summary and log tail.
pub fn render_result(fetched: &Fetched) -> String {
    let r = &fetched.response;
    let source = format!("compute:{}", r.job_id);
    let mut out = format!("Compute job {}: {}", r.job_id, r.state.as_str());
    if let Some(reason) = r.reason {
        let _ = write!(out, " ({}: {})", reason.as_str(), reason.describe());
    }
    let budget = r.cost.credits_reserved;
    let _ = write!(out, "\n  {}", cost_line(&r.cost, budget));
    let Some(result) = &r.result else {
        out.push_str("\n  no result: the job never ran");
        return out;
    };
    let exit = result
        .exit_code
        .map_or_else(|| "none".to_owned(), |c| c.to_string());
    let _ = write!(
        out,
        "\n  outcome: {}, exit code {exit}",
        result.summary.outcome.as_str()
    );
    if !result.summary.counts.is_empty() {
        let counts: Vec<String> = result
            .summary
            .counts
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect();
        let _ = write!(out, " ({})", counts.join(", "));
    }
    for artifact in &result.artifacts {
        let _ = write!(
            out,
            "\n  artifact: {} {} ({} bytes)",
            artifact.name, artifact.digest, artifact.size_bytes
        );
    }
    if let Some(logs) = &result.logs {
        let _ = write!(
            out,
            "\n  logs: {} bytes{}, {} redaction(s)",
            logs.size_bytes,
            if logs.truncated {
                " (last 256 KiB)"
            } else {
                ""
            },
            logs.redactions
        );
    }
    if let Some(dir) = &fetched.out_dir {
        let _ = write!(
            out,
            "\n  written to {}: {} file(s)",
            dir.display(),
            fetched.written.len()
        );
    }
    if !result.summary.text.trim().is_empty() {
        let _ = write!(
            out,
            "\n  summary:\n{}",
            wrap(UntrustedKind::TaskUpdate, &source, &result.summary.text)
        );
    }
    if let Some(tail) = fetched.log_tail.as_deref().filter(|t| !t.trim().is_empty()) {
        let _ = write!(
            out,
            "\n  log tail:\n{}",
            wrap(UntrustedKind::TaskUpdate, &source, tail)
        );
    }
    out
}
