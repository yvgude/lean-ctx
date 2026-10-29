use crate::core::workflow::types::WorkflowRun;
use std::path::{Path, PathBuf};

/// Stale threshold: workflows inactive for over 30 minutes are auto-cleared on load.
const STALE_MINUTES: i64 = 30;

/// TTL for expired workflow files (24 hours).
pub const WORKFLOW_TTL_SECS: u64 = 24 * 60 * 60;

fn workflows_dir() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("workflows"))
}

fn workflow_path_for_agent(agent_id: Option<&str>) -> Option<PathBuf> {
    let dir = workflows_dir()?;
    let filename = match agent_id {
        Some(id) if !id.trim().is_empty() => {
            let safe_id: String = id
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("workflow-{safe_id}.json")
        }
        _ => "active.json".to_string(),
    };
    Some(dir.join(filename))
}

pub fn load_active() -> Result<Option<WorkflowRun>, String> {
    load_active_for_agent(None)
}

pub fn load_active_for_agent(agent_id: Option<&str>) -> Result<Option<WorkflowRun>, String> {
    let Some(path) = workflow_path_for_agent(agent_id) else {
        return Ok(None);
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Backward compat: if agent-scoped file missing, try legacy active.json (read-only migration)
            if agent_id.is_some()
                && let Some(legacy) = workflow_path_for_agent(None)
                && let Ok(lc) = std::fs::read_to_string(&legacy)
            {
                let run: WorkflowRun = serde_json::from_str(&lc)
                    .map_err(|e| format!("Invalid legacy workflow JSON: {e}"))?;
                // Withheld first: the age of a workflow that may not be read
                // never turns it into "no workflow".
                return Ok(match serve_run(&legacy, &lc) {
                    Err(reason) => Some(withheld_sentinel(reason)),
                    Ok(()) if is_run_stale(&run) || run.current == "done" => None,
                    Ok(()) => Some(run),
                });
            }
            return Ok(None);
        }
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let run: WorkflowRun =
        serde_json::from_str(&content).map_err(|e| format!("Invalid workflow JSON: {e}"))?;

    // Withheld first. A workflow that exists but may not be read loads CLOSED
    // and its file is kept, however old it is: the stale rule below is a rule
    // for runs that were served, and it must not delete a workflow because
    // the reader has no right to it at this moment.
    if let Err(reason) = serve_run(&path, &content) {
        return Ok(Some(withheld_sentinel(reason)));
    }
    if is_run_stale(&run) || run.current == "done" {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(authority_path(&path));
        return Ok(None);
    }
    Ok(Some(run))
}

/// Whether `run` has been inactive for longer than the stale threshold. The
/// one stale rule, for the store and for both tool gates
/// (`workflow::gate`). The stand-in for a withheld workflow is never stale:
/// it ends when the stored workflow loads again or is stopped explicitly,
/// not by the passing of time.
#[must_use]
pub fn is_run_stale(run: &WorkflowRun) -> bool {
    !is_withheld_sentinel(run)
        && chrono::Utc::now()
            .signed_duration_since(run.updated_at)
            .num_minutes()
            > STALE_MINUTES
}

/// The binding of a stored workflow lives next to it, so the workflow file
/// and `WorkflowRun` keep their shape. It does not end in `.json`.
fn authority_path(workflow: &Path) -> PathBuf {
    workflow.with_extension("authority")
}

/// The text of a stored workflow as admission inspects it: every string
/// value, decoded, one per line (names, descriptions, notes, evidence).
fn run_text(json: &str) -> Result<String, String> {
    fn collect<'a>(value: &'a serde_json::Value, leaves: &mut Vec<&'a str>) {
        match value {
            serde_json::Value::String(text) if !text.is_empty() => leaves.push(text),
            serde_json::Value::Array(items) => items.iter().for_each(|item| collect(item, leaves)),
            serde_json::Value::Object(map) => map.values().for_each(|child| collect(child, leaves)),
            _ => {}
        }
    }
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("Invalid workflow JSON: {e}"))?;
    let mut leaves = Vec::new();
    collect(&value, &mut leaves);
    Ok(leaves.join("\n"))
}

fn request_project() -> Option<String> {
    crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .and_then(|root| root.to_str().map(str::to_owned))
}

const WITHHELD_SUBJECT: &str = "stored workflow";

