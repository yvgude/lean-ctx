// SPDX-License-Identifier: Apache-2.0
//! Bounded client for an explicitly provisioned local Intelligence session.
//! This module neither admits policy nor discovers, installs or trusts a peer.
//! Callers own fresh scoped keys, sequence allocation, revocation and fallback.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lean_ctx_protocol::runtime_exchange::{RuntimeRequestV1, RuntimeResponseV1};
use lean_ctx_protocol::runtime_frame::{
    RUNTIME_FRAME_HEADER_BYTES, RUNTIME_FRAME_NONCE_BYTES, RUNTIME_FRAME_TAG_BYTES,
    RuntimeFrameDirection, decode_runtime_frame, encode_runtime_frame, runtime_payload_length,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{Instant, timeout_at};

use super::DaemonAddr;

/// Content-free failures: never expose a key, endpoint or projection in logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeExchangeError {
    InvalidRequest,
    InvalidEndpoint,
    EntropyUnavailable,
    DeadlineExceeded,
    ConnectionUnavailable,
    Io,
    InvalidFrame,
    InvalidResponse,
    ClockUnavailable,
}

impl fmt::Display for RuntimeExchangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidRequest => "invalid local runtime request",
            Self::InvalidEndpoint => "invalid local runtime endpoint",
            Self::EntropyUnavailable => "local runtime randomness unavailable",
            Self::DeadlineExceeded => "local runtime deadline exceeded",
            Self::ConnectionUnavailable => "local runtime connection unavailable",
            Self::Io => "local runtime I/O failed",
            Self::InvalidFrame => "invalid authenticated runtime frame",
            Self::InvalidResponse => "invalid local runtime response",
            Self::ClockUnavailable => "local runtime clock unavailable",
        })
    }
}

impl std::error::Error for RuntimeExchangeError {}

/// Exchange one admitted request over a fresh connection to an explicit endpoint.
///
/// The caller must provision and verify the endpoint and peer independently, use
/// a fresh capability-scoped session key, serialize sequence allocation, and retire
/// the session on failure/reconnect. A valid MAC is not policy authorization.
/// No default daemon socket or global HTTP token is used. All connect/write/read
/// work shares ONE monotonic budget; even Windows connection retries are bounded.
/// The stream is dropped on every return. The caller alone persists host receipts
/// and decides whether to use the deterministic public fallback.
pub async fn exchange(
    address: &DaemonAddr,
    key: &[u8; 32],
    request: &RuntimeRequestV1,
) -> Result<RuntimeResponseV1, RuntimeExchangeError> {
    validate_local_address(address)?;
    let (deadline, frame) = prepare_exchange(key, request)?;
    let response = timeout_at(deadline, async {
        let mut stream = super::connect(address)
            .await
            .map_err(|_| RuntimeExchangeError::ConnectionUnavailable)?;
        exchange_connected(&mut stream, key, request, &frame).await
    })
    .await
    .map_err(|_| RuntimeExchangeError::DeadlineExceeded)?;
    finish_exchange(deadline, response)
}

/// Exchange over a caller-protected connected stream, including inherited Unix
/// streams and one-use Windows named pipes. Authentication and deadline checks
/// are identical to the explicit-endpoint exchange path.
pub(crate) async fn exchange_inherited<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    key: &[u8; 32],
    request: &RuntimeRequestV1,
) -> Result<RuntimeResponseV1, RuntimeExchangeError> {
    let (deadline, frame) = prepare_exchange(key, request)?;
    let response = timeout_at(deadline, exchange_connected(stream, key, request, &frame))
        .await
        .map_err(|_| RuntimeExchangeError::DeadlineExceeded)?;
    finish_exchange(deadline, response)
}

