use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

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
            "Context Ledger Protocol (hashed, deterministic, local-first). Actions: create|show|list|pull|clear|export|import.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "show", "list", "pull", "clear", "export", "import"],
                        "description": "Operation to perform (default: list)"
                    },
                    "path": { "type": "string", "description": "Ledger file path (for show/pull/import)" },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Optional file paths for curated refs (for create/export)" },
                    "format": { "type": "string", "description": "Output format (json|summary|a2a — a2a wraps the bundle in a spec-shaped A2A Task envelope)" },
                    "write": { "type": "boolean", "description": "Write export to file" },
                    "privacy": { "type": "string", "description": "Export privacy: redacted (default) | full (admin only)" },
                    "filename": { "type": "string", "description": "Custom filename for export" },
                    "apply_workflow": { "type": "boolean", "description": "For pull/import: apply workflow state (default: true)" },
                    "apply_session": { "type": "boolean", "description": "For pull/import: apply session snapshot (default: true)" },
                    "apply_knowledge": { "type": "boolean", "description": "For pull/import: import knowledge facts (default: true)" }
                }
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
            "list" => handle_list(),
            "clear" => handle_clear(),
            "show" => handle_show(args, ctx)?,
            "create" => handle_create(args, ctx)?,
            "export" => handle_export(args, ctx)?,
            "pull" => handle_pull(args, ctx)?,
            "import" => handle_import(args, ctx)?,
            _ => "Unknown action. Use: create, show, list, pull, clear, export, import".to_string(),
        };

        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action),
            path: None,
            changed: false,
            shell_outcome: None,
        })
    }
}

fn handle_list() -> String {
    let items = crate::core::handoff_ledger::list_ledgers();
    crate::tools::ctx_handoff::format_list(&items)
}

fn handle_clear() -> String {
    let removed = crate::core::handoff_ledger::clear_ledgers().unwrap_or_default();
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
        },
    )
    .map_err(|e| ErrorData::internal_error(format!("create ledger: {e}"), None))?;

    let privacy = crate::core::handoff_transfer_bundle::BundlePrivacyV1::parse(
        get_str(args, "privacy").as_deref(),
    );
    if privacy == crate::core::handoff_transfer_bundle::BundlePrivacyV1::Full
        && crate::core::roles::active_role_name() != "admin"
    {
        return Ok("ERROR: privacy=full requires role 'admin'.".to_string());
    }

    let mut bundle = crate::core::handoff_transfer_bundle::build_bundle_v1(
        ledger,
        project_root.as_deref(),
        privacy,
    );
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

    let root = project_root.clone().unwrap_or_else(|| {
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

        let mut ev = crate::core::evidence_ledger::EvidenceLedgerV1::load();
        let _ = ev.record_artifact_file(
            "proof:handoff-transfer-bundle-v1",
            &jailed,
            chrono::Utc::now(),
        );
        let _ = ev.save();

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
    let ledger = crate::core::handoff_ledger::load_ledger(std::path::Path::new(&path))
        .map_err(|e| ErrorData::internal_error(format!("load ledger: {e}"), None))?;

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

    if apply_session {
        let session_handle = ctx
            .session
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
        let mut session = session_handle.blocking_write();
        if let Some(t) = ledger.session.task.as_deref() {
            session.set_task(t, None);
        }
        for d in &ledger.session.decisions {
            session.add_decision(d, None);
        }
        for f in &ledger.session.findings {
            session.add_finding(None, None, f);
        }
        session.next_steps.clone_from(&ledger.session.next_steps);
        let _ = session.save();
    }

    let (knowledge_imported, contradictions) = if apply_knowledge {
        import_knowledge_from_ledger(ctx, &ledger)?
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
    ];
    Ok(lines.join("\n"))
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
        crate::core::handoff_transfer_bundle::BundleSignatureStatus::Verified(signer) => {
            format!(" signature: verified (signer={signer})")
        }
        crate::core::handoff_transfer_bundle::BundleSignatureStatus::Unsigned => {
            " signature: WARNING unsigned legacy bundle (re-export to sign)".to_string()
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

    if apply_session {
        let session_handle = ctx
            .session
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
        let mut session = session_handle.blocking_write();
        if let Some(t) = ledger.session.task.as_deref() {
            session.set_task(t, None);
        }
        for d in &ledger.session.decisions {
            session.add_decision(d, None);
        }
        for f in &ledger.session.findings {
            session.add_finding(None, None, f);
        }
        session.next_steps.clone_from(&ledger.session.next_steps);
        let _ = session.save();
    }

    let (knowledge_imported, contradictions) = if apply_knowledge {
        import_knowledge_from_ledger(ctx, &ledger)?
    } else {
        (0, 0)
    };

    Ok(crate::tools::ctx_handoff::format_imported(
        jailed.as_path(),
        schema_version,
        knowledge_imported,
        contradictions,
        warning.as_deref(),
        &signature_line,
    ))
}

/// Shared knowledge import logic used by both pull and import actions.
fn import_knowledge_from_ledger(
    ctx: &ToolContext,
    ledger: &crate::core::handoff_ledger::HandoffLedgerV1,
) -> Result<(u32, u32), ErrorData> {
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
            for fact in &ledger.knowledge.facts {
                let c = knowledge.remember(
                    &fact.category,
                    &fact.key,
                    &fact.value,
                    &session_id,
                    fact.confidence,
                    &policy,
                );
                if c.is_some() {
                    contradictions += 1;
                }
                imported += 1;
            }
            let _ = knowledge.run_memory_lifecycle(&policy);
            (imported, contradictions)
        });

    match result {
        Ok((_, counts)) => Ok(counts),
        Err(e) => Err(ErrorData::internal_error(
            format!("knowledge import save failed: {e}"),
            None,
        )),
    }
}
