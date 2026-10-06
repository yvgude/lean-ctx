use lean_ctx_protocol::{
    EvidenceKind, EvidenceRefV1, MoneyV1, SavingsObservationV1, SignatureStatus, UsageBreakdownV1,
};

/// Converts internal savings data into a protocol-compliant `SavingsObservationV1`.
/// This is what the OSS runtime emits to the Sidecar.
#[allow(clippy::too_many_arguments)]
pub fn build_observation(
    observation_id: &str,
    provider_id: &str,
    model_id: &str,
    original_input_tokens: u64,
    original_output_tokens: u64,
    actual_input_tokens: u64,
    actual_output_tokens: u64,
    cost_estimate_microdollars: i128,
    sequence_number: u64,
) -> SavingsObservationV1 {
    SavingsObservationV1 {
        observation_id: observation_id.to_owned(),
        provider_id: provider_id.to_owned(),
        model_id: model_id.to_owned(),
        original_usage: UsageBreakdownV1 {
            input_tokens: original_input_tokens,
            output_tokens: original_output_tokens,
            cached_input_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            other_units: Vec::new(),
        },
        actual_usage: UsageBreakdownV1 {
            input_tokens: actual_input_tokens,
            output_tokens: actual_output_tokens,
            cached_input_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            other_units: Vec::new(),
        },
        local_cost_estimate: MoneyV1 {
            currency: "USD".to_owned(),
            coefficient: cost_estimate_microdollars,
            scale: 6,
        },
        evidence_refs: Vec::new(),
        observed_at: chrono::Utc::now().to_rfc3339(),
        runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
        sequence_number,
    }
}

/// Adds an evidence reference to an observation.
pub fn add_evidence_ref(
    observation: &mut SavingsObservationV1,
    kind: EvidenceKind,
    uri: &str,
    digest: &str,
) {
    observation.evidence_refs.push(EvidenceRefV1 {
        schema_version: Some(1),
        kind,
        uri: uri.to_owned(),
        digest: digest.to_owned(),
        signature_status: SignatureStatus::NotSigned,
        media_type: None,
        extensions: Default::default(),
    });
}

#[cfg(test)]
mod tests {
    use lean_ctx_protocol::{EvidenceKind, SavingsObservationV1, SignatureStatus};

    use super::{add_evidence_ref, build_observation};

    fn observation() -> SavingsObservationV1 {
        build_observation("obs-1", "openai", "gpt-5", 1_000, 200, 400, 100, 1_234, 7)
    }

    #[test]
    fn build_observation_sets_protocol_fields() {
        let observation = observation();

        assert_eq!(observation.observation_id, "obs-1");
        assert_eq!(observation.provider_id, "openai");
        assert_eq!(observation.model_id, "gpt-5");
        assert_eq!(observation.original_usage.input_tokens, 1_000);
        assert_eq!(observation.original_usage.output_tokens, 200);
        assert_eq!(observation.actual_usage.input_tokens, 400);
        assert_eq!(observation.actual_usage.output_tokens, 100);
        assert_eq!(observation.local_cost_estimate.currency, "USD");
        assert_eq!(observation.local_cost_estimate.coefficient, 1_234);
        assert_eq!(observation.local_cost_estimate.scale, 6);
        assert!(observation.evidence_refs.is_empty());
        assert_eq!(observation.runtime_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(observation.sequence_number, 7);
        assert!(chrono::DateTime::parse_from_rfc3339(&observation.observed_at).is_ok());
    }

    #[test]
    fn add_evidence_ref_appends_unsigned_reference() {
        let mut observation = observation();

        add_evidence_ref(
            &mut observation,
            EvidenceKind::RuntimeLog,
            "file:///ledger.jsonl",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );

        assert_eq!(observation.evidence_refs.len(), 1);
        let evidence = &observation.evidence_refs[0];
        assert_eq!(evidence.kind, EvidenceKind::RuntimeLog);
        assert_eq!(evidence.uri, "file:///ledger.jsonl");
        assert_eq!(
            evidence.digest,
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(evidence.signature_status, SignatureStatus::NotSigned);
    }

    #[test]
    fn serialization_round_trip_preserves_observation() {
        let observation = observation();
        let json = serde_json::to_string(&observation).expect("observation should serialize");
        let decoded: SavingsObservationV1 =
            serde_json::from_str(&json).expect("observation should deserialize");

        assert_eq!(decoded, observation);
    }
}
