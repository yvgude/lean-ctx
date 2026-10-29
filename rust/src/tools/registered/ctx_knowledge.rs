use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_f64, get_str, get_str_array,
};
use crate::tool_defs::tool_def;

pub struct CtxKnowledgeTool;

impl McpTool for CtxKnowledgeTool {
    fn name(&self) -> &'static str {
        "ctx_knowledge"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_knowledge",
            "Persistent memory across sessions — remember decisions, patterns, and facts for recall.\n\
             WORKFLOW: save after completing significant tasks; recall at session start.\n\
             action=remember value='Y' saves a fact (key optional — derived from value; content= is an accepted alias).\n\
             action=recall query='X' retrieves it (bare recall lists recent facts). action=status shows all categories.\n\
action=consolidate imports latest session if present, runs lifecycle, then frees 25% facts/history/procedures capacity.\n\
             action=gotcha trigger='X' resolution='Y' for known pitfalls.\n\
             mode=semantic|exact for recall. category groups related facts.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "remember|recall|search|pattern|gotcha|relate|relations|consolidate|restore|status|timeline|rooms|wakeup|remove|export|import (also: feedback, unrelate, relations_diagram, health, lifecycle_report, policy, embeddings_*)"
                    },
                    "trigger": { "type": "string", "description": "gotcha trigger pattern" },
                    "resolution": { "type": "string", "description": "gotcha resolution/fix" },
                    "severity": { "type": "string", "description": "gotcha: critical|warning|info" },
                    "category": { "type": "string", "description": "Fact category" },
                    "key": { "type": "string" },
                    "value": { "type": "string", "description": "remember/pattern: the text. wakeup (optional): the memory hash a full briefing ended with, only while that briefing is still in your context — answered \"unchanged\" only if exactly that briefing was released to a request of the same host presenter capability; in every other case the full briefing is returned" },
                    "content": { "type": "string", "description": "Alias for value (remember)" },
                    "sources": { "type": "array", "items": { "type": "string" }, "description": "remember: project files the fact is derived from (read now and bound to it)" },
                    "anchors": { "type": "array", "items": { "type": "string" }, "description": "remember: code the fact is about, path or path::symbol (max 16); a later change there marks the fact to verify" },
                    "type": { "type": "string", "description": "remember: fact|decision|gotcha|procedure|finding (Hive capture)" },
                    "refs": { "type": "array", "items": { "type": "string" }, "description": "remember: references for Hive capture (max 10): file:<path>[#L<a>-<b>], sym:<path>#<symbol>, test:<path>::<name>, commit:<sha>, https://…" },
                    "evidence": { "type": "string", "description": "remember: observed|stated|imported" },
                    "steps": { "type": "array", "items": { "type": "string" }, "description": "remember type=procedure: steps (value = title)" },
                    "commands": { "type": "array", "items": { "type": "object", "properties": { "shell": { "type": "string" }, "cmd": { "type": "string" }, "cwd_hint": { "type": "string" } }, "required": ["shell", "cmd"] }, "description": "remember type=procedure: commands as data (never executed)" },
                    "validity": { "type": "string", "description": "remember: convention (project rule, shared with linked checkouts regardless of revision) | content (bound to its sources' revision). Default: convention for category convention/decision, content when sources bind" },
                    "query": { "type": "string", "description": "Query for recall/search/relate/restore" },
                    "mode": { "type": "string", "description": "auto|exact|semantic|hybrid" },
                    "as_of": { "type": "string", "description": "YYYY-MM-DD date filter" },
                    "pattern_type": { "type": "string" },
                    "examples": { "type": "array", "items": { "type": "string" } },
                    "confidence": { "type": "number", "description": "0.0-1.0" },
                    "store": { "type": "string", "description": "restore: facts|history|procedures|patterns (default: all)" },
                    "limit": { "type": "number", "description": "restore: max items to recover (default 50)" },
                    "dry_run": { "type": "boolean", "description": "consolidate: preview imports/reclaim without writing" },
                    "format": { "type": "string", "description": "export: json|okf (okf = portable Markdown bundle)" },
                    "path": { "type": "string", "description": "export/import: bundle directory (OKF) or file path" },
                    "merge": { "type": "string", "description": "import: replace|append|skip-existing (default skip-existing)" }
                },
                "allOf": [
                    {
                        "if": { "properties": { "action": { "const": "remember" } }, "required": ["action"] },
                        "then": { "required": ["category"], "anyOf": [{ "required": ["value"] }, { "required": ["content"] }] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "pattern" } }, "required": ["action"] },
                        "then": { "required": ["value"] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "search" } }, "required": ["action"] },
                        "then": { "required": ["query"] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "gotcha" } }, "required": ["action"] },
                        "then": { "required": ["trigger", "resolution"] }
                    }
                ],
                "required": ["action"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action")
            .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
        let category = get_str(args, "category");
        let key = get_str(args, "key");
        // `content` is the wording our own workflow docs use for remember;
        // accept it as an alias so agents following those docs succeed (#658).
        let value = get_str(args, "value").or_else(|| get_str(args, "content"));
        let query = get_str(args, "query");
        let mode = get_str(args, "mode");
        let as_of = get_str(args, "as_of");
        let pattern_type = get_str(args, "pattern_type");
        let examples = get_str_array(args, "examples");
        let confidence = get_f64(args, "confidence").map(|v| v as f32);
        let validity = match get_str(args, "validity") {
            None => None,
            Some(raw) => Some(
                crate::core::knowledge::FactValidity::parse(&raw).ok_or_else(|| {
                    ErrorData::invalid_params("validity must be convention or content", None)
                })?,
            ),
        };

        let session_handle = ctx
            .session
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
        let (session_id, project_root) = {
            let read_result =
                crate::server::bounded_lock::read(session_handle, "ctx_knowledge session read");
            if let Some(session) = read_result {
                let sid = session.id.clone();
                let root = session
                    .project_root
                    .clone()
                    .unwrap_or_else(|| ctx.project_root.clone());
                (sid, root)
            } else {
                tracing::warn!("ctx_knowledge: session read-lock timeout, using fallback");
                ("unknown".to_string(), ctx.project_root.clone())
            }
        };

        if action == "gotcha" {
            let trigger = get_str(args, "trigger").unwrap_or_default();
            let resolution = get_str(args, "resolution").unwrap_or_default();
            let severity = get_str(args, "severity").unwrap_or_default();
            let cat = category.as_deref().unwrap_or("convention");

            if trigger.is_empty() || resolution.is_empty() {
                return Ok(text_output(
                    &action,
                    "ERROR: trigger and resolution are required for gotcha action".to_string(),
                ));
            }

            let mut store = crate::core::gotcha_tracker::GotchaStore::load(&project_root);
            let msg = match store.report_gotcha(&trigger, &resolution, cat, &severity, &session_id)
            {
                Some(gotcha) => {
                    let conf = (gotcha.confidence * 100.0) as u32;
                    let label = gotcha.category.short_label();
                    format!("Gotcha recorded: [{label}] {trigger} (confidence: {conf}%)")
                }
                None => {
                    format!("Gotcha noted: {trigger} (evicted by higher-confidence entries)")
                }
            };
            let _ = store.save(&project_root);
            return Ok(text_output(&action, msg));
        }

        // Restore (#995 Phase 6): explicit cross-store undo from archive. Handled
        // inline (like `gotcha`) so `store`/`limit` can be passed without widening
        // the shared `handle()` signature.
        if action == "restore" {
            let store = match get_str(args, "store").as_deref() {
                Some(s) => match crate::core::memory_archive::MemoryStore::parse(s) {
                    Some(ms) => Some(ms),
                    None => {
                        return Ok(text_output(
                            &action,
                            format!(
                                "Unknown store: {s}. Use: facts, history, procedures, patterns"
                            ),
                        ));
                    }
                },
                None => None,
            };
            let limit = get_f64(args, "limit")
                .map_or(crate::tools::ctx_knowledge::DEFAULT_RESTORE_LIMIT, |v| {
                    v as usize
                });
            let opts =
                crate::tools::ctx_knowledge::RestoreOptions::new(store, query.clone(), limit);
            let text = match crate::tools::ctx_knowledge::run_restore(&project_root, &opts) {
                Ok(report) => crate::tools::ctx_knowledge::format_restore_report(&report),
                Err(e) => e,
            };
            return Ok(text_output(&action, text));
        }

        // Dry-run consolidate: preview imports + reclaim with no writes.
        let dry_run = args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if action == "consolidate" && dry_run {
            let text = match crate::tools::ctx_knowledge::consolidate_project_knowledge_with(
                &project_root,
                &crate::core::consolidation_engine::ConsolidateOptions::manual().into_dry_run(),
            ) {
                Ok(report) => crate::tools::ctx_knowledge::format_consolidation_report(&report),
                Err(e) => e,
            };
            return Ok(text_output(&action, text));
        }

        // OKF export/import handled inline so `format`/`path`/`merge` stay off the
        // shared handle() signature (same pattern as restore/gotcha above).
        if action == "export" && get_str(args, "format").as_deref() == Some("okf") {
            let out = match get_str(args, "path").or_else(|| get_str(args, "output")) {
                Some(p) => match jailed_path("ctx_knowledge.export", &p, &project_root) {
                    // Under a Workspace the authority confines it as well.
                    Ok(p) => {
                        match crate::core::workspace_authority::confine(&p, &project_root, true) {
                            Ok(p) => Some(p),
                            Err(error) => {
                                return Ok(text_output(&action, format!("ERROR: {error}")));
                            }
                        }
                    }
                    Err(e) => return Ok(text_output(&action, e)),
                },
                None => None,
            };
            return Ok(text_output(
                &action,
                crate::tools::ctx_knowledge::handle_export_okf(&project_root, out.as_deref()),
            ));
        }
        if action == "import" {
            let Some(path) = get_str(args, "path").or_else(|| query.clone()) else {
                return Ok(text_output(
                    &action,
                    "ERROR: import requires `path` (a file or an OKF directory)".to_string(),
                ));
            };
            let path =
                match jailed_path("ctx_knowledge.import", &path, &project_root).and_then(|p| {
                    // Under a Workspace the authority confines it as well.
                    crate::core::workspace_authority::confine(&p, &project_root, false)
                        .map_err(|error| format!("ERROR: {error}"))
                }) {
                    Ok(p) => p,
                    Err(e) => return Ok(text_output(&action, e)),
                };
            let merge = get_str(args, "merge")
                .and_then(|s| crate::core::knowledge::ImportMerge::parse(&s))
                .unwrap_or(crate::core::knowledge::ImportMerge::SkipExisting);
            return Ok(text_output(
                &action,
                crate::tools::ctx_knowledge::handle_import(
                    &project_root,
                    &path,
                    merge,
                    &session_id,
                ),
            ));
        }

        // A typed failure (every failure of the declaring actions `recall`,
        // `timeline`, `search`, and the refusals all actions share) is a tool
        // error with the same text — never a reply that could be recorded as
        // a delivery of the sources captured before it failed.
        let outcome = crate::tools::ctx_knowledge::handle_outcome(
            &project_root,
            &action,
            category.as_deref(),
            key.as_deref(),
            value.as_deref(),
            query.as_deref(),
            &session_id,
            pattern_type.as_deref(),
            examples,
            confidence,
            mode.as_deref(),
            as_of.as_deref(),
        );
        let failed = outcome.is_err();
        let (Ok(result) | Err(result)) = outcome;
        let named = get_str_array(args, "sources").unwrap_or_default();
        let result = if action == "remember" && !failed {
            remembered_with(
                result,
                &project_root,
                category.as_deref(),
                key.as_deref(),
                value.as_deref(),
                &named,
                validity,
            )
        } else {
            result
        };
        // J2: anchors to the code the fact is about, all or none.
        let anchors = get_str_array(args, "anchors").unwrap_or_default();
        let result = match (
            action == "remember" && !failed && !anchors.is_empty(),
            category.as_deref(),
            value.as_deref(),
        ) {
            (true, Some(category), Some(value)) => {
                match crate::tools::ctx_knowledge::anchor_remembered(
                    &project_root,
                    category,
                    key.as_deref(),
                    value,
                    &anchors,
                ) {
                    Ok(set) => format!("{result}\nanchors: {} recorded", set.len()),
                    Err(reason) => {
                        format!("{result}\nanchors: not recorded ({reason}); the fact has none")
                    }
                }
            }
            _ => result,
        };
        // Hive capture fields (v4 V1), all or none.
        let result = match (
            action == "remember" && !failed,
            category.as_deref(),
            value.as_deref(),
        ) {
            (true, Some(category), Some(value)) => match capture_fields(args) {
                Ok(None) => result,
                Ok(Some(fields)) => match crate::tools::ctx_knowledge::capture_remembered(
                    &project_root,
                    category,
                    key.as_deref(),
                    value,
                    fields,
                ) {
                    Ok(()) => format!("{result}\ncapture: recorded"),
                    Err(reason) => format!("{result}\ncapture: not recorded ({reason})"),
                },
                Err(reason) => format!("{result}\ncapture: not recorded ({reason})"),
            },
            _ => result,
        };

        if failed {
            return Err(crate::server::tool_trait::tool_execution_error(result));
        }
        Ok(text_output(&action, result))
    }

    /// Declared for `recall`, `timeline` and `search`. Their replies are
    /// fixed text, the request's own arguments, and stored facts with their
    /// identity and counters; every fact passes
    /// `ctx_knowledge::fact_served`, which publishes the files of a fact
    /// bound to sources and marks the capture incomplete for a fact without
    /// a binding. A judgement line under a recalled fact marks it too.
    /// `search` adds session entries only as the originless rule serves
    /// them, which is never while protection is active.
    ///
    /// Not declared: `remember` (echoes the value and previews similar
    /// facts), `status`, `wakeup` and `export` (rendered by the knowledge
    /// store, which consults no binding), `relations*`, `restore`,
    /// `consolidate*`, `gotcha`, `pattern`, the bridge and embeddings
    /// actions (other stores or no fact value), and the rest (no stored
    /// fact in the reply).
    fn publishes_every_source(&self, args: Option<&Map<String, Value>>) -> bool {
        matches!(
            args.and_then(|args| get_str(args, "action")).as_deref(),
            Some("recall" | "timeline" | "search")
        )
    }
}

