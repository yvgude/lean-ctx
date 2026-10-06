// SPDX-License-Identifier: Apache-2.0
//! Legacy V1 runtime notification contract, retained for wire compatibility.
//!
//! This one-way message does not authenticate a peer, admit a capability, or
//! establish a request/response session. Transport and admission remain host-owned.

use serde::{Deserialize, Serialize};

pub const HANDSHAKE_SCHEMA: &str = "leanctx.runtime-handshake/v1";
pub const WIRE_PROTOCOL: &str = "leanctx.protocol/v4";
pub const MAX_HANDSHAKE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeHandshake {
    pub schema: String,
    pub wire_protocol: String,
    pub runtime: String,
    pub capabilities: Vec<String>,
}

impl RuntimeHandshake {
    pub fn public(capabilities: impl IntoIterator<Item = String>) -> Self {
        Self {
            schema: HANDSHAKE_SCHEMA.to_owned(),
            wire_protocol: WIRE_PROTOCOL.to_owned(),
            runtime: "public-reference".to_owned(),
            capabilities: capabilities.into_iter().collect(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != HANDSHAKE_SCHEMA {
            return Err(format!("unsupported handshake schema: {}", self.schema));
        }
        if self.wire_protocol != WIRE_PROTOCOL {
            return Err(format!("unsupported wire protocol: {}", self.wire_protocol));
        }
        if self.runtime.trim().is_empty() {
            return Err("runtime identity must not be empty".to_owned());
        }
        Ok(())
    }

    /// Encode the legacy sidecar frame: big-endian length followed by JSON.
    pub fn encode_frame(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let payload = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        if payload.len() > MAX_HANDSHAKE_BYTES {
            return Err("handshake exceeds maximum size".to_owned());
        }
        let length = u32::try_from(payload.len()).map_err(|_| "handshake too large".to_owned())?;
        let mut frame = Vec::with_capacity(4 + payload.len());
        frame.extend_from_slice(&length.to_be_bytes());
        frame.extend_from_slice(&payload);
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_public_frame_preserves_exact_wire_bytes() {
        let value = RuntimeHandshake::public(["reference-planning".to_owned()]);
        let payload = br#"{"schema":"leanctx.runtime-handshake/v1","wire_protocol":"leanctx.protocol/v4","runtime":"public-reference","capabilities":["reference-planning"]}"#;
        let mut expected = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
        expected.extend_from_slice(payload);
        assert_eq!(value.encode_frame().unwrap(), expected);
        assert_eq!(
            serde_json::from_slice::<RuntimeHandshake>(payload).unwrap(),
            value
        );
    }

    #[test]
    fn legacy_validation_rejects_wrong_versions_and_empty_identity() {
        for (field, bad) in [
            ("schema", "leanctx.runtime-handshake/v2"),
            ("wire_protocol", "leanctx.protocol/v3"),
            ("runtime", "  "),
        ] {
            let mut json = serde_json::to_value(RuntimeHandshake::public([])).unwrap();
            json[field] = serde_json::Value::String(bad.to_owned());
            let value: RuntimeHandshake = serde_json::from_value(json).unwrap();
            assert!(value.validate().is_err(), "{field}");
            assert!(value.encode_frame().is_err(), "{field}");
        }
    }

    #[test]
    fn legacy_size_bound_is_on_encoded_payload_not_capability_count() {
        let value = RuntimeHandshake::public(["x".repeat(MAX_HANDSHAKE_BYTES)]);
        assert_eq!(
            value.encode_frame().unwrap_err(),
            "handshake exceeds maximum size"
        );
    }
}
