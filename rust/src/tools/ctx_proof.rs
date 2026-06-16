use std::path::Path;

use crate::core::context_ir::{ContextIrV1, write_project_context_ir};
use crate::core::context_proof::{ProofOptions, ProofSources, collect_v1, write_project_proof};
use crate::core::degradation_policy::{evaluate_v1_for_tool, write_project_degradation_policy};

pub fn handle_export(
    project_root: &str,
    format: Option<&str>,
    write: bool,
    filename: Option<&str>,
    max_evidence: Option<usize>,
    max_ledger_files: Option<usize>,
    sources: ProofSources,
) -> Result<String, String> {
    let opts = ProofOptions {
        max_evidence: max_evidence.unwrap_or(50),
        max_ledger_files: max_ledger_files.unwrap_or(10),
    };
    let proof = collect_v1(sources, opts);
    let ir = ContextIrV1::load();

    let mut out = String::new();
    let mut written: Option<String> = None;
    let mut written_ir: Option<String> = None;
    let mut written_degradation: Option<String> = None;
    let mut written_autonomy: Option<String> = None;
    let mut written_architecture_json: Option<String> = None;
    let mut written_architecture_html: Option<String> = None;
    if write {
        let root = Path::new(project_root);
        let ts = chrono::Utc::now().format("%Y-%m-%d_%H%M%S").to_string();
        let created_at = chrono::Utc::now().to_rfc3339();
        let default_proof_name = format!("context-proof-v1_{ts}.json");
        let ir_name = format!("context-ir-v1_{ts}.json");
        let degradation_name = format!("degradation-policy-v1_{ts}.json");
        let autonomy_name = format!("autonomy-drivers-v1_{ts}.json");
        let architecture_json_name = format!("architecture-overview-v1_{ts}.json");
        let architecture_html_name = format!("architecture-overview-v1_{ts}.html");
        let proof_name = filename.unwrap_or(default_proof_name.as_str());

        let path = write_project_proof(root, &proof, Some(proof_name))?;
        written = Some(path.to_string_lossy().to_string());

        let ir_path = write_project_context_ir(root, &ir, Some(&ir_name))?;
        written_ir = Some(ir_path.to_string_lossy().to_string());

        let policy = evaluate_v1_for_tool("ctx_proof", Some(&created_at));
        let policy_path = write_project_degradation_policy(root, &policy, Some(&degradation_name))?;
        written_degradation = Some(policy_path.to_string_lossy().to_string());

        {
            let ts = chrono::Utc::now();
            let mut ledger = crate::core::evidence_ledger::EvidenceLedgerV1::load();
            let _ = ledger.record_artifact_file("proof:context-proof-v1", path.as_path(), ts);
            let _ = ledger.record_artifact_file("proof:context-ir-v1", ir_path.as_path(), ts);
            let _ = ledger.record_artifact_file(
                "proof:degradation-policy-v1",
                policy_path.as_path(),
                ts,
            );
            let _ = ledger.save();
        }

        let autonomy = crate::core::autonomy_drivers::AutonomyDriversV1::load();
        let autonomy_path = crate::core::autonomy_drivers::write_project_autonomy_drivers_v1(
            root,
            &autonomy,
            Some(&autonomy_name),
        )?;
        written_autonomy = Some(autonomy_path.to_string_lossy().to_string());

        {
            let ts = chrono::Utc::now();
            let mut ledger = crate::core::evidence_ledger::EvidenceLedgerV1::load();
            let _ = ledger.record_artifact_file(
                "proof:autonomy-drivers-v1",
                autonomy_path.as_path(),
                ts,
            );
            let _ = ledger.save();
        }

        // Architecture overview proof artifacts (JSON + HTML).
        // Prefer fresh graph when possible (CI/proof reproducibility).
        {
            let status =
                crate::tools::ctx_impact::handle("status", None, project_root, None, Some("json"));
            let freshness = serde_json::from_str::<serde_json::Value>(&status)
                .ok()
                .and_then(|v| {
                    v.get("freshness")
                        .and_then(|f| f.as_str())
                        .map(std::string::ToString::to_string)
                })
                .unwrap_or_else(|| "unknown".to_string());
            if freshness != "fresh" {
                let _ = crate::tools::ctx_impact::handle("build", None, project_root, None, None);
            }

            let arch_json = crate::tools::ctx_architecture::handle(
                "overview",
                None,
                project_root,
                Some("json"),
            );

            let proofs_dir = root.join(".lean-ctx").join("proofs");
            std::fs::create_dir_all(&proofs_dir).map_err(|e| e.to_string())?;

            let json_path = proofs_dir.join(&architecture_json_name);
            let json_redacted = crate::core::redaction::redact_text(&arch_json);
            crate::config_io::write_atomic(&json_path, &json_redacted)?;
            written_architecture_json = Some(json_path.to_string_lossy().to_string());

            let html = render_architecture_overview_html(&arch_json);
            let html_path = proofs_dir.join(&architecture_html_name);
            let html_redacted = crate::core::redaction::redact_text(&html);
            crate::config_io::write_atomic(&html_path, &html_redacted)?;
            written_architecture_html = Some(html_path.to_string_lossy().to_string());

            let ts = chrono::Utc::now();
            let mut ledger = crate::core::evidence_ledger::EvidenceLedgerV1::load();
            let _ = ledger.record_artifact_file(
                "proof:architecture-overview-v1",
                json_path.as_path(),
                ts,
            );
            let _ = ledger.record_artifact_file(
                "proof:architecture-overview-v1-html",
                html_path.as_path(),
                ts,
            );
            let _ = ledger.save();
        }
    }

    match format.unwrap_or("json") {
        "summary" => {
            out.push_str("ContextProofV1 exported\n");
            if let Some(p) = written {
                out.push_str(&format!("path: {p}\n"));
            }
            if let Some(p) = written_ir {
                out.push_str(&format!("context_ir_path: {p}\n"));
            }
            if let Some(p) = written_degradation {
                out.push_str(&format!("degradation_policy_path: {p}\n"));
            }
            if let Some(p) = written_autonomy {
                out.push_str(&format!("autonomy_drivers_path: {p}\n"));
            }
            if let Some(p) = written_architecture_json {
                out.push_str(&format!("architecture_overview_json_path: {p}\n"));
            }
            if let Some(p) = written_architecture_html {
                out.push_str(&format!("architecture_overview_html_path: {p}\n"));
            }
            out.push_str(&format!("schema_version: {}\n", proof.schema_version));
            out.push_str(&format!(
                "project_root_hash: {}\n",
                proof.project.project_root_hash.clone().unwrap_or_default()
            ));
            out.push_str(&format!("role: {}\n", proof.role.name));
            out.push_str(&format!("profile: {}\n", proof.profile.name));
            out.push_str(&format!(
                "context_ir_schema_version: {}\n",
                ir.schema_version
            ));
            out.push_str(&format!("context_ir_items: {}\n", ir.items.len()));
        }
        "both" => {
            if let Some(p) = &written {
                out.push_str(&format!("[proof_path: {p}]\n\n"));
            }
            if let Some(p) = &written_ir {
                out.push_str(&format!("[context_ir_path: {p}]\n\n"));
            }
            if let Some(p) = &written_degradation {
                out.push_str(&format!("[degradation_policy_path: {p}]\n\n"));
            }
            if let Some(p) = &written_autonomy {
                out.push_str(&format!("[autonomy_drivers_path: {p}]\n\n"));
            }
            if let Some(p) = &written_architecture_json {
                out.push_str(&format!("[architecture_overview_json_path: {p}]\n\n"));
            }
            if let Some(p) = &written_architecture_html {
                out.push_str(&format!("[architecture_overview_html_path: {p}]\n\n"));
            }
            out.push_str(&serde_json::to_string_pretty(&proof).map_err(|e| e.to_string())?);
        }
        _ => {
            if let Some(p) = &written {
                out.push_str(&format!("[proof_path: {p}]\n\n"));
            }
            if let Some(p) = &written_ir {
                out.push_str(&format!("[context_ir_path: {p}]\n\n"));
            }
            if let Some(p) = &written_degradation {
                out.push_str(&format!("[degradation_policy_path: {p}]\n\n"));
            }
            if let Some(p) = &written_autonomy {
                out.push_str(&format!("[autonomy_drivers_path: {p}]\n\n"));
            }
            if let Some(p) = &written_architecture_json {
                out.push_str(&format!("[architecture_overview_json_path: {p}]\n\n"));
            }
            if let Some(p) = &written_architecture_html {
                out.push_str(&format!("[architecture_overview_html_path: {p}]\n\n"));
            }
            out.push_str(&serde_json::to_string_pretty(&proof).map_err(|e| e.to_string())?);
        }
    }

    Ok(out)
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn render_architecture_overview_html(json_payload: &str) -> String {
    let escaped = escape_html(json_payload);
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>LeanCTX — Architecture Overview</title>
  <style>
    :root {{ color-scheme: light dark; }}
    body {{ font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, Arial, sans-serif; margin: 24px; }}
    h1 {{ margin: 0 0 12px 0; }}
    p  {{ margin: 0 0 18px 0; opacity: 0.8; }}
    pre {{ padding: 16px; border-radius: 10px; overflow: auto; }}
  </style>
</head>
<body>
  <h1>Architecture Overview (JSON)</h1>
  <p>Generated by LeanCTX proof export. Content is redacted-by-default for CI safety.</p>
  <pre>{escaped}</pre>
</body>
</html>"#
    )
}