/// The capture fields a `remember` names; `None` when it names none.
fn capture_fields(
    args: &Map<String, Value>,
) -> Result<Option<crate::core::knowledge::FactCapture>, String> {
    use crate::core::knowledge::{EvidenceLevel, FactCapture, NodeType, ProcedureCommand};
    let node_type = get_str(args, "type")
        .map(|t| NodeType::parse(&t).ok_or_else(|| format!("type {t:?}: not a node type")))
        .transpose()?;
    let evidence = get_str(args, "evidence")
        .map(|e| EvidenceLevel::parse(&e).ok_or_else(|| format!("evidence {e:?}: not a level")))
        .transpose()?;
    let commands: Vec<ProcedureCommand> = match args.get("commands") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("commands: {e}"))?,
        None => Vec::new(),
    };
    let fields = FactCapture {
        node_type,
        steps: get_str_array(args, "steps").unwrap_or_default(),
        commands,
        refs: get_str_array(args, "refs").unwrap_or_default(),
        evidence,
        changed_at: None,
    };
    Ok((fields != FactCapture::default()).then_some(fields))
}

const SOURCES_BOUND: &str = "sources: bound to the fact";
const SOURCES_NOT_BOUND: &str = "sources: not bound (a named source is not an admitted project file, or the fact was not stored as given); the fact has no source binding";
const SOURCES_NOT_BOUND_CONVENTION: &str =
    "sources: not bound (a convention is not bound to a revision)";

