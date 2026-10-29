use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::core::handoff_ledger::{HandoffLedgerV1, LedgerLineage, LedgerPart};
use crate::core::session::SessionState;
use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_bool, get_str, get_str_array,
};
use crate::tool_defs::tool_def;

pub struct CtxHandoffTool;

impl McpTool for CtxHandoffTool {
    fn name(&self) -> &'static str {
        "ctx_handoff"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_handoff",
            "Context handoff protocol (hashed, deterministic, local-first).\n\
             Actions: create|show|list|pull|merge|clear|export|import. Stores curated file refs with hashes.\n\
             Before ending a session or handing off to another agent; merge combines ledgers of parallel agents.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "show", "list", "pull", "merge", "clear", "export", "import"],
                        "description": "create|show|list|pull|merge|clear|export|import"
                    },
                    "ledger": { "type": "string", "description": "Stored ledger to export (action=export), e.g. a merged one; default: a new ledger of this session" },
                    "ledgers": { "type": "array", "items": { "type": "string" }, "description": "Ledger file paths to merge (action=merge, 2-8)" },
                    "path": { "type": "string", "description": "Ledger file path (for show/pull/import)" },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "File paths for curated refs" },
                    "format": { "type": "string", "description": "json|summary|a2a" },
                    "write": { "type": "boolean", "description": "Write export to file" },
                    "privacy": { "type": "string", "description": "redacted|full (admin only)" },
                    "filename": { "type": "string", "description": "Custom filename for export" },
                    "apply_workflow": { "type": "boolean", "description": "Apply workflow state" },
                    "apply_session": { "type": "boolean", "description": "Apply session snapshot" },
                    "apply_knowledge": { "type": "boolean", "description": "Import knowledge facts" },
                    "request_id": { "type": "string", "maxLength": 128, "description": "create: idempotency key; a retry with the same key and paths returns the first ledger, other paths are refused" }
                },
                "allOf": [
                    {
                        "if": { "properties": { "action": { "const": "show" } }, "required": ["action"] },
                        "then": { "required": ["action", "path"] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "pull" } }, "required": ["action"] },
                        "then": { "required": ["action", "path"] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "import" } }, "required": ["action"] },
                        "then": { "required": ["action", "path"] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "merge" } }, "required": ["action"] },
                        "then": { "required": ["action", "ledgers"] }
                    }
                ]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action").unwrap_or_else(|| "list".to_string());
        let result = match action.as_str() {
            "list" => handle_list(&ctx.project_root),
            "clear" => handle_clear(&ctx.project_root),
            "show" => handle_show(args, ctx)?,
            "create" => handle_create(args, ctx)?,
            "export" => handle_export(args, ctx)?,
            "pull" => handle_pull(args, ctx)?,
            "import" => handle_import(args, ctx)?,
            "merge" => handle_merge(args, ctx)?,
            _ => "Unknown action. Use: create, show, list, pull, merge, clear, export, import"
                .to_string(),
        };

        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action),
            path: None,
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        })
    }

    /// Declared for `show` only: its reply is the ledger path and the ledger
    /// as `handoff_ledger::load_ledger` serves it, which publishes the joined
    /// files of a ledger made of curated excerpts and of findings and facts
    /// that were all bound at its creation, and marks the capture incomplete
    /// for every other ledger (an originless finding or fact, session text
    /// or a workflow has no source to re-check).
    ///
    /// Not declared: `create` (curated refs come from the read cache),
    /// `merge` (the reply carries digests and counts, never ledger text),
    /// `export`/`import` (bundle files, read outside the admitted readers),
    /// `pull` (applies the ledger; the reply carries counts), `list`, `clear`.
    fn publishes_every_source(&self, args: Option<&Map<String, Value>>) -> bool {
        args.and_then(|args| get_str(args, "action")).as_deref() == Some("show")
    }
}

fn handle_list(project_root: &str) -> String {
    let items = crate::core::handoff_ledger::list_ledgers(project_root);
    crate::tools::ctx_handoff::format_list(&items)
}

fn handle_clear(project_root: &str) -> String {
    let removed = crate::core::handoff_ledger::clear_ledgers(project_root).unwrap_or_default();
    crate::tools::ctx_handoff::format_clear(removed)
}

fn handle_show(args: &Map<String, Value>, ctx: &ToolContext) -> Result<String, ErrorData> {
    let path = get_str(args, "path")
        .ok_or_else(|| ErrorData::invalid_params("path is required for action=show", None))?;
    let path = ctx
        .resolve_path_sync(&path)
        .map_err(|e| ErrorData::invalid_params(e, None))?;
    let ledger = crate::core::handoff_ledger::load_ledger(std::path::Path::new(&path))
        .map_err(|e| ErrorData::internal_error(format!("load ledger: {e}"), None))?;
    Ok(crate::tools::ctx_handoff::format_show(
        std::path::Path::new(&path),
        &ledger,
    ))
}

/// `merge`: the served ledgers at `ledgers` become one new stored ledger
/// (`handoff_ledger::merge_ledgers`). A withheld input refuses the merge.
fn handle_merge(args: &Map<String, Value>, ctx: &ToolContext) -> Result<String, ErrorData> {
    let ledgers = get_str_array(args, "ledgers").unwrap_or_default();
    if ledgers.len() > crate::core::handoff_ledger::MAX_MERGE_INPUTS {
        return Err(ErrorData::invalid_params(
            format!(
                "ledgers: at most {} paths",
                crate::core::handoff_ledger::MAX_MERGE_INPUTS
            ),
            None,
        ));
    }
    let paths = ledgers
        .iter()
        .map(|path| ctx.resolve_path_sync(path).map(std::path::PathBuf::from))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| ErrorData::invalid_params(e, None))?;
    match crate::core::handoff_ledger::merge_ledgers(&paths) {
        Ok(outcome) => Ok(crate::tools::ctx_handoff::format_merged(&outcome)),
        Err(e) => Ok(format!("ctx_handoff merge\n refused: {e}")),
    }
}

fn resolve_curated_refs(
    args: &Map<String, Value>,
    ctx: &ToolContext,
) -> Result<Vec<(String, String)>, ErrorData> {
    let curated_paths = get_str_array(args, "paths").unwrap_or_default();
    let mut curated_refs: Vec<(String, String)> = Vec::new();
    if curated_paths.is_empty() {
        return Ok(curated_refs);
    }

    let mut resolved: Vec<String> = Vec::new();
    for p in curated_paths.into_iter().take(20) {
        let abs = ctx
            .resolve_path_sync(&p)
            .map_err(|e| ErrorData::invalid_params(e, None))?;
        resolved.push(abs);
    }

    let cache_handle = ctx
        .cache
        .as_ref()
        .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
    let Some(mut cache) = crate::server::bounded_lock::write(cache_handle, "ctx_handoff") else {
        return Err(ErrorData::internal_error(
            "cache busy (ctx_handoff) — retry in a moment",
            None,
        ));
    };
    for abs in &resolved {
        let mode = if crate::tools::ctx_read::is_instruction_file(abs) {
            "full"
        } else {
            "signatures"
        };
        let text =
            crate::tools::ctx_read::handle_with_task(&mut cache, abs, mode, ctx.crp_mode, None);
        curated_refs.push((abs.clone(), text));
    }

    Ok(curated_refs)
}

