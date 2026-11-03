// SPDX-License-Identifier: Apache-2.0
//! Where a persisted Engine receipt is handed on. The Hive connector
//! registers a sink that queues the receipt for upload when receipt uploads
//! are on; without a registration a receipt stays local only.

use std::path::Path;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use lean_ctx_protocol::{EngineInvocationV1, EngineObservationV1};

/// Notes one persisted receipt: `(root, receipt_id, invocation,
/// observation, now)`. Cheap and silent: it never fails the Engine call.
pub type ReceiptSink = fn(&Path, &str, &EngineInvocationV1, &EngineObservationV1, DateTime<Utc>);

static SINK: OnceLock<ReceiptSink> = OnceLock::new();

/// Register the receipt sink. The first registration wins.
pub fn register_receipt_sink(sink: ReceiptSink) {
    let _ = SINK.set(sink);
}

/// Hand a persisted receipt to the registered sink, if any.
pub(crate) fn note_receipt(
    root: &Path,
    receipt_id: &str,
    invocation: &EngineInvocationV1,
    observation: &EngineObservationV1,
    now: DateTime<Utc>,
) {
    if let Some(sink) = SINK.get() {
        sink(root, receipt_id, invocation, observation, now);
    }
}
