// SPDX-License-Identifier: Apache-2.0
//! The workflow tool gate: the one decision both call paths apply — the
//! direct call (`server/call_tool/policy.rs::guard_workflow`) and the inner
//! tool of `ctx_call` (`server/dispatch/mod.rs`). They pass the active run and
//! the tool name and act on the answer; neither decides anything itself.

use super::store::{is_run_stale, is_withheld_sentinel};
use super::types::WorkflowRun;

/// The workflow control tool. It is never gated: it is how a run is seen,
/// advanced and ended.
const CONTROL_TOOL: &str = "ctx_workflow";

/// The `ctx_call` wrapper. Under the stand-in for a withheld workflow the
/// wrapper name itself passes, because on a surface that exposes only the
/// wrapper it is the one way to reach [`CONTROL_TOOL`]. The tool it invokes
/// is gated by its own name at both call paths, so this opens nothing else.
const WRAPPER_TOOL: &str = "ctx_call";

/// What the gate answers for one tool call under an active run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolGate {
    /// The run is finished or stale: the caller clears it (memory and store)
    /// and the call proceeds ungated.
    Expired,
    /// The call proceeds.
    Pass,
    /// The call is refused with this message.
    Refuse(String),
}

/// Decide `tool` under `run`. `passthrough` is the server's list of tools a
/// real run never blocks.
///
/// - The stand-in for a withheld workflow is CLOSED: only the control tool
///   (and the wrapper that reaches it) passes. The pass-through list does not
///   apply, and the stand-in never expires — an expiry would clear it into
///   "no workflow", which gates nothing, and delete the stored workflow it
///   stands for. The refusal names no workflow, state or file.
/// - A real run: finished or stale → [`ToolGate::Expired`]; otherwise the
///   control tool and the pass-through tools pass, and any other tool must be
///   in the allowed list of the current state, when that state has one.
#[must_use]
pub fn tool_gate(run: &WorkflowRun, tool: &str, passthrough: &[&str]) -> ToolGate {
    if is_withheld_sentinel(run) {
        // `ctx` is the meta name; a call through it reaches the gate under
        // the name of the tool it resolves to, so it opens nothing either.
        return if matches!(tool, CONTROL_TOOL | WRAPPER_TOOL | "ctx") {
            ToolGate::Pass
        } else {
            ToolGate::Refuse(format!(
                "Tool '{tool}' blocked: the stored workflow is withheld by the context gateway. \
                 Use ctx_workflow(action=\"status\") to check it again, or \
                 ctx_workflow(action=\"stop\") to discard it."
            ))
        };
    }
    if tool == CONTROL_TOOL {
        return ToolGate::Pass;
    }
    if run.current == "done" || is_run_stale(run) {
        return ToolGate::Expired;
    }
    if passthrough.contains(&tool) {
        return ToolGate::Pass;
    }
    let Some(allowed) = run
        .spec
        .state(&run.current)
        .and_then(|state| state.allowed_tools.as_ref())
    else {
        return ToolGate::Pass;
    };
    if allowed.iter().any(|permitted| permitted == tool) {
        return ToolGate::Pass;
    }
    let mut shown = allowed.clone();
    shown.sort();
    shown.truncate(30);
    ToolGate::Refuse(format!(
        "Tool '{tool}' blocked by workflow '{}' (state: {}). Allowed: {}. Use ctx_workflow(action=\"stop\") to exit.",
        run.spec.name,
        run.current,
        shown.join(", ")
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{ToolGate, tool_gate};
    use crate::core::context_admission::stored::lineage_support::{governed, project, scoped};
    use crate::core::policy::runtime::TestPolicyOverride;
    use crate::core::workflow::store::{
        is_run_stale, is_withheld_sentinel, load_active, save_active,
    };
    use crate::core::workflow::types::{StateSpec, WorkflowRun, WorkflowSpec};

    /// The server's list, as `server::WORKFLOW_PASSTHROUGH_TOOLS` has it.
    const PASSTHROUGH: &[&str] = crate::server::WORKFLOW_PASSTHROUGH_TOOLS;

    /// A real run whose only state allows `ctx_overview`. Shared with the
    /// tests of the two server gates.
    pub(crate) fn review() -> WorkflowRun {
        let now = chrono::Utc::now();
        WorkflowRun {
            spec: WorkflowSpec {
                name: "review".to_string(),
                description: Some("Check the parser change".to_string()),
                initial: "planning".to_string(),
                states: vec![StateSpec {
                    name: "planning".to_string(),
                    description: None,
                    allowed_tools: Some(vec!["ctx_overview".to_string()]),
                    requires_evidence: None,
                }],
                transitions: Vec::new(),
            },
            current: "planning".to_string(),
            started_at: now,
            updated_at: now,
            transitions: Vec::new(),
            evidence: Vec::new(),
        }
    }

    /// Where the store keeps the active workflow.
    pub(crate) fn workflow_file() -> std::path::PathBuf {
        crate::core::data_dir::lean_ctx_data_dir()
            .expect("data dir")
            .join("workflows")
            .join("active.json")
    }

    /// The stand-in is closed and stays closed: neither the pass-through list
    /// nor its age opens it, the stored workflow it stands for is untouched,
    /// and it is gone once the workflow may be read again.
    #[test]
    fn the_withheld_stand_in_is_closed_and_never_expires() {
        let _data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let (_other_directory, other) = project();
        scoped(&root, || save_active(&review())).expect("saved in its project");
        let stored = std::fs::read(workflow_file()).expect("stored workflow");

        let mut closed = scoped(&other, load_active)
            .expect("load")
            .expect("a withheld workflow is still a workflow");
        assert!(is_withheld_sentinel(&closed));
        // Older than the stale threshold: still not stale, still closed.
        closed.updated_at = chrono::Utc::now() - chrono::Duration::minutes(45);
        assert!(!is_run_stale(&closed));
        for tool in [
            "ctx_read",
            "ctx_session",
            "ctx_ledger",
            "ctx_search",
            "ctx_overview",
        ] {
            let ToolGate::Refuse(reason) = tool_gate(&closed, tool, PASSTHROUGH) else {
                panic!("{tool} must be refused under the stand-in");
            };
            assert!(
                reason.contains("withheld by the context gateway"),
                "{reason}"
            );
            assert!(!reason.contains("review"), "no workflow name: {reason}");
            assert!(!reason.contains("planning"), "no state name: {reason}");
            assert!(!reason.contains("active.json"), "no file name: {reason}");
        }
        assert_eq!(
            tool_gate(&closed, "ctx_workflow", PASSTHROUGH),
            ToolGate::Pass
        );
        assert_eq!(tool_gate(&closed, "ctx_call", PASSTHROUGH), ToolGate::Pass);
        assert_eq!(
            std::fs::read(workflow_file()).expect("stored workflow"),
            stored,
            "the withheld workflow is byte-identical on disk"
        );

        // The authority holds again: the real workflow loads.
        let real = scoped(&root, load_active).expect("load").expect("active");
        assert!(!is_withheld_sentinel(&real));
        assert_eq!(real.spec.name, "review");
        assert_eq!(
            tool_gate(&real, "ctx_overview", PASSTHROUGH),
            ToolGate::Pass
        );
    }

    /// A withheld workflow whose own run is older than the stale threshold is
    /// kept and loads closed; once it may be read, the normal stale rule
    /// applies to it.
    #[test]
    fn a_withheld_workflow_is_not_deleted_for_its_age() {
        let _data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let (_other_directory, other) = project();
        let mut old = review();
        old.updated_at = chrono::Utc::now() - chrono::Duration::minutes(45);
        scoped(&root, || save_active(&old)).expect("saved in its project");
        let stored = std::fs::read(workflow_file()).expect("stored workflow");

        let closed = scoped(&other, load_active).expect("load").expect("closed");
        assert!(is_withheld_sentinel(&closed));
        assert_eq!(std::fs::read(workflow_file()).expect("kept"), stored);

        assert!(scoped(&root, load_active).expect("load").is_none());
        assert!(
            !workflow_file().exists(),
            "a served stale run is cleared as before"
        );
    }

    /// Control: a real run is gated exactly as before.
    #[test]
    fn a_real_run_goes_stale_and_keeps_its_pass_through_tools() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = TestPolicyOverride::set(None);
        let mut run = review();
        assert_eq!(tool_gate(&run, "ctx_read", PASSTHROUGH), ToolGate::Pass);
        assert_eq!(tool_gate(&run, "ctx_session", PASSTHROUGH), ToolGate::Pass);
        assert_eq!(tool_gate(&run, "ctx_overview", PASSTHROUGH), ToolGate::Pass);
        assert_eq!(tool_gate(&run, "ctx_workflow", PASSTHROUGH), ToolGate::Pass);
        assert_eq!(
            tool_gate(&run, "ctx_shell", PASSTHROUGH),
            ToolGate::Refuse(
                "Tool 'ctx_shell' blocked by workflow 'review' (state: planning). Allowed: ctx_overview. Use ctx_workflow(action=\"stop\") to exit."
                    .to_string()
            )
        );

        run.updated_at = chrono::Utc::now() - chrono::Duration::minutes(31);
        assert!(is_run_stale(&run));
        assert_eq!(tool_gate(&run, "ctx_shell", PASSTHROUGH), ToolGate::Expired);
        assert_eq!(tool_gate(&run, "ctx_read", PASSTHROUGH), ToolGate::Expired);
        assert_eq!(tool_gate(&run, "ctx_workflow", PASSTHROUGH), ToolGate::Pass);

        run.updated_at = chrono::Utc::now();
        run.current = "done".to_string();
        assert_eq!(tool_gate(&run, "ctx_shell", PASSTHROUGH), ToolGate::Expired);
    }
}
