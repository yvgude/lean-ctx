pub fn handle_proof(format: Option<&str>) -> Result<String, String> {
    // The caller's own session: a proof never covers another agent's (I07).
    let session = crate::core::session::caller::with(Clone::clone);
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

    let proof = extractor.finalize();

    match format.unwrap_or("json") {
        "summary" => {
            let s = &proof.summary;
            Ok(format!(
                "ContextProofV2 · {} claims · Q{} ({:?})\n  passed: {} · failed: {} · skipped: {}",
                s.total_claims,
                proof.quality_level as u8,
                proof.quality_level,
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

#[cfg(test)]
mod tests {
    use super::handle_proof;

    #[test]
    fn proof_reports_only_claims_checked_at_runtime() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let json = handle_proof(Some("json")).unwrap();
        let proof: serde_json::Value = serde_json::from_str(&json).unwrap();
        for claim in proof["claims"].as_array().unwrap() {
            assert_eq!(claim["verifier"], "path_policy", "{claim}");
        }
        for unchecked in ["proved", "lean_theorem", "lean_axioms", "formally_verified"] {
            assert!(!json.contains(unchecked), "{unchecked} in {json}");
        }
    }
}
