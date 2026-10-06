// SPDX-License-Identifier: Apache-2.0
//! Encrypted, authenticated local-runtime frames; admission/replay are host-owned.
//! Owners provision a fresh scoped 32-byte key and a fresh random 24-byte nonce
//! for EVERY frame. Never reuse a nonce with the same key, including on retries.

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use std::fmt;

const MAGIC: &[u8; 8] = b"LCTXIR02";
/// Magic/version and big-endian plaintext length, read before any allocation.
pub const RUNTIME_FRAME_HEADER_BYTES: usize = 12;
/// Extended nonce; generated with a cryptographic random source by the sender.
pub const RUNTIME_FRAME_NONCE_BYTES: usize = 24;
/// Full Poly1305 tag, appended to the ciphertext. Truncation is not accepted.
pub const RUNTIME_FRAME_TAG_BYTES: usize = 16;
/// Maximum plaintext bytes, excluding header, nonce and authentication tag.
pub const MAX_RUNTIME_FRAME_PAYLOAD_BYTES: usize =
    crate::runtime_exchange::MAX_RUNTIME_EXCHANGE_BYTES;

/// Authenticated associated data separates request and response domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFrameDirection {
    Request,
    Response,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFrameError {
    InvalidHeader,
    InvalidLength,
    EncryptionFailed,
    AuthenticationFailed,
}

impl fmt::Display for RuntimeFrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidHeader => "invalid runtime frame header or version",
            Self::InvalidLength => "invalid runtime frame length",
            Self::EncryptionFailed => "runtime frame encryption failed",
            Self::AuthenticationFailed => "runtime frame authentication failed",
        })
    }
}

impl std::error::Error for RuntimeFrameError {}

/// Check the fixed header BEFORE allocating/reading the declared ciphertext.
pub fn runtime_payload_length(header: &[u8]) -> Result<usize, RuntimeFrameError> {
    let length_bytes: [u8; 4] = header
        .strip_prefix(MAGIC)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(RuntimeFrameError::InvalidHeader)?;
    let length = usize::try_from(u32::from_be_bytes(length_bytes))
        .map_err(|_| RuntimeFrameError::InvalidLength)?;
    validate_length(length)?;
    Ok(length)
}

/// Encode version/length, random nonce, ciphertext and its complete tag.
/// Nonce MUST be fresh cryptographic randomness for every call under this key.
pub fn encode_runtime_frame(
    key: &[u8; 32],
    direction: RuntimeFrameDirection,
    nonce: &[u8; RUNTIME_FRAME_NONCE_BYTES],
    payload: &[u8],
) -> Result<Vec<u8>, RuntimeFrameError> {
    let aad = associated_data(direction, payload.len())?;
    let ciphertext = XChaCha20Poly1305::new(key.into())
        .encrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: payload,
                aad: &aad,
            },
        )
        .map_err(|_| RuntimeFrameError::EncryptionFailed)?;
    let mut frame = Vec::with_capacity(
        RUNTIME_FRAME_HEADER_BYTES + RUNTIME_FRAME_NONCE_BYTES + ciphertext.len(),
    );
    frame.extend_from_slice(&aad[..RUNTIME_FRAME_HEADER_BYTES]);
    frame.extend_from_slice(nonce);
    frame.extend_from_slice(&ciphertext);
    Ok(frame)
}

/// Authenticate and decrypt before JSON decoding or dispatch. A wrong peer or
/// active relay sees ciphertext, not the projection. This does not admit a
/// capability or prevent replay; those require trusted policy and session state.
pub fn decode_runtime_frame(
    key: &[u8; 32],
    direction: RuntimeFrameDirection,
    frame: &[u8],
) -> Result<Vec<u8>, RuntimeFrameError> {
    let overhead = RUNTIME_FRAME_HEADER_BYTES + RUNTIME_FRAME_NONCE_BYTES + RUNTIME_FRAME_TAG_BYTES;
    if frame.len() < overhead {
        return Err(RuntimeFrameError::InvalidLength);
    }
    let (header, body) = frame.split_at(RUNTIME_FRAME_HEADER_BYTES);
    let length = runtime_payload_length(header)?;
    if frame.len() != overhead + length {
        return Err(RuntimeFrameError::InvalidLength);
    }
    let (nonce, ciphertext) = body.split_at(RUNTIME_FRAME_NONCE_BYTES);
    let aad = associated_data(direction, length)?;
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| RuntimeFrameError::AuthenticationFailed)
}

