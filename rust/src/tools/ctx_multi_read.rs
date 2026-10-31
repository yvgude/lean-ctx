//! Byte cap shared by batch reads (`ctx_read` with `paths`).

const DEFAULT_MAX_MULTI_READ_BYTES: usize = 512 * 1024;

pub(crate) fn max_multi_read_bytes() -> usize {
    std::env::var("LCTX_MAX_MULTI_READ_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_MULTI_READ_BYTES)
}