/// The `remember` reply with what this call recorded beyond the fact: the
/// source binding (`sources`) and the validity class (AP01 §4). An explicit
/// `validity` wins; a fact that names sources is `content`. A
/// convention is never bound to sources — it is shared regardless of
/// revision, so a revision must not decide whether it is served.
fn remembered_with(
    reply: String,
    project_root: &str,
    category: Option<&str>,
    key: Option<&str>,
    value: Option<&str>,
    named: &[String],
    validity: Option<crate::core::knowledge::FactValidity>,
) -> String {
    use crate::core::knowledge::FactValidity;
    let mut reply = reply;
    if !named.is_empty() {
        let line = if validity == Some(FactValidity::Convention) {
            SOURCES_NOT_BOUND_CONVENTION
        } else if bind_remembered(project_root, category, key, value, named) {
            SOURCES_BOUND
        } else {
            SOURCES_NOT_BOUND
        };
        reply = format!("{reply}\n{line}");
    }
    // A fact that names files is about code at a revision, bound or not: an
    // unbound one is then never served where its files cannot be checked.
    let class = validity.or((!named.is_empty()).then_some(FactValidity::Content));
    if let (Some(class), Some(category), Some(value)) = (class, category, value)
        && crate::tools::ctx_knowledge::classify_remembered(
            project_root,
            category,
            key,
            value,
            class,
        )
    {
        reply = format!("{reply}\nvalidity: {}", class.as_str());
    }
    reply
}

