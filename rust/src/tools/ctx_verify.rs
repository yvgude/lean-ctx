pub fn handle_proof(format: Option<&str>) -> Result<String, String> {
    let session = crate::core::session::SessionState::load_latest();
    let run_id = session
        .as_ref()
        .map_or_else(|| "anonymous".to_string(), |s| s.id.clone());
    let session_id = session.as_ref().map(|s| s.id.clone());

    let mut extractor =
        crate::core::claim_extractor::ClaimExtractor::new(&run_id, session_id.as_deref());

    if let Some(ref sess) = session {
        let jail_root = sess.project_root.as_ref().map_or_else(
            || std::env::current_dir().unwrap_or_default(),
            std::path::PathBuf::from,
        );
        for ft in &sess.files_touched {
            extractor.verify_pathjail(&ft.path, &jail_root);
        }
    }

    extractor.verify_budget_compliance();

    extractor.add_lean_proof(
        "pathjail_no_escape",
        "PathJail prevents directory traversal outside root",
        crate::core::context_proof_v2::ClaimKind::PathjailCompliance,
        "LeanCtxProofs.Policy.PathJail.jail_no_escape",
    );
    extractor.add_lean_proof(
        "budget_monotonic",
        "Budget consumption is monotonically increasing",
        crate::core::context_proof_v2::ClaimKind::BudgetCompliance,
        "LeanCtxProofs.Policy.BudgetEnforcement.spend_monotonic",
    );
    extractor.add_lean_proof(
        "terse_quality_gate",
        "Quality gate preserves paths and identifiers",
        crate::core::context_proof_v2::ClaimKind::CompressionInvariant,
        "LeanCtxProofs.Compression.TerseQuality.both_ok_passes",
    );
    extractor.add_lean_proof(
        "terse_filter_subset",
        "Terse filtering produces a subset of input",
        crate::core::context_proof_v2::ClaimKind::CompressionInvariant,
        "LeanCtxProofs.Compression.TerseEngine.filter_subset",
    );

    let proof = extractor.finalize();

    match format.unwrap_or("json") {
        "summary" => {
            let s = &proof.summary;
            Ok(format!(
                "ContextProofV2 · {} claims · Q{} ({:?})\n  proved: {} · passed: {} · failed: {} · skipped: {}",
                s.total_claims,
                proof.quality_level as u8,
                proof.quality_level,
                s.proved,
                s.passed,
                s.failed,
                s.skipped,
            ))
        }
        _ => serde_json::to_string_pretty(&proof).map_err(|e| e.to_string()),
    }
}

pub fn handle_stats(format: Option<&str>) -> Result<String, String> {
    let snap = crate::core::verification_observability::snapshot_v1();
    match format.unwrap_or("summary") {
        "json" => Ok(serde_json::to_string_pretty(&snap).map_err(|e| e.to_string())?),
        "both" => Ok(format!(
            "{}\n\n{}",
            crate::core::verification_observability::format_compact(&snap),
            serde_json::to_string_pretty(&snap).map_err(|e| e.to_string())?
        )),
        _ => Ok(crate::core::verification_observability::format_compact(
            &snap,
        )),
    }
}