fn prepare_exchange(
    key: &[u8; 32],
    request: &RuntimeRequestV1,
) -> Result<(Instant, Vec<u8>), RuntimeExchangeError> {
    let started = Instant::now();
    let now = unix_millis()?;
    request
        .validate_at(now)
        .map_err(|_| RuntimeExchangeError::InvalidRequest)?;
    let deadline = started + Duration::from_millis(request.deadline_unix_ms - now);
    let payload = request
        .to_bytes()
        .map_err(|_| RuntimeExchangeError::InvalidRequest)?;
    let mut nonce = [0; RUNTIME_FRAME_NONCE_BYTES];
    getrandom::fill(&mut nonce).map_err(|_| RuntimeExchangeError::EntropyUnavailable)?;
    let frame = encode_runtime_frame(key, RuntimeFrameDirection::Request, &nonce, &payload)
        .map_err(|_| RuntimeExchangeError::InvalidFrame)?;
    ensure_remaining(deadline)?;
    Ok((deadline, frame))
}

fn finish_exchange(
    deadline: Instant,
    response: Result<RuntimeResponseV1, RuntimeExchangeError>,
) -> Result<RuntimeResponseV1, RuntimeExchangeError> {
    // timeout_at can poll a ready future after its deadline; reject that too.
    ensure_remaining(deadline)?;
    response
}

fn validate_local_address(address: &DaemonAddr) -> Result<(), RuntimeExchangeError> {
    let valid = match address {
        #[cfg(unix)]
        DaemonAddr::Unix(path) => path.is_absolute(),
        #[cfg(windows)]
        DaemonAddr::NamedPipe(name) => name.strip_prefix(r"\\.\pipe\").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix.len() <= 200
                && !suffix
                    .chars()
                    .any(|ch| ch.is_control() || ch == '\\' || ch == '/')
        }),
    };
    if !valid {
        return Err(RuntimeExchangeError::InvalidEndpoint);
    }
    Ok(())
}

async fn exchange_connected<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    key: &[u8; 32],
    request: &RuntimeRequestV1,
    frame: &[u8],
) -> Result<RuntimeResponseV1, RuntimeExchangeError> {
    stream
        .write_all(frame)
        .await
        .map_err(|_| RuntimeExchangeError::Io)?;
    stream.flush().await.map_err(|_| RuntimeExchangeError::Io)?;
    let response_frame = read_frame(stream).await?;
    let payload = decode_runtime_frame(key, RuntimeFrameDirection::Response, &response_frame)
        .map_err(|_| RuntimeExchangeError::InvalidFrame)?;
    RuntimeResponseV1::from_bytes(&payload, request, unix_millis()?)
        .map_err(|_| RuntimeExchangeError::InvalidResponse)
}

async fn read_frame<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>, RuntimeExchangeError> {
    let mut header = [0_u8; RUNTIME_FRAME_HEADER_BYTES];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|_| RuntimeExchangeError::Io)?;
    // Validate magic/version and the length limit before allocation or body reads.
    let length = runtime_payload_length(&header).map_err(|_| RuntimeExchangeError::InvalidFrame)?;
    let mut frame = vec![
        0;
        RUNTIME_FRAME_HEADER_BYTES
            + RUNTIME_FRAME_NONCE_BYTES
            + RUNTIME_FRAME_TAG_BYTES
            + length
    ];
    frame[..RUNTIME_FRAME_HEADER_BYTES].copy_from_slice(&header);
    stream
        .read_exact(&mut frame[RUNTIME_FRAME_HEADER_BYTES..])
        .await
        .map_err(|_| RuntimeExchangeError::Io)?;
    Ok(frame)
}

fn ensure_remaining(deadline: Instant) -> Result<(), RuntimeExchangeError> {
    if Instant::now() >= deadline {
        return Err(RuntimeExchangeError::DeadlineExceeded);
    }
    Ok(())
}

fn unix_millis() -> Result<u64, RuntimeExchangeError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .ok_or(RuntimeExchangeError::ClockUnavailable)
}

#[cfg(test)]
mod tests;