/// Bind the fact a `remember` has just stored to the `sources` its writer
/// named. Each is read now through the admitted reader; the fact is bound to
/// all of them or to none (`archive::bindings`). Bound is only the fact that
/// carries exactly the value of this call — never an older value under the
/// same key, and never a value the store merged, rewrote or rejected.
fn bind_remembered(
    project_root: &str,
    category: Option<&str>,
    key: Option<&str>,
    value: Option<&str>,
    sources: &[String],
) -> bool {
    use crate::core::archive::bindings::{self, Item};
    let (Some(category), Some(value)) = (category, value) else {
        return false;
    };
    let Some(knowledge) = crate::core::knowledge::ProjectKnowledge::load(project_root) else {
        return false;
    };
    let Some(fact) = knowledge.facts.iter().rev().find(|fact| {
        fact.is_current()
            && fact.category == category
            && fact.value == value
            && key.is_none_or(|key| fact.key == key)
    }) else {
        return false;
    };
    let Some(sources) = bindings::sources_at(project_root, sources, "ctx_knowledge") else {
        return false;
    };
    let item = Item::KnowledgeFact {
        project: project_root,
        category: &fact.category,
        key: &fact.key,
    };
    bindings::bind(&item, &fact.value, project_root, sources)
}

/// OKF export/import take a caller-supplied `path`. Like ctx_read and
/// ctx_handoff, keep it inside the project jail; a relative path is taken from
/// the project root.
fn jailed_path(tool: &str, raw: &str, project_root: &str) -> Result<String, String> {
    let root = std::path::Path::new(project_root);
    let candidate = std::path::Path::new(raw);
    let candidate = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    crate::core::io_boundary::jail_and_check_path(tool, &candidate, root)
        .map(|(p, _warning)| p.to_string_lossy().into_owned())
}