fn handle_create(args: &Map<String, Value>, ctx: &ToolContext) -> Result<String, ErrorData> {
    let curated_refs = resolve_curated_refs(args, ctx)?;

    let session_handle = ctx
        .session
        .as_ref()
        .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
    let session = { session_handle.blocking_read().clone() };
    let active_intent = session.active_structured_intent.clone();

    let tool_calls = ctx
        .tool_calls
        .as_ref()
        .map(|tc| tc.blocking_read().clone())
        .unwrap_or_default();
    let workflow = ctx
        .workflow
        .as_ref()
        .map(|w| w.blocking_read().clone())
        .unwrap_or_default();
    let agent_id = ctx
        .agent_id
        .as_ref()
        .map(|a| a.blocking_read().clone())
        .unwrap_or_default();
    let client_name = ctx
        .client_name
        .as_ref()
        .map(|c| c.blocking_read().clone())
        .unwrap_or_default();
    let project_root = session.project_root.clone();

    let (ledger, path) = crate::core::handoff_ledger::create_ledger(
        crate::core::handoff_ledger::CreateLedgerInput {
            agent_id,
            client_name: Some(client_name),
            project_root,
            session,
            tool_calls,
            workflow,
            curated_refs,
            request_id: get_str(args, "request_id"),
        },
    )
    .map_err(|e| ErrorData::internal_error(format!("create ledger: {e}"), None))?;

    let ctx_ledger_handle = ctx
        .ledger
        .as_ref()
        .ok_or_else(|| ErrorData::internal_error("ledger not available", None))?;
    let ctx_ledger = ctx_ledger_handle.blocking_read();
    let package = crate::core::handoff_ledger::HandoffPackage::build(
        ledger.clone(),
        active_intent.as_ref(),
        if ctx_ledger.entries.is_empty() {
            None
        } else {
            Some(&*ctx_ledger)
        },
    );
    drop(ctx_ledger);

    let mut output = crate::tools::ctx_handoff::format_created(&path, &ledger);
    let compact = package.format_compact();
    if !compact.is_empty() {
        output.push_str("\n\n");
        output.push_str(&compact);
    }

    Ok(output)
}

fn handle_export(args: &Map<String, Value>, ctx: &ToolContext) -> Result<String, ErrorData> {
    let (ledger, project_root) = match get_str(args, "ledger") {
        // A stored ledger (e.g. a merged one), served as a whole first.
        Some(stored) => {
            let stored = ctx
                .resolve_path_sync(&stored)
                .map_err(|e| ErrorData::invalid_params(e, None))?;
            let ledger = crate::core::handoff_ledger::load_ledger(std::path::Path::new(&stored))
                .map_err(|e| ErrorData::internal_error(format!("load ledger: {e}"), None))?;
            let project_root = ledger.project_root.clone();
            (ledger, project_root)
        }
        None => export_new_ledger(args, ctx)?,
    };
    export_ledger(args, ctx, ledger, project_root.as_deref())
}

/// A new ledger of the current session, as `create` writes it.
fn export_new_ledger(
    args: &Map<String, Value>,
    ctx: &ToolContext,
) -> Result<(HandoffLedgerV1, Option<String>), ErrorData> {
    let curated_refs = resolve_curated_refs(args, ctx)?;

    let session_handle = ctx
        .session
        .as_ref()
        .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
    let session = { session_handle.blocking_read().clone() };

    let tool_calls = ctx
        .tool_calls
        .as_ref()
        .map(|tc| tc.blocking_read().clone())
        .unwrap_or_default();
    let workflow = ctx
        .workflow
        .as_ref()
        .map(|w| w.blocking_read().clone())
        .unwrap_or_default();
    let agent_id = ctx
        .agent_id
        .as_ref()
        .map(|a| a.blocking_read().clone())
        .unwrap_or_default();
    let client_name = ctx
        .client_name
        .as_ref()
        .map(|c| c.blocking_read().clone())
        .unwrap_or_default();
    let project_root = session.project_root.clone();

    let (ledger, _ledger_path) = crate::core::handoff_ledger::create_ledger(
        crate::core::handoff_ledger::CreateLedgerInput {
            agent_id,
            client_name: Some(client_name),
            project_root: project_root.clone(),
            session,
            tool_calls,
            workflow,
            curated_refs,
            request_id: None,
        },
    )
    .map_err(|e| ErrorData::internal_error(format!("create ledger: {e}"), None))?;
    Ok((ledger, project_root))
}

