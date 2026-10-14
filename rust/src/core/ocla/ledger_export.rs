//! Unified ledger export and offline verification.

use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind};

use serde::{Deserialize, Serialize};

use super::unified_ledger::UnifiedSavingsEventV2;

/// A self-contained export bundle for offline verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerExportBundle {
    pub schema_version: u32,
    pub exported_at: String,
    pub event_count: usize,
    pub hash_chain_valid: bool,
    pub total_saved_tokens: u64,
    pub events: Vec<UnifiedSavingsEventV2>,
}

/// Result of offline verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    pub valid: bool,
    pub event_count: usize,
    pub chain_breaks: Vec<usize>,
    pub total_saved_tokens: u64,
    pub errors: Vec<String>,
}

/// Export the unified ledger as a verifiable bundle.
pub fn export_unified_ledger() -> Result<LedgerExportBundle, String> {
    let events = load_unified_events()?;
    let hash_chain_valid = verify_hash_chain(&events);
    let total_saved_tokens = events.iter().map(|event| event.saved_tokens).sum();

    Ok(LedgerExportBundle {
        schema_version: 2,
        exported_at: chrono::Utc::now().to_rfc3339(),
        event_count: events.len(),
        hash_chain_valid,
        total_saved_tokens,
        events,
    })
}

/// Verify an exported bundle offline without reading a ledger file.
pub fn verify_export_bundle(bundle: &LedgerExportBundle) -> VerificationResult {
    let chain_breaks = bundle
        .events
        .windows(2)
        .enumerate()
        .filter_map(|(index, window)| {
            (window[1].prev_hash != window[0].event_hash).then_some(index + 1)
        })
        .collect::<Vec<_>>();
    let mut errors = Vec::new();

    let total_saved_tokens = bundle.events.iter().map(|event| event.saved_tokens).sum();
    if total_saved_tokens != bundle.total_saved_tokens {
        errors.push(format!(
            "token total mismatch: header={}, computed={}",
            bundle.total_saved_tokens, total_saved_tokens
        ));
    }

    if bundle.events.len() != bundle.event_count {
        errors.push(format!(
            "event count mismatch: header={}, actual={}",
            bundle.event_count,
            bundle.events.len()
        ));
    }

    VerificationResult {
        valid: chain_breaks.is_empty() && errors.is_empty(),
        event_count: bundle.events.len(),
        chain_breaks,
        total_saved_tokens,
        errors,
    }
}

fn load_unified_events() -> Result<Vec<UnifiedSavingsEventV2>, String> {
    let path = crate::core::data_dir::lean_ctx_data_dir()
        .map_err(|error| format!("failed to load unified ledger: {error}"))?
        .join("ledger")
        .join("unified_events.jsonl");
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "failed to load unified ledger {}: {error}",
                path.display()
            ));
        }
    };

    Ok(BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect())
}

fn verify_hash_chain(events: &[UnifiedSavingsEventV2]) -> bool {
    events
        .windows(2)
        .all(|window| window[1].prev_hash == window[0].event_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(prev_hash: &str, event_hash: &str, saved_tokens: u64) -> UnifiedSavingsEventV2 {
        UnifiedSavingsEventV2 {
            tool_name: "ctx_read".to_owned(),
            mode: "auto".to_owned(),
            original_tokens: saved_tokens + 10,
            compressed_tokens: 10,
            saved_tokens,
            content_hash: "content".to_owned(),
            timestamp_epoch_ms: 1,
            prev_hash: prev_hash.to_owned(),
            event_hash: event_hash.to_owned(),
            intent: None,
            outcome: None,
            routing_decision: None,
            agent_id: None,
            efficiency_etpao: None,
            attribution_id: "attribution".to_owned(),
            trace_id: None,
            request_id: None,
            session_id: None,
            quality_ref: None,
        }
    }

    fn bundle(events: Vec<UnifiedSavingsEventV2>) -> LedgerExportBundle {
        LedgerExportBundle {
            schema_version: 2,
            exported_at: "2026-01-01T00:00:00Z".to_owned(),
            event_count: events.len(),
            hash_chain_valid: verify_hash_chain(&events),
            total_saved_tokens: events.iter().map(|event| event.saved_tokens).sum(),
            events,
        }
    }

    #[test]
    fn test_export_empty_ledger() {
        let _data_dir = crate::core::data_dir::isolated_data_dir();

        let exported = export_unified_ledger().expect("empty ledger should export");

        assert_eq!(exported.event_count, 0);
        assert!(exported.events.is_empty(), "{:?}", exported.events);
        assert!(exported.hash_chain_valid);
        assert_eq!(exported.total_saved_tokens, 0);
    }

    #[test]
    fn test_verify_valid_bundle() {
        let exported = bundle(vec![event("", "first", 5), event("first", "second", 7)]);

        let result = verify_export_bundle(&exported);

        assert!(result.valid);
        assert!(result.chain_breaks.is_empty(), "{:?}", result.chain_breaks);
        assert_eq!(result.total_saved_tokens, 12);
    }

    #[test]
    fn test_verify_broken_chain() {
        let exported = bundle(vec![event("", "first", 5), event("tampered", "second", 7)]);

        let result = verify_export_bundle(&exported);

        assert!(!result.valid);
        assert_eq!(result.chain_breaks, vec![1]);
    }

    #[test]
    fn test_verify_token_mismatch() {
        let mut exported = bundle(vec![event("", "first", 5)]);
        exported.total_saved_tokens = 99;

        let result = verify_export_bundle(&exported);

        assert!(!result.valid);
        assert_eq!(
            result.errors[0],
            "token total mismatch: header=99, computed=5"
        );
    }

    #[test]
    fn test_verify_count_mismatch() {
        let mut exported = bundle(vec![event("", "first", 5)]);
        exported.event_count = 2;

        let result = verify_export_bundle(&exported);

        assert!(!result.valid);
        assert_eq!(result.errors[0], "event count mismatch: header=2, actual=1");
    }
}