/// A plain text `ToolOutput` tagged with the action as its mode. `ctx_knowledge`
/// results are already compressed prose, so token accounting is left at zero.
fn text_output(action: &str, text: String) -> ToolOutput {
    ToolOutput {
        text,
        original_tokens: 0,
        saved_tokens: 0,
        mode: Some(action.to_string()),
        path: None,
        changed: false,
        shell_outcome: None,
        content_blocks: None,
    }
}

#[cfg(test)]
mod source_binding_tests {
    use super::*;
    use crate::core::archive::bindings::{self, Item};
    use crate::core::context_admission::stored::lineage_support::{
        deliver, governed, project, reusable,
    };

    fn remember(root: &str, key: &str, value: &str) {
        let reply = crate::tools::ctx_knowledge::handle(
            root,
            "remember",
            Some("architecture"),
            Some(key),
            Some(value),
            None,
            "session-1",
            None,
            None,
            None,
            None,
            None,
        );
        assert!(!reply.starts_with("ERROR"), "{reply}");
    }

    /// `remember` with a readable source binds the stored fact; with a source
    /// that is not admitted, or for a value other than the stored one, the
    /// fact stays stored and originless.
    #[test]
    fn remember_binds_the_stored_fact_to_the_sources_named_with_it() {
        let _data = governed();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        std::fs::write(format!("{root}/auth.rs"), "pub fn verify_rs256() {}\n").expect("source");
        std::fs::write(format!("{root}/plan.md"), "*** TOP SECRET ***\nlaunch\n").expect("marked");
        let value = "Tokens are verified with RS256 in the auth module";
        remember(&root, "token-check", value);
        let key = |sources: &[&str]| {
            let sources: Vec<String> = sources.iter().map(|path| (*path).to_owned()).collect();
            bind_remembered(
                &root,
                Some("architecture"),
                Some("token-check"),
                Some(value),
                &sources,
            )
        };
        assert!(!key(&["plan.md"]), "a restricted source binds nothing");
        assert!(!key(&["auth.rs", "missing.rs"]), "all sources or none");
        assert!(!bind_remembered(
            &root,
            Some("architecture"),
            Some("token-check"),
            Some("another value"),
            &["auth.rs".to_owned()],
        ));
        let item = Item::KnowledgeFact {
            project: &root,
            category: "architecture",
            key: "token-check",
        };
        let recall = || {
            deliver(&root, || {
                bindings::serve(&item, value, Some(&root)).expect("served");
                format!("[architecture/token-check]: {value}")
            })
        };
        assert_eq!(recall().1, 0, "stored, and originless so far");

        assert!(key(&["auth.rs"]));
        let (reply, recorded) = recall();
        assert_eq!(recorded, 1);
        assert_eq!(reusable(&root, &reply), Ok(()));
    }

