use axum::http::StatusCode;
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use std::io::{Read, Write};

pub(super) fn decode_zstd_bounded(data: &[u8], max_bytes: usize) -> Result<Vec<u8>, StatusCode> {
    let decoder = zstd::Decoder::new(data).map_err(|e| {
        tracing::warn!("lean-ctx proxy: invalid zstd request body: {}", e.kind());
        StatusCode::BAD_REQUEST
    })?;
    read_bounded(decoder, max_bytes).inspect_err(|e| {
        tracing::warn!("lean-ctx proxy: zstd request decode failed: {e}");
    })
}

pub(super) fn encode_zstd(data: &[u8]) -> Result<Vec<u8>, StatusCode> {
    zstd::encode_all(data, 3).map_err(|e| {
        tracing::error!("lean-ctx proxy: zstd request encode failed: {}", e.kind());
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

pub(super) fn decode_gzip_bounded(data: &[u8], max_bytes: usize) -> Result<Vec<u8>, StatusCode> {
    read_bounded(GzDecoder::new(data), max_bytes).inspect_err(|e| {
        tracing::warn!("lean-ctx proxy: gzip request decode failed: {e}");
    })
}

pub(super) fn encode_gzip(data: &[u8]) -> Result<Vec<u8>, StatusCode> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).map_err(|e| {
        tracing::error!("lean-ctx proxy: gzip request encode failed: {}", e.kind());
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    encoder.finish().map_err(|e| {
        tracing::error!("lean-ctx proxy: gzip request encode failed: {}", e.kind());
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

pub(super) fn read_bounded<R: Read>(reader: R, max_bytes: usize) -> Result<Vec<u8>, StatusCode> {
    let mut limited = reader.take(max_bytes as u64 + 1);
    let mut out = Vec::new();
    limited
        .read_to_end(&mut out)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if out.len() > max_bytes {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RequestBodyEncoding {
    Identity,
    Gzip,
    Zstd,
    Passthrough,
}

pub(super) fn request_body_encoding(parts: &axum::http::request::Parts) -> RequestBodyEncoding {
    let Some(value) = parts
        .headers
        .get(axum::http::header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
    else {
        return RequestBodyEncoding::Identity;
    };

    let encodings = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty() && !part.eq_ignore_ascii_case("identity"))
        .collect::<Vec<_>>();
    match encodings.as_slice() {
        [] => RequestBodyEncoding::Identity,
        [encoding] if encoding.eq_ignore_ascii_case("gzip") => RequestBodyEncoding::Gzip,
        [encoding] if encoding.eq_ignore_ascii_case("zstd") => RequestBodyEncoding::Zstd,
        _ => RequestBodyEncoding::Passthrough,
    }
}

pub(super) fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503)
}

pub(super) async fn retry_backoff() {
    let mut buf = [0u8; 2];
    let jitter_ms =
        getrandom::fill(&mut buf).map_or(100, |()| u64::from(u16::from_le_bytes(buf)) % 200);
    tokio::time::sleep(std::time::Duration::from_millis(150 + jitter_ms)).await;
}

/// True when the upstream base URL points at a loopback/local endpoint (an
/// Ollama/vLLM-style local model): billed with the transparent
/// `local_shadow_rate` instead of provider list prices (enterprise#15/#18).
pub(super) fn upstream_is_local(upstream_base: &str) -> bool {
    let rest = upstream_base
        .strip_prefix("https://")
        .or_else(|| upstream_base.strip_prefix("http://"))
        .unwrap_or(upstream_base);
    let host_port = rest.split(['/', '?']).next().unwrap_or(rest);
    let host = if let Some(b) = host_port.strip_prefix('[') {
        b.split(']').next().unwrap_or(b)
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "0.0.0.0")
}

/// Constructs the full upstream URL from the request path (or default).
pub(super) fn build_upstream_url(
    parts: &axum::http::request::Parts,
    base: &str,
    default_path: &str,
) -> String {
    format!(
        "{base}{}",
        parts
            .uri
            .path_and_query()
            .map_or(default_path, axum::http::uri::PathAndQuery::as_str)
    )
}
