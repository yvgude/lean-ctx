// SPDX-License-Identifier: Apache-2.0
//! Release of a tool-session result and its Decision Receipt (G08).
//!
//! A tool-session `call` is a delivery to the host over the session
//! transport, like an MCP tool call: its output passes the release point
//! ([`protect_output`]), and what actually leaves — the response frame, byte
//! for byte with its final `\n` — is receipted the way the MCP dispatcher
//! does it (`server::call_tool::gateway_receipt`): one admission capture per
//! call with the call's policy snapshot taken at entry, the same duty, the
//! same seal (receipt, trace sidecar, audit anchor), and the observation
//! `returned_to_transport` written through the receipt's own handle only
//! after the frame was written and flushed. Where the receipt is mandatory
//! and cannot be sealed, the frame is not written; a protocol-valid error
//! frame takes its place.
//!
//! `hello`, `close` and protocol-error frames carry no tool result and open
//! no capture: they are protocol data and are not receipted.

use std::sync::Arc;

use crate::core::context_admission::capture::AdmissionCapture;
use crate::core::context_admission::receipt_store::{self, frame, observed, trace};
use crate::server::tool_trait::ToolOutput;

use super::{ErrorV1, agent_shell_outcome, output_withheld};

pub(super) fn protect_output(tool: &str, mut output: ToolOutput) -> Result<ToolOutput, ErrorV1> {
    use crate::server::policy_guard;
    use rmcp::model::{CallToolResult, ContentBlock};

    // The project policy pack, when one is active.
    if crate::core::policy::runtime::is_active() {
        if output.mode.as_deref() == Some("error") {
            return Err(output_withheld());
        }
        output.text =
            policy_guard::protect_result(tool, &output.text).map_err(|_| output_withheld())?;
        if let Some(blocks) = output.content_blocks.as_mut() {
            for block in blocks {
                let Some(text) = block.as_text() else {
                    return Err(output_withheld());
                };
                // The SDK has no authorized binary/image inspection path. Text
                // blocks are rebuilt so unchecked parallel metadata cannot escape.
                *block = ContentBlock::text(
                    policy_guard::protect_result(tool, &text.text)
                        .map_err(|_| output_withheld())?,
                );
            }
        }
    }
    // The release point every tool result passes — pack or not: the context
    // gateway runs in it whether or not a pack exists (a governed or sovereign
    // deployment without a pack was not covered here before). The content is
    // released on its own, so that the gateway may deliver a redacted text
    // block as the MCP carrier does; the engine's own metadata is verified
    // separately and never rewritten.
    let blocks = output.content_blocks.as_ref().map_or(0, Vec::len);
    let mut content = output.content_blocks.clone().unwrap_or_default();
    content.push(ContentBlock::text(&output.text));
    let released = policy_guard::release_result(tool, CallToolResult::success(content));
    if released.is_error == Some(true) || released.content.len() != blocks + 1 {
        return Err(output_withheld());
    }
    let mut metadata = CallToolResult::success(Vec::new());
    metadata.structured_content = Some(serde_json::json!({
        "mode": output.mode,
        "shell": agent_shell_outcome(output.shell_outcome.as_ref()),
    }));
    if policy_guard::release_result(tool, metadata).is_error == Some(true) {
        return Err(output_withheld());
    }
    let mut delivered = released.content;
    let Some(text) = delivered
        .pop()
        .and_then(|block| block.as_text().map(|text| text.text.clone()))
    else {
        return Err(output_withheld());
    };
    output.text = text;
    if output.content_blocks.is_some() {
        output.content_blocks = Some(delivered);
    }
    output.saved_tokens = output
        .original_tokens
        .saturating_sub(crate::core::tokens::count_tokens(&output.text));
    Ok(output)
}

/// One `call` of the session whose response frame is still to be written:
/// the shared frame receipt (`receipt_store::frame`) for the session
/// transport.
pub(super) struct FrameCall(frame::FrameCall);

impl FrameCall {
    /// Open the capture of one call and take its ONE policy snapshot — the
    /// entry of the call. Call inside the request scope.
    pub(super) fn open(tool: &str) -> Self {
        Self(frame::FrameCall::open(tool))
    }

    /// The capture the call's handler and release point run inside.
    pub(super) fn capture(&self) -> Arc<AdmissionCapture> {
        self.0.capture()
    }

    /// Seal the receipt of exactly `frame` — the bytes about to be written
    /// to the session transport — BEFORE they are written. `Ok(Some(_))`:
    /// sealed, confirm with [`returned`] after the write. `Ok(None)`:
    /// nothing to observe. `Err(())`: mandatory and missing — `frame` must
    /// not be written. Call inside the request scope of the session's
    /// project.
    pub(super) fn seal(
        self,
        project_root: &str,
        frame: &[u8],
        is_error: bool,
    ) -> Result<Option<observed::Prepared>, ()> {
        self.0
            .seal(project_root, frame, is_error, trace::Delivery::tool_session)
    }
}

/// The frame `written` left through the session transport (`write_all` and
/// `flush` succeeded): record that for the receipt behind `prepared`, if the
/// bytes are exactly the prepared ones. What is observed is the hand-over to
/// the transport; that the host read the frame is not observable.
pub(super) fn returned(prepared: &observed::Prepared, written: &[u8]) {
    prepared.returned_to_transport(&receipt_store::output_digest(written));
}

/// The message of the error frame that stands in for a frame whose mandatory
/// receipt could not be sealed: the typed, content-free gateway notice.
pub(super) fn not_persisted_message() -> String {
    receipt_store::not_persisted_notice()
}