fn export_ledger(
    args: &Map<String, Value>,
    ctx: &ToolContext,
    ledger: HandoffLedgerV1,
    project_root: Option<&str>,
) -> Result<String, ErrorData> {
    let privacy = crate::core::handoff_transfer_bundle::BundlePrivacyV1::parse(
        get_str(args, "privacy").as_deref(),
    );
    if privacy == crate::core::handoff_transfer_bundle::BundlePrivacyV1::Full
        && crate::core::roles::active_role_name() != "admin"
    {
        return Ok("ERROR: privacy=full requires role 'admin'.".to_string());
    }

    let mut bundle =
        crate::core::handoff_transfer_bundle::build_bundle_v1(ledger, project_root, privacy);
    // Sign every export (GL #465) so receivers can verify origin + integrity.
    // Signing failure (e.g. unwritable key dir) degrades to an unsigned bundle
    // with a warning — export stays local-first, import-side policy decides.
    let signer_id = ctx
        .agent_id
        .as_ref()
        .and_then(|a| a.blocking_read().clone())
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| crate::core::agent_identity::current_agent_id().to_string());
    let sign_warning = crate::core::handoff_transfer_bundle::sign_bundle(&mut bundle, &signer_id)
        .err()
        .map(|e| format!("WARNING: bundle not signed: {e}"));
    let json = crate::core::handoff_transfer_bundle::serialize_bundle_v1_pretty(&bundle)
        .map_err(|e| ErrorData::internal_error(e, None))?;

    let write = get_bool(args, "write").unwrap_or(false);
    let format = get_str(args, "format").unwrap_or_else(|| {
        if write || get_str(args, "path").is_some() || get_str(args, "filename").is_some() {
            "summary".to_string()
        } else {
            "json".to_string()
        }
    });

    let root = project_root.map(str::to_owned).unwrap_or_else(|| {
        std::env::current_dir()
            .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string())
    });
    let root_path = std::path::PathBuf::from(&root);

    let mut written: Option<std::path::PathBuf> = None;
    if write || get_str(args, "path").is_some() || get_str(args, "filename").is_some() {
        let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let candidate = if let Some(p) = get_str(args, "path") {
            let p = std::path::PathBuf::from(p);
            if p.is_absolute() {
                p
            } else {
                root_path.join(p)
            }
        } else if let Some(name) = get_str(args, "filename") {
            root_path.join(".lean-ctx").join("proofs").join(name)
        } else {
            let session_id = bundle.ledger.session.id.clone();
            root_path
                .join(".lean-ctx")
                .join("proofs")
                .join(format!("handoff-transfer-bundle-v1_{session_id}_{ts}.json"))
        };

        let jailed = match crate::core::io_boundary::jail_and_check_path(
            "ctx_handoff.export",
            candidate.as_path(),
            root_path.as_path(),
        ) {
            Ok((p, _warning)) => p,
            Err(e) => return Ok(e),
        };

        // Read-only-roots choke point (#475): export must not write a bundle into
        // a read-only root even when the jail allows reads.
        if let Err(e) = crate::core::pathjail::enforce_writable(&jailed) {
            return Ok(format!("Export write failed: {e}"));
        }
        if let Err(e) = crate::core::handoff_transfer_bundle::write_bundle_v1(&jailed, &json) {
            return Ok(format!("Export write failed: {e}"));
        }

        written = Some(jailed);
    }

    let out = match format.as_str() {
        // The structured formats (json/a2a) stay machine-parseable: signature
        // state is visible in the payload itself. Only the human summary
        // carries the explicit signing line.
        "summary" => {
            let mut s = crate::tools::ctx_handoff::format_exported(
                written.as_deref(),
                bundle.schema_version,
                json.len(),
                &bundle.privacy,
            );
            match sign_warning.as_deref() {
                Some(w) => {
                    s.push('\n');
                    s.push_str(w);
                }
                None => {
                    s.push_str(&format!("\n signed_by: {signer_id}"));
                }
            }
            s
        }
        // A2A envelope (GL#449): spec-shaped Task object a foreign agent can
        // parse without knowing the lean-ctx bundle format.
        "a2a" => {
            let task = crate::core::a2a::transfer::wrap_bundle_as_a2a_task(&bundle)
                .map_err(|e| ErrorData::internal_error(e, None))?;
            serde_json::to_string_pretty(&task)
                .map_err(|e| ErrorData::internal_error(format!("a2a serialization: {e}"), None))?
        }
        _ => {
            if let Some(p) = written.as_deref() {
                format!("{json}\n\npath: {}", p.display())
            } else {
                json
            }
        }
    };

    Ok(out)
}

fn handle_pull(args: &Map<String, Value>, ctx: &ToolContext) -> Result<String, ErrorData> {
    let path = get_str(args, "path")
        .ok_or_else(|| ErrorData::invalid_params("path is required for action=pull", None))?;
    let path = ctx
        .resolve_path_sync(&path)
        .map_err(|e| ErrorData::invalid_params(e, None))?;
    // The ledger is re-authorized as a whole before anything is applied: a
    // withheld ledger returns here, and nothing of it is stored.
    let (ledger, lineage) =
        crate::core::handoff_ledger::load_ledger_bound(std::path::Path::new(&path))
            .map_err(|e| ErrorData::internal_error(format!("load ledger: {e}"), None))?;
    let incoming = Incoming::Ledger(lineage.as_ref());

    let apply_workflow = get_bool(args, "apply_workflow").unwrap_or(true);
    let apply_session = get_bool(args, "apply_session").unwrap_or(true);
    let apply_knowledge = get_bool(args, "apply_knowledge").unwrap_or(true);

    if apply_workflow && let Some(wf_lock) = ctx.workflow.as_ref() {
        let mut wf = wf_lock.blocking_write();
        if ledger
            .workflow
            .as_ref()
            .is_some_and(|r| r.current == "done")
        {
            *wf = None;
        } else {
            wf.clone_from(&ledger.workflow);
        }
    }

    let mut skipped = 0usize;
    if apply_session {
        let session_handle = ctx
            .session
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
        let mut session = session_handle.blocking_write();
        skipped += restore_session(&mut session, &ledger, incoming);
        let _ = session.save();
    }

    let (knowledge_imported, contradictions) = if apply_knowledge {
        let (imported, contradictions, withheld) =
            import_knowledge_from_ledger(ctx, &ledger, incoming)?;
        skipped += withheld;
        (imported, contradictions)
    } else {
        (0, 0)
    };

    let lines = [
        "ctx_handoff pull".to_string(),
        format!(" path: {path}"),
        format!(" md5: {}", ledger.content_md5),
        format!(" applied_workflow: {apply_workflow}"),
        format!(" applied_session: {apply_session}"),
        format!(" imported_knowledge: {knowledge_imported}"),
        format!(" contradictions: {contradictions}"),
        format!(" skipped_withheld: {skipped}"),
    ];
    Ok(lines.join("\n"))
}

/// Where the texts a write path is about to store come from.
#[derive(Clone, Copy)]
enum Incoming<'a> {
    /// `pull`: a ledger of this store that was just served as a whole. With a
    /// lineage, each restored finding and fact that was bound at the
    /// ledger's creation is bound to the ledger's joined source set, and
    /// each that was not is stored originless; without a lineage all are
    /// stored originless, as before.
    Ledger(Option<&'a LedgerLineage>),
    /// `import`: the ledger of a transfer bundle. A bundle carries no binding
    /// and may come from a machine whose files cannot be read here: its texts
    /// are caller-supplied text without source authority. Each passes store
    /// admission first and is stored originless.
    Bundle,
}

impl<'a> Incoming<'a> {
    /// `text` as it may be stored, `None` when it must not be: a bundle text
    /// that admission withholds or that is restricted is never stored (E3),
    /// one it masks is stored masked.
    fn storable(self, text: &str) -> Option<String> {
        match self {
            Self::Ledger(_) => Some(text.to_owned()),
            Self::Bundle => crate::core::context_admission::carrier::admit_for_store(text).ok(),
        }
    }

    fn lineage(self) -> Option<&'a LedgerLineage> {
        match self {
            Self::Ledger(lineage) => lineage,
            Self::Bundle => None,
        }
    }
}