/// A stored workflow is caller text: it is served as a new delivery under the
/// CURRENT policy — bound to the project it was saved in (a binding without
/// sources: there is no file to re-read), then the content rules. A workflow
/// file without a binding follows the originless rule. Its names are keys of
/// the state machine, so a text the current rules would rewrite is refused
/// rather than changed.
fn serve_run(path: &Path, json: &str) -> Result<(), String> {
    use crate::core::context_admission::carrier::Withheld;
    use crate::core::context_admission::stored::{StoredBinding, serve_stored};
    let binding: Option<StoredBinding> = std::fs::read_to_string(authority_path(path))
        .ok()
        .and_then(|authority| serde_json::from_str(&authority).ok());
    let text = run_text(json)?;
    let served = serve_stored(&text, binding.as_ref(), request_project().as_deref())
        .map_err(|withheld| withheld.message(WITHHELD_SUBJECT))?;
    if served == text {
        Ok(())
    } else {
        Err(Withheld::new(&["policy.content_redacted"]).message(WITHHELD_SUBJECT))
    }
}

/// Name and only state of the run that stands in for a withheld workflow.
const CLOSED: &str = "gateway-withheld";

/// The CLOSED run that stands in for a stored workflow that may not be served
/// now; `reason` is the typed, content-free refusal of `serve_run`.
///
/// A workflow gates tools per state, and "no workflow" means no gate
/// (`workflow::gate::tool_gate`). A workflow that exists but may not be read
/// must therefore never load as "none": it loads as this run, for which the
/// gate allows the workflow control tool alone, with the reason as its
/// description. The stored file is kept; it loads again once it is admitted
/// (same project, rules that admit it). Explicit ends, through
/// `ctx_workflow`: `stop` removes the stored workflow, `start` replaces it.
/// The stand-in never goes stale ([`is_run_stale`]) and is never saved.
fn withheld_sentinel(reason: String) -> WorkflowRun {
    use crate::core::workflow::types::{StateSpec, WorkflowSpec};
    let now = chrono::Utc::now();
    WorkflowRun {
        spec: WorkflowSpec {
            name: CLOSED.to_string(),
            description: Some(reason.clone()),
            initial: CLOSED.to_string(),
            states: vec![StateSpec {
                name: CLOSED.to_string(),
                description: Some(reason),
                allowed_tools: Some(vec!["ctx".to_string(), "ctx_workflow".to_string()]),
                requires_evidence: None,
            }],
            transitions: Vec::new(),
        },
        current: CLOSED.to_string(),
        started_at: now,
        updated_at: now,
        transitions: Vec::new(),
        evidence: Vec::new(),
    }
}

fn is_closed(run: &WorkflowRun) -> bool {
    run.spec.name == CLOSED && run.current == CLOSED
}

/// Whether `run` is the stand-in `withheld_sentinel` builds. The one test
/// for it: the store, the stale rule and the tool gate all ask here. The
/// marker is the reserved name pair, which no stored workflow can carry —
/// [`save_active_for_agent`] refuses to write a run that has it.
#[must_use]
pub fn is_withheld_sentinel(run: &WorkflowRun) -> bool {
    is_closed(run)
}

pub fn save_active(run: &WorkflowRun) -> Result<(), String> {
    save_active_for_agent(run, None)
}

pub fn save_active_for_agent(run: &WorkflowRun, agent_id: Option<&str>) -> Result<(), String> {
    let Some(path) = workflow_path_for_agent(agent_id) else {
        return Err("No home directory available".to_string());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    // The stand-in for a withheld workflow is never written over the
    // workflow it stands for.
    if is_closed(run) {
        return Err(run.spec.description.clone().unwrap_or_default());
    }
    let json = serde_json::to_string_pretty(run).map_err(|e| format!("serialize failed: {e}"))?;
    // Admission precedes persistence. Restricted text is never stored (E3),
    // and text the rules would rewrite is refused: names are keys.
    let binding = admit_run(&run_text(&json)?)?;
    let authority =
        serde_json::to_string(&binding).map_err(|e| format!("serialize failed: {e}"))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write failed: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename failed: {e}"))?;
    if let Err(error) =
        crate::config_io::write_atomic_with_backup(&authority_path(&path), &authority)
    {
        // Without its binding the workflow would load closed under
        // protection and unbound otherwise: do not leave it behind.
        let _ = std::fs::remove_file(&path);
        return Err(format!("write failed: {error}"));
    }
    Ok(())
}

/// Admit the text of a workflow for the store and bind it to the project of
/// the current request as caller-supplied text.
fn admit_run(text: &str) -> Result<crate::core::context_admission::stored::StoredBinding, String> {
    use crate::core::context_admission::carrier::Withheld;
    use crate::core::context_admission::stored::{Lineage, admit_and_bind};
    let refused = |withheld: Withheld| withheld.message(WITHHELD_SUBJECT);
    let (admitted, binding) = admit_and_bind(
        text,
        "workflow",
        request_project().as_deref(),
        Lineage::CallerSupplied,
    )
    .map_err(refused)?;
    if admitted != text {
        return Err(refused(Withheld::new(&["policy.content_redacted"])));
    }
    Ok(binding)
}

pub fn clear_active() -> Result<(), String> {
    clear_active_for_agent(None)
}

pub fn clear_active_for_agent(agent_id: Option<&str>) -> Result<(), String> {
    let Some(path) = workflow_path_for_agent(agent_id) else {
        return Ok(());
    };
    let _ = std::fs::remove_file(authority_path(&path));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove {}: {e}", path.display())),
    }
}

