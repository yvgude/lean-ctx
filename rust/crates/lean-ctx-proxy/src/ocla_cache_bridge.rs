// SPDX-License-Identifier: Apache-2.0
//! Adapter between proxy request fields and the OCLA response cache.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;

use lean_ctx::core::ocla::response_cache::{CachedResponse, ResponseCache, ResponseCacheKey};

/// Shared proxy adapter for the OCLA response cache.
#[derive(Clone, Debug)]
pub struct OclaCacheBridge {
    cache: Arc<ResponseCache>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedProviderResponse {
    pub body: Vec<u8>,
    pub status: StatusCode,
}

impl OclaCacheBridge {
    /// Creates a bridge backed by a shared OCLA response cache.
    pub fn new(cache: Arc<ResponseCache>) -> Self {
        Self { cache }
    }

    /// Returns a cached response body for matching request fields.
    pub fn try_cache_hit(
        &self,
        model: &str,
        prompt_hash: &str,
        temp: f32,
        max_tokens: u32,
    ) -> Option<CachedProviderResponse> {
        self.cache
            .get(&cache_key(model, prompt_hash, temp, max_tokens))
            .map(|response| CachedProviderResponse {
                body: response.body,
                status: StatusCode::from_u16(response.status).unwrap_or(StatusCode::OK),
            })
    }

    /// Records a response for matching request fields.
    pub fn record_response(
        &self,
        model: &str,
        prompt_hash: &str,
        temp: f32,
        max_tokens: u32,
        status: StatusCode,
        body: &[u8],
        tokens: u64,
    ) {
        self.cache.put(
            cache_key(model, prompt_hash, temp, max_tokens),
            CachedResponse {
                body: body.to_vec(),
                status: status.as_u16(),
                tokens,
                created_at: Instant::now(),
                ttl: Duration::ZERO,
            },
        );
    }
}

/// Computes the stable blake3 hash used by the bridge for a request body.
pub fn prompt_hash(request_body: &[u8]) -> String {
    blake3::hash(request_body).to_hex().to_string()
}

fn cache_key(model: &str, prompt_hash: &str, temp: f32, max_tokens: u32) -> ResponseCacheKey {
    let prompt_digest = blake3::hash(prompt_hash.as_bytes());
    let mut hash_bytes = [0; 8];
    hash_bytes.copy_from_slice(&prompt_digest.as_bytes()[..8]);
    ResponseCacheKey::new(
        model,
        u64::from_be_bytes(hash_bytes),
        temp,
        u64::from(max_tokens),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_hash_is_stable_and_content_sensitive() {
        assert_eq!(prompt_hash(b"prompt"), prompt_hash(b"prompt"));
        assert_ne!(prompt_hash(b"prompt"), prompt_hash(b"different"));
    }

    #[test]
    fn bridge_serves_recorded_response_and_misses_other_requests() {
        let bridge = OclaCacheBridge::new(Arc::new(ResponseCache::new(4, Duration::from_mins(1))));
        let hash = prompt_hash(br#"{"prompt":"hello"}"#);

        assert!(bridge.try_cache_hit("model", &hash, 0.2, 128).is_none());
        bridge.record_response(
            "model",
            &hash,
            0.2,
            128,
            StatusCode::INTERNAL_SERVER_ERROR,
            b"answer",
            7,
        );

        assert_eq!(
            bridge.try_cache_hit("model", &hash, 0.2, 128),
            Some(CachedProviderResponse {
                body: b"answer".to_vec(),
                status: StatusCode::INTERNAL_SERVER_ERROR,
            })
        );
        assert!(bridge.try_cache_hit("model", &hash, 0.3, 128).is_none());
        assert!(
            bridge
                .try_cache_hit("other-model", &hash, 0.2, 128)
                .is_none()
        );
    }
}