    fn git(dir: &str, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    /// S05 (AP01 §4): two worktrees of one repository, linked to one project
    /// (D01). A convention is served in both regardless of branch. A fact
    /// bound to `retry.rs` is served in its own worktree, withheld as stale
    /// in the other while the file differs there on its branch, and served
    /// there once the file is identical. An unclassified fact stays in its
    /// own worktree, as before classes.
    #[test]
    fn linked_worktrees_share_conventions_and_withhold_stale_code() {
        use crate::core::project_link::{ProjectLink, link, link_time};
        let _data = governed();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_main_dir, main) = project();
        let (_other_dir, other_parent) = project();
        let feature = format!("{other_parent}/feature");
        std::fs::write(format!("{main}/retry.rs"), "fn retry() { /* bounded */ }\n").unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["add", "retry.rs"]);
        git(&main, &["commit", "-q", "-m", "retry"]);
        git(&main, &["worktree", "add", "-q", "-b", "feature", &feature]);
        let feature = crate::core::pathutil::safe_canonicalize(std::path::Path::new(&feature))
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        std::fs::write(format!("{feature}/retry.rs"), "fn retry() { loop {} }\n").unwrap();
        git(&feature, &["commit", "-q", "-am", "unbounded"]);
        for root in [&main, &feature] {
            link(ProjectLink {
                workspace_root: root.clone(),
                team_project_id: "gateway".into(),
                organization_id: None,
                label: None,
                linked_at: link_time(),
            })
            .unwrap();
        }

        let convention = "Branch names follow the pattern type slash slug";
        let code = "The retry loop in retry.rs is bounded by its budget";
        let local = "The release captain promotes deployments on Fridays";
        let remember_in = |category: &str, key: &str, value: &str| {
            let reply = crate::tools::ctx_knowledge::handle(
                &main,
                "remember",
                Some(category),
                Some(key),
                Some(value),
                None,
                "session-1",
                None,
                None,
                None,
                None,
                None,
            );
            assert!(reply.starts_with("Remembered"), "{reply}");
        };
        remember_in("decision", "branch-naming", convention);
        remember_in("architecture", "retry-bound", code);
        remember_in("architecture", "release-duty", local);
        let reply = remembered_with(
            String::new(),
            &main,
            Some("architecture"),
            Some("retry-bound"),
            Some(code),
            &["retry.rs".to_owned()],
            None,
        );
        assert!(reply.contains(SOURCES_BOUND), "{reply}");
        assert!(reply.contains("validity: content"), "{reply}");

        let recall = |root: &str| {
            crate::tools::ctx_knowledge::handle(
                root,
                "recall",
                None,
                None,
                None,
                None,
                "session-1",
                None,
                None,
                None,
                None,
                None,
            )
        };
        let own = recall(&main);
        assert!(own.contains(&format!("{convention} (confidence")), "{own}");
        assert!(own.contains("[convention]") && own.contains(code), "{own}");
        assert!(own.contains(local), "{own}");

        let linked = recall(&feature);
        assert!(
            linked.contains(&format!("{convention} [convention]")),
            "{linked}"
        );
        assert!(!linked.contains(code), "{linked}");
        assert!(
            linked.contains("1 code-bound fact(s) withheld: stale"),
            "{linked}"
        );
        assert!(!linked.contains(local), "{linked}");