/// Apply the session part of a ledger to `session`. Returns how many texts
/// were NOT stored (a count only; no text of them is reported).
fn restore_session(
    session: &mut SessionState,
    ledger: &HandoffLedgerV1,
    incoming: Incoming<'_>,
) -> usize {
    let excerpt = &ledger.session;
    let mut skipped = 0usize;
    if let Some(task) = excerpt.task.as_deref() {
        if let Some(task) = incoming.storable(task) {
            session.set_task(&task, None);
        } else {
            skipped += 1;
        }
    }
    for decision in &excerpt.decisions {
        if let Some(decision) = incoming.storable(decision) {
            session.add_decision(&decision, None);
        } else {
            skipped += 1;
        }
    }
    for (index, finding) in excerpt.findings.iter().enumerate() {
        // The index is the finding's place in the stored ledger's list.
        let lineage = incoming.lineage().map(|lineage| (lineage, index));
        let stored = incoming
            .storable(finding)
            .is_some_and(|finding| restore_finding(session, &finding, lineage));
        if !stored {
            skipped += 1;
        }
    }
    let next_steps: Vec<String> = excerpt
        .next_steps
        .iter()
        .filter_map(|step| incoming.storable(step))
        .collect();
    skipped += excerpt.next_steps.len() - next_steps.len();
    session.next_steps = next_steps;
    skipped
}

/// Record `summary`, a finding restored from a ledger, in `session`. Returns
/// whether the session keeps it.
///
/// Without a lineage, and when the finding is not in the ledger's bound-part
/// set, it is recorded originless, as before — an unbound finding gets no
/// binding from bound siblings. A finding that was bound at the ledger's
/// creation is bound to the ledger's joined source set
/// (`LedgerLineage::carry`) — to all files of the ledger, not only its own.
/// While protection is active such a finding whose binding cannot be made or
/// written is not kept: it would be an unbound copy of text whose sources
/// are bound.
fn restore_finding(
    session: &mut SessionState,
    summary: &str,
    lineage: Option<(&LedgerLineage, usize)>,
) -> bool {
    use crate::core::archive::bindings::Item;
    let Some((lineage, index)) = lineage else {
        session.add_finding(None, None, summary);
        return true;
    };
    let id = session.id.clone();
    let item = Item::SessionFinding {
        session: &id,
        file: None,
    };
    let Ok(carried) = lineage.carry(&LedgerPart::Finding(summary), index, &item) else {
        return false;
    };
    session.add_finding(None, None, summary);
    let Some(carried) = carried else {
        return true;
    };
    let kept = session
        .findings
        .last()
        .is_some_and(|finding| finding.summary == summary);
    if (kept && carried.commit(&item, summary))
        || !crate::core::context_admission::stored::source_authority_required()
    {
        return true;
    }
    session.findings.pop();
    false
}

/// How the user admits a bundle that the import refused (GHSA-gvg8-285q-m62g).
const HANDOFF_UNVERIFIED_HINT: &str = "The user can pin the signer key for this agent id in \
     trusted-signers.toml, or set `mcp_import_allow_unverified = true` in the global config. \
     A tool argument cannot grant this.";

/// Global-only user opt-in; never merged from project config.
fn import_unverified_allowed() -> bool {
    crate::core::config::Config::load().mcp_import_allow_unverified
}

fn handle_import(args: &Map<String, Value>, ctx: &ToolContext) -> Result<String, ErrorData> {
    let path = get_str(args, "path")
        .ok_or_else(|| ErrorData::invalid_params("path is required for action=import", None))?;

    let project_root = ctx.project_root.clone();
    let root_path = std::path::PathBuf::from(&project_root);

    let candidate = {
        let p = std::path::PathBuf::from(&path);
        if p.is_absolute() {
            p
        } else {
            root_path.join(p)
        }
    };
    let jailed = match crate::core::io_boundary::jail_and_check_path(
        "ctx_handoff.import",
        candidate.as_path(),
        root_path.as_path(),
    ) {
        Ok((p, _warning)) => p,
        Err(e) => return Ok(e),
    };

    let bundle = match crate::core::handoff_transfer_bundle::read_bundle_v1(&jailed) {
        Ok(b) => b,
        Err(e) => return Ok(format!("Import failed: {e}")),
    };

    // Signature enforcement (GL #465): a bundle with broken/tampered signature
    // material is rejected fail-closed; legacy unsigned bundles import with a
    // warning; valid signatures surface the verified signer.
    let signature_line = match crate::core::handoff_transfer_bundle::check_bundle_signature(&bundle)
    {
        crate::core::handoff_transfer_bundle::BundleSignatureStatus::Invalid(reason) => {
            crate::core::audit_trail::record(crate::core::audit_trail::AuditEntryData {
                agent_id: bundle
                    .signer_agent_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                tool: "ctx_handoff".to_string(),
                action: Some("import_signature_invalid".to_string()),
                input_hash: crate::core::audit_trail::hash_input(args),
                output_tokens: 0,
                role: crate::core::roles::active_role_name(),
                event_type: crate::core::audit_trail::AuditEventType::SecurityViolation,
            });
            return Ok(format!(
                "IMPORT BLOCKED: bundle signature verification failed ({reason}).\n\
                 The bundle was modified after signing or carries broken signature material.\n\
                 Re-export it on the source agent (ctx_handoff export signs automatically)."
            ));
        }
        // GHSA-gvg8-285q-m62g: imported content is replayed into session and
        // knowledge stores. Unsigned or untrusted-signer bundles need the
        // user's global opt-in — a tool argument would let a prompt-injected
        // model vouch for the file itself.
        crate::core::handoff_transfer_bundle::BundleSignatureStatus::Signed(verified) => {
            if !verified.trusted && !import_unverified_allowed() {
                return Ok(format!(
                    "IMPORT BLOCKED: {}.\n\
                     A valid signature from a key that is not pinned for this signer proves \
                     integrity only, not who sent the bundle.\n{HANDOFF_UNVERIFIED_HINT}",
                    verified.describe()
                ));
            }
            format!(" signature: {}", verified.describe())
        }
        crate::core::handoff_transfer_bundle::BundleSignatureStatus::Unsigned => {
            if !import_unverified_allowed() {
                return Ok(format!(
                    "IMPORT BLOCKED: bundle is unsigned, so its origin and integrity \
                     cannot be checked.\n\
                     Re-export it on the source agent (ctx_handoff export signs automatically).\n\
                     {HANDOFF_UNVERIFIED_HINT}"
                ));
            }
            " signature: WARNING unsigned bundle imported (mcp_import_allow_unverified)".to_string()
        }
    };

    let warning =
        crate::core::handoff_transfer_bundle::project_identity_warning(&bundle, &project_root);

    if let Some(ref w) = warning {
        let source_hash = bundle
            .project
            .project_root_hash
            .as_deref()
            .unwrap_or("unknown");
        let target_hash = crate::core::project_hash::hash_project_root(&project_root);
        let role = crate::core::roles::active_role();
        if !role.io.allow_cross_project_search {
            let event = crate::core::memory_boundary::CrossProjectAuditEvent {
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                event_type: crate::core::memory_boundary::CrossProjectEventType::Import,
                source_project_hash: source_hash.to_string(),
                target_project_hash: target_hash,
                tool: "ctx_handoff".to_string(),
                action: "import".to_string(),
                facts_accessed: 0,
                allowed: false,
                policy_reason: format!("identity mismatch: {w}"),
            };
            crate::core::memory_boundary::record_audit_event(&event);
            return Ok(format!(
                "IMPORT BLOCKED: project identity mismatch. {w}\n\
                 Set `io.allow_cross_project_search = true` in your role to allow cross-project imports."
            ));
        }
    }

    let schema_version = bundle.schema_version;
    let ledger = bundle.ledger;

    let apply_workflow = get_bool(args, "apply_workflow").unwrap_or(true);
    let apply_session = get_bool(args, "apply_session").unwrap_or(true);
    let apply_knowledge = get_bool(args, "apply_knowledge").unwrap_or(true);

    if apply_workflow && let Some(wf_lock) = ctx.workflow.as_ref() {
        let mut wf = wf_lock.blocking_write();
        if ledger
            .workflow
            .as_ref()
            .is_some_and(|r| r.current == "done")
        {
            *wf = None;
        } else {
            wf.clone_from(&ledger.workflow);
        }
    }

    // A bundle is caller-supplied text: no binding travels in it, and its
    // ledger never passed `load_ledger`. Every text is admitted for the store
    // before it is kept and is stored originless; the reply carries counts.
    let mut skipped = 0usize;
    if apply_session {
        let session_handle = ctx
            .session
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
        let mut session = session_handle.blocking_write();
        skipped += restore_session(&mut session, &ledger, Incoming::Bundle);
        let _ = session.save();
    }

    let (knowledge_imported, contradictions) = if apply_knowledge {
        let (imported, contradictions, withheld) =
            import_knowledge_from_ledger(ctx, &ledger, Incoming::Bundle)?;
        skipped += withheld;
        (imported, contradictions)
    } else {
        (0, 0)
    };

    Ok(crate::tools::ctx_handoff::format_imported(
        jailed.as_path(),
        schema_version,
        knowledge_imported,
        contradictions,
        skipped,
        warning.as_deref(),
        &signature_line,
    ))
}