fn validate_length(length: usize) -> Result<(), RuntimeFrameError> {
    if length == 0 || length > MAX_RUNTIME_FRAME_PAYLOAD_BYTES {
        return Err(RuntimeFrameError::InvalidLength);
    }
    Ok(())
}

fn associated_data(
    direction: RuntimeFrameDirection,
    length: usize,
) -> Result<Vec<u8>, RuntimeFrameError> {
    validate_length(length)?;
    let length = u32::try_from(length).map_err(|_| RuntimeFrameError::InvalidLength)?;
    let mut aad = MAGIC.to_vec();
    aad.extend_from_slice(&length.to_be_bytes());
    aad.extend_from_slice(match direction {
        RuntimeFrameDirection::Request => b"request\0",
        RuntimeFrameDirection::Response => b"response\0",
    });
    Ok(aad)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Public synthetic vectors only. Production senders generate fresh randomness.
    const KEY: [u8; 32] = [0x0b; 32];
    const NONCE: [u8; 24] = [0x0d; 24];

    #[test]
    fn round_trip_authenticates_every_byte_and_direction() {
        let request = RuntimeFrameDirection::Request;
        let payload = b"bounded projection";
        let frame = encode_runtime_frame(&KEY, request, &NONCE, payload).unwrap();
        assert_eq!(runtime_payload_length(&frame[..12]).unwrap(), payload.len());
        assert_eq!(
            decode_runtime_frame(&KEY, request, &frame).unwrap(),
            payload
        );
        assert!(!frame.windows(payload.len()).any(|window| window == payload));
        assert_ne!(
            frame,
            encode_runtime_frame(&KEY, request, &[0x0e; 24], payload).unwrap()
        );
        assert!(decode_runtime_frame(&KEY, RuntimeFrameDirection::Response, &frame).is_err());
        assert!(decode_runtime_frame(&[0x0c; 32], request, &frame).is_err());
        for index in 0..frame.len() {
            let mut tampered = frame.clone();
            tampered[index] ^= 1;
            assert!(
                decode_runtime_frame(&KEY, request, &tampered).is_err(),
                "byte {index}"
            );
        }
        for length in 0..frame.len() {
            assert!(decode_runtime_frame(&KEY, request, &frame[..length]).is_err());
        }
        let mut trailing = frame;
        trailing.push(0);
        assert!(decode_runtime_frame(&KEY, request, &trailing).is_err());
    }

    #[test]
    fn unpublished_plaintext_hmac_predecessor_is_not_a_downgrade_path() {
        // Keep the independently calculated old HMAC vector as a rejection test.
        let mut old = b"LCTXIR01".to_vec();
        old.extend_from_slice(&18_u32.to_be_bytes());
        old.extend_from_slice(&[
            0x22, 0x7a, 0xca, 0x2b, 0x41, 0x04, 0xc8, 0x81, 0xa1, 0xd8, 0x67, 0xfa, 0x3d, 0x9b,
            0x8d, 0xce, 0xdf, 0xa4, 0x8d, 0x1b, 0x61, 0x28, 0x6f, 0xd2, 0xe8, 0x1c, 0xc9, 0x2b,
            0x3a, 0x92, 0x18, 0x2c,
        ]);
        old.extend_from_slice(b"bounded projection");
        assert!(decode_runtime_frame(&KEY, RuntimeFrameDirection::Request, &old).is_err());
    }

    #[test]
    fn validates_header_before_allocation_and_rejects_oversize() {
        for length in [0_u32, 1_048_577, u32::MAX] {
            let mut header = MAGIC.to_vec();
            header.extend_from_slice(&length.to_be_bytes());
            assert!(runtime_payload_length(&header).is_err());
        }
        assert!(runtime_payload_length(b"LCTXIR02").is_err());
        let direction = RuntimeFrameDirection::Response;
        assert!(encode_runtime_frame(&KEY, direction, &NONCE, b"").is_err());
        assert!(encode_runtime_frame(&KEY, direction, &NONCE, &vec![0; 1_048_577]).is_err());
        let payload = vec![0; MAX_RUNTIME_FRAME_PAYLOAD_BYTES];
        let frame = encode_runtime_frame(&KEY, direction, &NONCE, &payload).unwrap();
        assert_eq!(
            decode_runtime_frame(&KEY, direction, &frame).unwrap(),
            payload
        );
    }
}
