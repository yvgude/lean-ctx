// SPDX-License-Identifier: Apache-2.0
//! SSE keepalive injection for proxy-to-client streams.
//!
//! When the upstream (e.g. chatgpt.com during extended thinking) goes idle
//! for longer than the keepalive interval, this wrapper injects SSE comment
//! lines (`: keepalive\n\n`) into the downstream stream. SSE comments are
//! ignored by all compliant clients but reset their read-idle timers,
//! preventing "stream disconnected" errors in Codex Desktop and similar
//! consumers.
//!
//! The wrapper is transparent: every upstream byte is forwarded unchanged,
//! and keepalives are only injected during genuine idle gaps.

use std::time::Duration;

use axum::body::Bytes;
use futures::{Stream, StreamExt};

/// Codex Desktop's internal read timeout is ~30s; we ping well before that.
const DEFAULT_INTERVAL_SECS: u64 = 15;

/// SSE comment — ignored by all compliant clients, resets their idle timer.
const KEEPALIVE_BYTES: &[u8] = b": keepalive\n\n";

fn keepalive_interval() -> Duration {
    let secs = std::env::var("LEAN_CTX_PROXY_SSE_KEEPALIVE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_INTERVAL_SECS);
    Duration::from_secs(secs)
}

/// Wraps an upstream SSE byte stream: forwards every upstream chunk unchanged
/// and injects `": keepalive\n\n"` SSE comments during idle gaps so the
/// downstream client's read-idle timer never fires.
///
/// Uses `tokio::time::timeout` per chunk to detect idle periods without
/// requiring `pin_project` or manual `Pin` implementations.
pub fn keepalive_stream<S, E>(inner: S) -> impl Stream<Item = Result<Bytes, E>> + Send + Unpin
where
    S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    E: Send + 'static,
{
    keepalive_stream_with_interval(inner, keepalive_interval())
}

fn keepalive_stream_with_interval<S, E>(
    inner: S,
    interval: Duration,
) -> impl Stream<Item = Result<Bytes, E>> + Send + Unpin
where
    S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    E: Send + 'static,
{
    Box::pin(futures::stream::unfold(
        (inner, interval),
        |(mut inner, interval)| async move {
            match tokio::time::timeout(interval, inner.next()).await {
                Ok(Some(item)) => Some((item, (inner, interval))),
                Ok(None) => None,
                Err(_timeout) => Some((Ok(Bytes::from_static(KEEPALIVE_BYTES)), (inner, interval))),
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keepalive_injected_during_idle() {
        let (mut tx, rx) = futures::channel::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
        let stream = rx.map(|item| item);

        let mut wrapped = keepalive_stream_with_interval(Box::pin(stream), Duration::from_secs(1));

        let item = tokio::time::timeout(Duration::from_secs(3), wrapped.next())
            .await
            .expect("should not timeout waiting for keepalive");

        let bytes = item.expect("stream not ended").expect("no error");
        assert_eq!(bytes.as_ref(), KEEPALIVE_BYTES);

        use futures::SinkExt;
        tx.send(Ok(Bytes::from_static(b"data: hello\n\n")))
            .await
            .unwrap();
        let item = wrapped.next().await.expect("stream not ended");
        assert_eq!(item.unwrap().as_ref(), b"data: hello\n\n");

        drop(tx);
    }

    #[tokio::test]
    async fn no_keepalive_when_data_flows() {
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from_static(b"data: a\n\n")),
            Ok(Bytes::from_static(b"data: b\n\n")),
        ];
        let stream = futures::stream::iter(chunks);
        let mut wrapped = keepalive_stream(Box::pin(stream));

        let a = wrapped.next().await.unwrap().unwrap();
        assert_eq!(a.as_ref(), b"data: a\n\n");
        let b = wrapped.next().await.unwrap().unwrap();
        assert_eq!(b.as_ref(), b"data: b\n\n");
        assert!(wrapped.next().await.is_none());
    }
}