/// Shared knowledge import logic used by both pull and import actions.
/// Returns (facts stored, contradictions, facts not stored).
///
/// A fact value is stored as `incoming` allows ([`Incoming::storable`]). From
/// a ledger with a lineage the fact is bound to the ledger's joined source
/// set once the store kept exactly its text
/// (`consolidation_engine::store_carried`); while protection is active a
/// fact that cannot be bound that way is not stored. Otherwise it is stored
/// originless.
fn import_knowledge_from_ledger(
    ctx: &ToolContext,
    ledger: &HandoffLedgerV1,
    incoming: Incoming<'_>,
) -> Result<(u32, u32, usize), ErrorData> {
    use crate::core::archive::bindings::Item;
    let project_root = ctx.project_root.clone();
    let session_id = {
        let session_handle = ctx
            .session
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
        let s = session_handle.blocking_read();
        s.id.clone()
    };

    let policy = match crate::core::config::Config::load().memory_policy_effective() {
        Ok(p) => p,
        Err(e) => {
            let path = crate::core::config::Config::path().map_or_else(
                || "~/.lean-ctx/config.toml".to_string(),
                |p| p.display().to_string(),
            );
            return Err(ErrorData::internal_error(
                format!("Error: invalid memory policy: {e}\nFix: edit {path}"),
                None,
            ));
        }
    };

    // Import under the cross-process lock (#326/#594): the daemon, the MCP
    // server and a CLI handoff can all write knowledge concurrently.
    let result =
        crate::core::knowledge::ProjectKnowledge::mutate_locked(&project_root, |knowledge| {
            let mut imported = 0u32;
            let mut contradictions = 0u32;
            let mut skipped = 0usize;
            let root = knowledge.project_root.clone();
            for (index, fact) in ledger.knowledge.facts.iter().enumerate() {
                let Some(value) = incoming.storable(&fact.value) else {
                    skipped += 1;
                    continue;
                };
                let carried = match incoming.lineage() {
                    Some(lineage) => {
                        let item = Item::KnowledgeFact {
                            project: &root,
                            category: &fact.category,
                            key: &fact.key,
                        };
                        let part = LedgerPart::Fact {
                            category: &fact.category,
                            key: &fact.key,
                            value: &value,
                        };
                        lineage.carry(&part, index, &item)
                    }
                    None => Ok(None),
                };
                let mut contradiction = false;
                let stored = crate::core::consolidation_engine::store_carried(
                    knowledge,
                    carried,
                    [&fact.category, &fact.key, &value],
                    false,
                    |knowledge| {
                        contradiction = knowledge
                            .remember(
                                &fact.category,
                                &fact.key,
                                &value,
                                &session_id,
                                fact.confidence,
                                &policy,
                            )
                            .is_some();
                    },
                );
                if !stored {
                    skipped += 1;
                    continue;
                }
                if contradiction {
                    contradictions += 1;
                }
                imported += 1;
            }
            (imported, contradictions, skipped)
        });

    match result {
        Ok((_, counts)) => Ok(counts),
        Err(e) => Err(ErrorData::internal_error(
            format!("knowledge import save failed: {e}"),
            None,
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use serde_json::{Map, Value, json};
    use tokio::sync::RwLock;

    use super::{handle_import, handle_pull};
    use crate::core::archive::delivered::LineageRefusal;
    use crate::core::context_admission::stored::lineage_support::{
        BOUND_FACT, TYPED_FACT, bound_fact, deliver, governed, project, reusable, scoped, two_facts,
    };
    use crate::core::handoff_ledger::{CreateLedgerInput, KnowledgeFactMini, create_ledger};
    use crate::core::handoff_transfer_bundle::{
        BundlePrivacyV1, build_bundle_v1, serialize_bundle_v1_pretty,
    };
    use crate::core::knowledge::ProjectKnowledge;
    use crate::core::policy::runtime::TestPolicyOverride;
    use crate::core::session::SessionState;
    use crate::server::tool_trait::ToolContext;
    use crate::tools::ctx_knowledge::fact_served;

    const POOL: &str = "pub fn lease() {}\n";
    const AUTH: &str = "pub fn verify_rs256() {}\n";
    const FINDING: &str = "the pool leaks one lease";
    const TYPED_FINDING: &str = "noted without a file";
    const DECISION: &str = "lease from the pool only";
    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
    const MARKED: &str = "*** TOP SECRET ***\nlaunch window and codes";

    /// A tool context of `root` with a fresh session, as `pull` and `import`
    /// need it. Ledgers live in the data directory, which is made a trusted
    /// root of the context so that a ledger path resolves.
    fn context(root: &str, data: &Path) -> (ToolContext, Arc<RwLock<SessionState>>) {
        let mut session = SessionState::new();
        session.project_root = Some(root.to_owned());
        let session = Arc::new(RwLock::new(session));
        let canonical = data.canonicalize().unwrap_or_else(|_| data.to_path_buf());
        let ctx = ToolContext {
            project_root: root.to_owned(),
            extra_roots: vec![
                data.to_string_lossy().into_owned(),
                canonical.to_string_lossy().into_owned(),
            ],
            session: Some(Arc::clone(&session)),
            ..ToolContext::default()
        };
        (ctx, session)
    }

    fn args(action: &str, path: &str) -> Map<String, Value> {
        json!({ "action": action, "path": path })
            .as_object()
            .cloned()
            .expect("object")
    }

    /// The ledger of a writer session of `root`: a finding bound to
    /// `pool.rs`, one decision, the fact bound to `auth.rs` and — with
    /// `typed`, a mixed ledger — a finding and a fact without a binding.
    fn ledger_of_a_writer(root: &str, typed: bool) -> PathBuf {
        std::fs::write(format!("{root}/pool.rs"), POOL).expect("source");
        let mut writer = SessionState::new();
        writer.project_root = Some(root.to_owned());
        writer.add_finding_for_file(Some("pool.rs"), Some(1), FINDING);
        writer.add_decision(DECISION, None);
        if typed {
            writer.add_finding_for_file(None, None, TYPED_FINDING);
            two_facts(root, "architecture");
        } else {
            bound_fact(root, "architecture");
        }
        let (_, path) = scoped(root, || {
            create_ledger(CreateLedgerInput {
                agent_id: None,
                client_name: None,
                project_root: Some(root.to_owned()),
                session: writer,
                tool_calls: Vec::new(),
                workflow: None,
                curated_refs: Vec::new(),
                request_id: None,
            })
        })
        .expect("create ledger");
        path
    }

    fn pull(root: &str, ctx: &ToolContext, ledger: &Path) -> Result<String, String> {
        let args = args("pull", &ledger.to_string_lossy());
        scoped(root, || handle_pull(&args, ctx)).map_err(|error| error.message.to_string())
    }

    /// Deliver the finding `summary` of `session` the way a renderer does.
    fn finding(root: &str, session: &RwLock<SessionState>, summary: &str) -> (String, usize) {
        let session = session.blocking_read().clone();
        let found = session
            .findings
            .iter()
            .find(|finding| finding.summary == summary)
            .cloned();
        deliver(root, || match found {
            Some(found) if session.finding_served(&found) => format!("finding: {summary}"),
            Some(_) => "finding withheld".to_owned(),
            None => "no such finding".to_owned(),
        })
    }

    /// Deliver the fact `token-check` the way a recall does.
    fn recall(root: &str) -> (String, usize) {
        deliver(root, || {
            if fact_served(root, "architecture", "token-check", BOUND_FACT) {
                format!("[architecture] {BOUND_FACT}")
            } else {
                "fact withheld".to_owned()
            }
        })
    }

    /// A finding and a fact pulled from a ledger whose parts were all bound
    /// are bound to the ledger's joined source set: their delivery is
    /// registered, and each is withheld when ANY file of the set goes — also
    /// the file the other item came from (the stated over-approximation).
    #[test]
    fn pull_binds_restored_items_to_the_joined_set_of_an_all_bound_ledger() {
        let data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let ledger = ledger_of_a_writer(&root, false);
        let (ctx, session) = context(&root, data.path());

        let reply = pull(&root, &ctx, &ledger).expect("pull");
        assert!(reply.contains("imported_knowledge: 1"), "{reply}");
        assert!(reply.contains("skipped_withheld: 0"), "{reply}");
        assert!(
            !reply.contains(FINDING) && !reply.contains("RS256"),
            "{reply}"
        );

        let (delivered, recorded) = finding(&root, &session, FINDING);
        assert_eq!(delivered, format!("finding: {FINDING}"));
        assert_eq!(recorded, 1, "the restored finding has a file lineage");
        assert_eq!(reusable(&root, &delivered), Ok(()));
        let (recalled, recorded) = recall(&root);
        assert!(recalled.contains("RS256"), "{recalled}");
        assert_eq!(recorded, 1);

        // `pool.rs` is the finding's file: the fact is bound to it as well.
        std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
        assert_eq!(recall(&root), ("fact withheld".to_owned(), 0));
        assert_eq!(
            reusable(&root, &delivered),
            Err(LineageRefusal::SourceNoLongerAdmitted)
        );
        std::fs::write(format!("{root}/pool.rs"), POOL).expect("same bytes");
        assert_eq!(recall(&root).1, 1);

        // `auth.rs` is the fact's file: the finding is bound to it as well.
        std::fs::remove_file(format!("{root}/auth.rs")).expect("delete");
        assert_eq!(
            finding(&root, &session, FINDING),
            ("finding withheld".to_owned(), 0)
        );
    }

    /// Nothing is stored from a withheld ledger: `pull` answers with the
    /// typed refusal before any write, for a deleted finding source and for
    /// a reclassified fact source; with the sources back it applies.
    #[test]
    fn pull_stores_nothing_from_a_withheld_ledger() {
        let data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let ledger = ledger_of_a_writer(&root, false);
        let (ctx, session) = context(&root, data.path());
        let untouched = || {
            let session = session.blocking_read();
            session.findings.is_empty() && session.decisions.is_empty() && session.task.is_none()
        };
        let facts = || {
            scoped(&root, || {
                ProjectKnowledge::load(&root).map_or(0, |knowledge| knowledge.facts.len())
            })
        };
        let before = facts();

        std::fs::remove_file(format!("{root}/pool.rs")).expect("delete");
        let refusal = pull(&root, &ctx, &ledger).expect_err("the ledger is withheld");
        assert!(refusal.contains("store.source_unavailable"), "{refusal}");
        assert!(
            !refusal.contains(FINDING) && !refusal.contains("RS256"),
            "{refusal}"
        );
        assert!(untouched());
        assert_eq!(facts(), before);

        std::fs::write(format!("{root}/pool.rs"), POOL).expect("same bytes");
        std::fs::write(
            format!("{root}/auth.rs"),
            format!("*** TOP SECRET ***\n{AUTH}"),
        )
        .expect("reclassify");
        let refusal = pull(&root, &ctx, &ledger).expect_err("the ledger is withheld");
        assert!(refusal.contains("store."), "{refusal}");
        assert!(
            !refusal.contains(FINDING) && !refusal.contains("RS256"),
            "{refusal}"
        );
        assert!(untouched());
        assert_eq!(facts(), before);

        std::fs::write(format!("{root}/auth.rs"), AUTH).expect("same bytes");
        pull(&root, &ctx, &ledger).expect("served again");
        assert!(!untouched());
    }

    /// Deliver the fact `release-duty`, which no writer ever bound.
    fn recall_typed(root: &str) -> (String, usize) {
        deliver(root, || {
            if fact_served(root, "architecture", "release-duty", TYPED_FACT) {
                format!("[architecture] {TYPED_FACT}")
            } else {
                "fact withheld".to_owned()
            }
        })
    }

    /// A mixed ledger — one bound and one unbound finding, one bound and one
    /// unbound fact — does not launder: `pull` restores the bound items WITH
    /// a binding to the ledger's joined set (registered; withheld when a file
    /// of the set goes) and the unbound items originless (never registered,
    /// never withheld for a file: they got no binding from their siblings).
    /// Once a bound source is withdrawn the whole ledger is withheld and
    /// nothing is restored.
    #[test]
    fn pull_from_a_mixed_ledger_keeps_the_binding_of_each_bound_part() {
        let data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let ledger = ledger_of_a_writer(&root, true);
        let (ctx, session) = context(&root, data.path());

        let reply = pull(&root, &ctx, &ledger).expect("a mixed ledger is served");
        assert!(reply.contains("imported_knowledge: 2"), "{reply}");
        assert!(reply.contains("skipped_withheld: 0"), "{reply}");

        let (bound, recorded) = finding(&root, &session, FINDING);
        assert_eq!(bound, format!("finding: {FINDING}"));
        assert_eq!(recorded, 1, "the bound finding keeps a file lineage");
        assert_eq!(reusable(&root, &bound), Ok(()));
        assert_eq!(recall(&root).1, 1, "the bound fact keeps a file lineage");
        let (typed, recorded) = finding(&root, &session, TYPED_FINDING);
        assert_eq!(typed, format!("finding: {TYPED_FINDING}"));
        assert_eq!(recorded, 0, "the unbound finding stays originless");
        assert_eq!(reusable(&root, &typed), Err(LineageRefusal::Unknown));
        let (typed_fact, recorded) = recall_typed(&root);
        assert!(typed_fact.contains("captain"), "{typed_fact}");
        assert_eq!(recorded, 0, "the unbound fact stays originless");

        // A file of the joined set goes: the bound items are withheld, the
        // unbound ones are delivered as before.
        std::fs::remove_file(format!("{root}/auth.rs")).expect("delete");
        assert_eq!(
            finding(&root, &session, FINDING),
            ("finding withheld".to_owned(), 0)
        );
        assert_eq!(recall(&root), ("fact withheld".to_owned(), 0));
        assert_eq!(finding(&root, &session, TYPED_FINDING).0, typed);
        assert_eq!(recall_typed(&root).0, typed_fact);

        // The same ledger now: withheld as a whole, nothing restored.
        let (fresh_ctx, fresh) = context(&root, data.path());
        let refusal = pull(&root, &fresh_ctx, &ledger).expect_err("the ledger is withheld");
        assert!(refusal.contains("store.source_unavailable"), "{refusal}");
        assert!(
            !refusal.contains(FINDING) && !refusal.contains(TYPED_FINDING),
            "{refusal}"
        );
        let fresh = fresh.blocking_read();
        assert!(fresh.findings.is_empty() && fresh.decisions.is_empty());
    }

    /// A ledger created under protection whose bound record is gone, or
    /// whose bound element was edited, is not treated as "nothing was
    /// bound": `pull` gets the typed refusal and stores nothing.
    #[test]
    fn pull_stores_nothing_from_a_ledger_that_disagrees_with_its_bound_record() {
        let data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let ledger = ledger_of_a_writer(&root, true);
        let (ctx, session) = context(&root, data.path());
        let refused = |what: &str| {
            let refusal = pull(&root, &ctx, &ledger).expect_err(what);
            assert!(
                refusal.contains("store.ledger_authority_inconsistent"),
                "{what}: {refusal}"
            );
            assert!(
                !refusal.contains(FINDING) && !refusal.contains("two leases"),
                "{refusal}"
            );
            let session = session.blocking_read();
            assert!(
                session.findings.is_empty() && session.decisions.is_empty(),
                "{what}"
            );
        };

        let stored = std::fs::read_to_string(&ledger).expect("ledger");
        std::fs::write(
            &ledger,
            stored.replace(FINDING, "the pool leaks two leases"),
        )
        .expect("edit");
        refused("the text of a bound element was edited");
        std::fs::write(&ledger, &stored).expect("restore");

        let sidecar = ledger.with_extension("authority");
        let mut authority: Value =
            serde_json::from_str(&std::fs::read_to_string(&sidecar).expect("sidecar"))
                .expect("json");
        authority
            .as_object_mut()
            .expect("object")
            .remove("bound_parts");
        std::fs::write(&sidecar, authority.to_string()).expect("strip");
        refused("the bound record was removed");
    }

    /// A ledger written without protection carries the unchecked marker and
    /// no bound record: every item restored from it is originless — no
    /// register entry is written for a restored finding.
    #[test]
    fn pull_from_an_unchecked_ledger_restores_every_item_originless() {
        use crate::core::archive::bindings::{self, Item};
        let data = crate::core::data_dir::isolated_data_dir();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let ledger = ledger_of_a_writer(&root, true);
        let (ctx, session) = context(&root, data.path());

        pull(&root, &ctx, &ledger).expect("pull");
        let session = session.blocking_read();
        assert_eq!(session.findings.len(), 2);
        for restored in &session.findings {
            let item = Item::SessionFinding {
                session: &session.id,
                file: None,
            };
            let entry = bindings::held(&item, &restored.summary, Some(&root));
            assert_eq!(entry.map(|held| held.is_some()), Ok(false));
        }
    }

    /// A bundle is caller-supplied text: a clean value is stored originless,
    /// a key-shaped value is stored masked, a restricted value is skipped
    /// and counted, and the reply states counts only.
    #[test]
    fn import_stores_bundle_texts_originless_after_store_admission() {
        let data = governed();
        let _community = TestPolicyOverride::set(None);
        let (_directory, root) = project();
        let (ctx, session) = context(&root, data.path());
        let fact = |key: &str, value: &str| KnowledgeFactMini {
            category: "architecture".to_owned(),
            key: key.to_owned(),
            value: value.to_owned(),
            confidence: 0.9,
        };
        let mut bundle = build_bundle_v1(
            crate::core::handoff_ledger::HandoffLedgerV1::default(),
            Some(&root),
            BundlePrivacyV1::Redacted,
        );
        bundle.ledger.session.findings = vec![
            FINDING.to_owned(),
            format!("the deploy key is {AWS_KEY}"),
            MARKED.to_owned(),
        ];
        bundle.ledger.knowledge.facts = vec![
            fact("release-duty", TYPED_FACT),
            fact(
                "deploy-key",
                &format!("Deployments are signed with the key {AWS_KEY}"),
            ),
            fact("launch", MARKED),
        ];
        let json = serialize_bundle_v1_pretty(&bundle).expect("bundle");
        std::fs::write(format!("{root}/bundle.json"), json).expect("bundle file");
        // The bundle is unsigned: over MCP that needs the user's global opt-in
        // (GHSA-gvg8-285q-m62g), a top-level key ahead of the governed table.
        let config = crate::core::paths::config_dir_read_only()
            .expect("isolated config dir")
            .join("config.toml");
        let governed_config = std::fs::read_to_string(&config).expect("governed config");
        std::fs::write(
            &config,
            format!("mcp_import_allow_unverified = true\n{governed_config}"),
        )
        .expect("opt in");

        let reply = scoped(&root, || {
            handle_import(&args("import", "bundle.json"), &ctx)
        })
        .expect("import");
        assert!(reply.contains("imported_knowledge: 2"), "{reply}");
        assert!(reply.contains("skipped_withheld: 2"), "{reply}");
        for text in [FINDING, "captain", AWS_KEY, "launch window", "deploy key"] {
            assert!(!reply.contains(text), "counts only — {text}: {reply}");
        }

        let summaries: Vec<String> = session
            .blocking_read()
            .findings
            .iter()
            .map(|finding| finding.summary.clone())
            .collect();
        assert_eq!(summaries.len(), 2, "{summaries:?}");
        assert_eq!(summaries[0], FINDING);
        assert!(
            summaries[1].starts_with("the deploy key is "),
            "{summaries:?}"
        );
        assert!(!summaries[1].contains(AWS_KEY), "stored masked");

        let values: Vec<String> = scoped(&root, || {
            ProjectKnowledge::load(&root)
                .expect("store")
                .facts
                .iter()
                .map(|fact| fact.value.clone())
                .collect()
        });
        assert!(values.iter().any(|value| value == TYPED_FACT), "{values:?}");
        assert!(
            values
                .iter()
                .any(|value| value.starts_with("Deployments are signed"))
        );
        for value in &values {
            assert!(
                !value.contains(AWS_KEY) && !value.contains("launch"),
                "{values:?}"
            );
        }

        // Stored originless: the deliveries have no lineage.
        let (delivered, recorded) = finding(&root, &session, FINDING);
        assert_eq!(delivered, format!("finding: {FINDING}"));
        assert_eq!(recorded, 0);
        let (typed, recorded) = deliver(&root, || {
            assert!(fact_served(
                &root,
                "architecture",
                "release-duty",
                TYPED_FACT
            ));
            format!("[architecture] {TYPED_FACT}")
        });
        assert_eq!(recorded, 0);
        assert_eq!(reusable(&root, &typed), Err(LineageRefusal::Unknown));
    }

    use crate::core::handoff_transfer_bundle as bundle_mod;

    fn import(root: &std::path::Path, file: &std::path::Path, extra: Value) -> String {
        let mut args = json!({
            "action": "import",
            "path": file.to_string_lossy(),
            "apply_workflow": false,
            "apply_session": false,
            "apply_knowledge": false,
        });
        if let (Some(args), Value::Object(extra)) = (args.as_object_mut(), extra) {
            args.extend(extra);
        }
        let ctx = ToolContext {
            project_root: root.to_string_lossy().to_string(),
            ..ToolContext::default()
        };
        match handle_import(args.as_object().unwrap(), &ctx) {
            Ok(text) => text,
            Err(e) => format!("error: {e:?}"),
        }
    }

    /// GHSA-gvg8-285q-m62g: over MCP, unsigned and untrusted-signer bundles
    /// need the user's global opt-in; the model cannot grant it by argument.
    #[test]
    fn mcp_import_requires_user_opt_in_for_unverified_bundles() {
        let data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();

        let unsigned = bundle_mod::build_bundle_v1(
            crate::core::handoff_ledger::HandoffLedgerV1::default(),
            None,
            bundle_mod::BundlePrivacyV1::Redacted,
        );
        let key = ed25519_dalek::SigningKey::from_bytes(&[21u8; 32]);
        let mut foreign = unsigned.clone();
        foreign.signer_public_key = Some(crate::core::agent_identity::hex_encode(
            &key.verifying_key().to_bytes(),
        ));
        foreign.signer_agent_id = Some("trusted-release-bot".into());
        foreign.signature_scheme = Some(bundle_mod::BOUND_SIGNATURE_SCHEME.into());
        let canonical = serde_json::to_string(&foreign).unwrap();
        foreign.signature = Some(crate::core::agent_identity::hex_encode(
            &crate::core::agent_identity::sign_bytes_with(&key, canonical.as_bytes()),
        ));

        let unsigned_path = project.path().join("unsigned.json");
        std::fs::write(&unsigned_path, serde_json::to_string(&unsigned).unwrap()).unwrap();
        let foreign_path = project.path().join("foreign.json");
        std::fs::write(&foreign_path, serde_json::to_string(&foreign).unwrap()).unwrap();

        for path in [&unsigned_path, &foreign_path] {
            for extra in [json!({}), json!({"allow_unsigned": true})] {
                let out = import(project.path(), path, extra);
                assert!(out.starts_with("IMPORT BLOCKED"), "{out}");
                assert!(out.contains("mcp_import_allow_unverified"), "{out}");
                assert!(!out.contains("verified (signer="), "{out}");
            }
        }

        let global = crate::core::config::Config::path().unwrap();
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(&global, "mcp_import_allow_unverified = true\n").unwrap();
        for path in [&unsigned_path, &foreign_path] {
            let out = import(project.path(), path, json!({}));
            assert!(!out.starts_with("IMPORT BLOCKED"), "{out}");
        }
        let out = import(project.path(), &foreign_path, json!({}));
        assert!(out.contains("(untrusted"), "{out}");
        std::fs::remove_file(&global).unwrap();
        drop(data);
    }
}
