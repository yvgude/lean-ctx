// SPDX-License-Identifier: Apache-2.0
//! Legacy one-way notification adapter; not an authenticated runtime session.

pub use lean_ctx_protocol::runtime_handshake::{
    HANDSHAKE_SCHEMA, MAX_HANDSHAKE_BYTES, RuntimeHandshake, WIRE_PROTOCOL,
};

/// Best-effort notification to the configured private runtime.
#[cfg(unix)]
pub fn notify_configured_runtime() -> Result<(), String> {
    use std::{io::Write, os::unix::net::UnixStream, path::Path, time::Duration};
    let Some(raw) = std::env::var_os("LEANCTX_ENTERPRISE_RUNTIME_SOCKET") else {
        return Ok(());
    };
    let path = Path::new(&raw);
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().len() > 104 {
        return Err("enterprise runtime socket path is unsafe".to_owned());
    }
    let mut stream = UnixStream::connect(path).map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| error.to_string())?;
    stream
        .write_all(&RuntimeHandshake::public(["reference-planning".to_owned()]).encode_frame()?)
        .map_err(|error| error.to_string())
}

#[cfg(not(unix))]
pub fn notify_configured_runtime() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_handshake_matches_private_wire_contract() {
        let value = RuntimeHandshake::public(["reference-planning".to_owned()]);
        let encoded = serde_json::to_vec(&value).unwrap();
        let decoded: RuntimeHandshake = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, value);
        decoded.validate().unwrap();
    }
    #[test]
    fn incompatible_wire_is_rejected() {
        let mut value = RuntimeHandshake::public([]);
        value.wire_protocol = "leanctx.protocol/v3".to_owned();
        assert!(value.validate().is_err());
    }

    #[test]
    fn frame_has_bounded_big_endian_length_prefix() {
        let frame = RuntimeHandshake::public(["reference-planning".to_owned()])
            .encode_frame()
            .unwrap();
        let length = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(length, frame.len() - 4);
        assert!(length <= MAX_HANDSHAKE_BYTES);
    }
}