        std::fs::write(
            format!("{feature}/retry.rs"),
            "fn retry() { /* bounded */ }\n",
        )
        .unwrap();
        let same = recall(&feature);
        assert!(same.contains(&format!("{code} [content]")), "{same}");
        assert!(!same.contains("withheld"), "{same}");
    }

    fn declares(action: &str) -> bool {
        let mut args = Map::new();
        args.insert("action".to_owned(), Value::String(action.to_owned()));
        CtxKnowledgeTool.publishes_every_source(Some(&args))
    }

    /// The recall of the knowledge tool itself, as a declared action
    /// delivers it.
    fn recall(root: &str) -> (String, usize) {
        deliver(root, || {
            crate::tools::ctx_knowledge::handle(
                root,
                "recall",
                Some("architecture"),
                None,
                None,
                None,
                "session-1",
                None,
                None,
                None,
                None,
                None,
            )
        })
    }

    /// A fact remembered with sources and recalled by a declared action
    /// gives the reply the lineage of its files. A fact without sources in
    /// the same reply leaves it unknown. When a source is withheld by a
    /// tightened rule, or deleted, the lineage refuses and the bound fact is
    /// no longer recalled — the fact without sources still is.
    #[test]
    fn a_declared_recall_carries_the_lineage_of_its_bound_facts() {
        use crate::core::archive::delivered::LineageRefusal;
        let _data = governed();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_directory, root) = project();
        std::fs::write(format!("{root}/auth.rs"), "pub fn verify_rs256() {}\n").expect("source");
        let value = "Tokens are verified with RS256 in the auth module";
        remember(&root, "token-check", value);
        assert!(bind_remembered(
            &root,
            Some("architecture"),
            Some("token-check"),
            Some(value),
            &["auth.rs".to_owned()],
        ));
        assert!(declares("recall") && declares("timeline") && declares("search"));
        assert!(!declares("remember") && !declares("status") && !declares("wakeup"));

        let (reply, recorded) = recall(&root);
        assert!(reply.contains(value), "{reply}");
        assert_eq!(recorded, 1);
        assert_eq!(reusable(&root, &reply), Ok(()));

        let typed = "Deployments are promoted by the release captain on Fridays";
        remember(&root, "release-duty", typed);
        let (mixed, recorded) = recall(&root);
        assert!(mixed.contains(value) && mixed.contains(typed), "{mixed}");
        assert_eq!(recorded, 0, "one fact without a source: unknown");
        assert_eq!(reusable(&root, &mixed), Err(LineageRefusal::Unknown));

        // The source is reclassified: the recorded reply is refused and the
        // bound fact is left out of the next recall.
        std::fs::write(
            format!("{root}/auth.rs"),
            "*** TOP SECRET ***\npub fn verify_rs256() {}\n",
        )
        .expect("mark");
        assert_eq!(
            reusable(&root, &reply),
            Err(LineageRefusal::SourceNoLongerAdmitted)
        );
        let (withheld, recorded) = recall(&root);
        assert!(!withheld.contains(value), "{withheld}");
        assert!(withheld.contains(typed), "{withheld}");
        assert_eq!(recorded, 0);

        std::fs::remove_file(format!("{root}/auth.rs")).expect("delete");
        assert_eq!(
            reusable(&root, &reply),
            Err(LineageRefusal::SourceNoLongerAdmitted)
        );
        assert!(!recall(&root).0.contains(value));
    }
}

// The jail is the point of these tests; a `no-jail` build switches it off.
#[cfg(all(test, not(feature = "no-jail")))]
mod tests {
    use super::*;

    fn call(ctx: &ToolContext, args: &Value) -> String {
        CtxKnowledgeTool
            .handle(args.as_object().unwrap(), ctx)
            .unwrap()
            .text
    }

    #[test]
    fn okf_export_and_import_paths_stay_in_the_project_jail() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let ctx = ToolContext {
            project_root: root.path().to_string_lossy().into_owned(),
            session: Some(std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::core::session::SessionState::new(),
            ))),
            ..ToolContext::default()
        };
        call(
            &ctx,
            &json!({"action": "remember", "category": "architecture",
                    "key": "parser", "value": "the parser is bounded"}),
        );

        let export_dir = outside.path().join("okf");
        let text = call(
            &ctx,
            &json!({"action": "export", "format": "okf", "path": export_dir}),
        );
        assert!(!export_dir.exists(), "OKF export left the jail: {text}");

        let facts = outside.path().join("facts.json");
        std::fs::write(
            &facts,
            r#"[{"category":"secret","key":"outside","value":"OUTSIDE-JAIL"}]"#,
        )
        .unwrap();
        let text = call(&ctx, &json!({"action": "import", "path": facts}));
        assert!(
            !text.contains("Import complete"),
            "import read outside the jail: {text}"
        );

        let inside = root.path().join("okf-export");
        let text = call(
            &ctx,
            &json!({"action": "export", "format": "okf", "path": inside}),
        );
        assert!(inside.join("index.md").exists(), "{text}");
        let text = call(&ctx, &json!({"action": "import", "path": inside}));
        assert!(text.contains("import"), "{text}");
    }
}