/// Remove workflow files older than `WORKFLOW_TTL_SECS`.
pub fn cleanup_expired() -> (u32, u64) {
    let Some(dir) = workflows_dir() else {
        return (0, 0);
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (0, 0);
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0u32;
    let mut freed = 0u64;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("json") {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .map_or(0, |d| d.as_secs());
        if age > WORKFLOW_TTL_SECS {
            freed += meta.len();
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(authority_path(&path));
            removed += 1;
        }
    }
    (removed, freed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_admission::stored::lineage_support::{governed, project, scoped};
    use crate::core::policy::runtime::TestPolicyOverride;
    use crate::core::workflow::types::{StateSpec, WorkflowSpec};

    fn review(description: &str) -> WorkflowRun {
        let now = chrono::Utc::now();
        WorkflowRun {
            spec: WorkflowSpec {
                name: "review".to_string(),
                description: Some(description.to_string()),
                initial: "planning".to_string(),
                states: vec![StateSpec {
                    name: "planning".to_string(),
                    description: None,
                    allowed_tools: Some(vec!["ctx_read".to_string()]),
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

    fn allowed(run: &WorkflowRun) -> Vec<String> {
        run.spec
            .state(&run.current)
            .and_then(|state| state.allowed_tools.clone())
            .expect("the current state gates tools")
    }

    /// Under protection a workflow saved in a project loads for that project.
    /// Whenever it may not be read — another project, no binding — it loads
    /// CLOSED: never as "no workflow" (which gates nothing), never wider than
    /// its own state, with a typed reason and the stored file kept.
    #[test]
    fn a_withheld_workflow_loads_closed_and_never_as_absent() {
        let _data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let (_other_directory, other) = project();
        let saved = review("Check the parser change");
        scoped(&root, || save_active(&saved)).expect("saved in its project");
        let path = workflow_path_for_agent(None).expect("path");

        let loaded = scoped(&root, load_active).expect("load").expect("active");
        assert_eq!(loaded.spec.name, "review");
        assert_eq!(allowed(&loaded), ["ctx_read"]);

        let closed = scoped(&other, load_active)
            .expect("a refusal is not an error that reads as absent")
            .expect("a withheld workflow is still a workflow");
        assert!(is_closed(&closed));
        assert_eq!(allowed(&closed), ["ctx", "ctx_workflow"]);
        let reason = closed.spec.description.clone().expect("typed reason");
        assert!(reason.contains("store.project_mismatch"), "{reason}");
        assert!(!reason.contains("parser"), "the reason is content-free");
        // The stand-in is never written over the workflow it stands for.
        assert!(scoped(&other, || save_active(&closed)).is_err());
        let again = scoped(&root, load_active).expect("load").expect("active");
        assert_eq!(again.spec.name, "review", "the stored workflow is kept");

        // No request project, and a file without its binding: closed as well.
        assert!(is_closed(&load_active().expect("load").expect("active")));
        std::fs::remove_file(authority_path(&path)).expect("binding");
        let unbound = scoped(&root, load_active).expect("load").expect("active");
        assert!(is_closed(&unbound));
        let reason = unbound.spec.description.expect("typed reason");
        assert!(reason.contains("store.no_source_authority"), "{reason}");

        // Restricted text is never stored, and a refused save writes nothing.
        clear_active().expect("clear");
        let marked = review("*** TOP SECRET ***\nrotate the signing key");
        assert!(scoped(&root, || save_active(&marked)).is_err());
        assert!(!path.exists());
        assert!(scoped(&root, load_active).expect("load").is_none());
    }

    /// Without protection the behaviour is as before: a workflow is saved and
    /// loaded without a project, and a file without a binding still loads.
    #[test]
    fn without_protection_a_workflow_loads_as_before() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = TestPolicyOverride::set(None);
        save_active(&review("Check the parser change")).expect("saved");
        let path = workflow_path_for_agent(None).expect("path");
        assert!(authority_path(&path).exists());
        assert_eq!(
            load_active().expect("load").expect("active").spec.name,
            "review"
        );
        std::fs::remove_file(authority_path(&path)).expect("binding");
        let legacy = load_active().expect("load").expect("active");
        assert_eq!(legacy.spec.name, "review");
        assert_eq!(allowed(&legacy), ["ctx_read"]);
    }
}
