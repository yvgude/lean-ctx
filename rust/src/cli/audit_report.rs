use std::collections::HashMap;

use crate::core::audit_trail::{AuditEntry, AuditEventType};

/// `lean-ctx audit evidence --from <rfc3339> --to <rfc3339>
/// [--framework <id>] [--pack <name|path>] [--out <file>]` —
/// deterministic, offline-verifiable evidence bundle (GL #425,
/// `docs/contracts/evidence-bundle-v1.md`; verifier:
/// `packages/leanctx-verify`).
pub fn cmd_evidence(args: &[String]) {
    let flag = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|pos| args.get(pos + 1).cloned())
    };
    let (Some(from), Some(to)) = (flag("--from"), flag("--to")) else {
        eprintln!(
            "audit evidence: --from and --to (RFC 3339) are required\n\n\
USAGE:\n  lean-ctx audit evidence --from 2026-05-01T00:00:00Z --to 2026-06-01T00:00:00Z \\\n\
      [--framework eu-ai-act|iso42001|soc2] [--pack <name|path>] [--out bundle.zip]\n\n\
Verify without LeanCTX: leanctx-verify <bundle.zip> [--pubkey <hex>]"
        );
        std::process::exit(2);
    };

    let spec = crate::core::evidence_bundle::BundleSpec {
        from,
        to,
        framework: flag("--framework"),
        pack: flag("--pack"),
        out: flag("--out").map(std::path::PathBuf::from),
    };
    match crate::core::evidence_bundle::generate(&spec) {
        Ok(result) => {
            println!("evidence bundle written: {}", result.path.display());
            println!("bundle sha256: {}", result.sha256);
            println!("audit entries: {}", result.entries);
            for f in &result.files {
                println!("  {f}");
            }
            println!(
                "\nverify offline (no LeanCTX needed):\n  leanctx-verify {}",
                result.path.display()
            );
        }
        Err(e) => {
            eprintln!("audit evidence: {e}");
            std::process::exit(1);
        }
    }
}

#[must_use]
pub fn generate_report() -> String {
    let entries = crate::core::audit_trail::load_recent(10000);
    let chain = crate::core::audit_trail::verify_chain();

    let mut report = String::new();
    report.push_str("# lean-ctx Compliance Report\n\n");
    report.push_str(&format!("Generated: {}\n", chrono::Utc::now().to_rfc3339()));
    report.push_str(&format!("Audit Trail Entries: {}\n", entries.len()));
    report.push_str(&format!(
        "Chain Integrity: {}\n\n",
        if chain.valid { "VALID" } else { "BROKEN" }
    ));

    let mut by_agent: HashMap<String, Vec<&AuditEntry>> = HashMap::new();
    for e in &entries {
        by_agent.entry(e.agent_id.clone()).or_default().push(e);
    }

    report.push_str("## Per-Agent Summary\n\n");
    for (agent, agent_entries) in &by_agent {
        let tool_calls = agent_entries
            .iter()
            .filter(|e| matches!(e.event_type, AuditEventType::ToolCall))
            .count();
        let denials = agent_entries
            .iter()
            .filter(|e| matches!(e.event_type, AuditEventType::ToolDenied))
            .count();
        report.push_str(&format!("### Agent: {agent}\n"));
        report.push_str(&format!("- Tool calls: {tool_calls}\n"));
        report.push_str(&format!("- Denials: {denials}\n\n"));
    }

    let security_events: Vec<_> = entries
        .iter()
        .filter(|e| !matches!(e.event_type, AuditEventType::ToolCall))
        .collect();
    report.push_str(&format!(
        "## Security Events ({} total)\n\n",
        security_events.len()
    ));
    for e in security_events.iter().take(50) {
        report.push_str(&format!(
            "- [{}] {:?} tool={} agent={}\n",
            e.timestamp, e.event_type, e.tool, e.agent_id
        ));
    }

    report.push_str("\n\n");
    report.push_str(&crate::core::owasp_alignment::summary());

    report
}
