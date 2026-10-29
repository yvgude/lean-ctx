// SPDX-License-Identifier: Apache-2.0
//! The receipt of one response the Engine writes itself, byte for byte
//! (G08): a tool-session frame, an Engine-interface document.
//!
//! Such a response is a wire document. It is not passed through a text
//! release gate that could mask or rewrite it — its content was admitted at
//! the source seam, inside the call's capture. What is sealed here is the
//! delivery: one capture per call with the policy snapshot of its entry, the
//! same duty and the same seal as an MCP result (receipt, trace sidecar,
//! audit anchor), binding exactly the bytes about to be written. Where the
//! receipt is mandatory and cannot be sealed, the bytes must not be written.

use std::sync::Arc;

use super::{ReceiptSubject, observed, trace};
use crate::core::context_admission::capture::{AdmissionCapture, CallSnapshot, Delivered};

/// One call whose response is still to be written: its admission capture
/// (with the policy snapshot of its entry) and the endpoint it ran.
pub(crate) struct FrameCall {
    capture: Arc<AdmissionCapture>,
    endpoint: String,
}

impl FrameCall {
    /// Open the capture of one call and take its ONE policy snapshot — the
    /// entry of the call. Call inside the request scope when there is one.
    pub(crate) fn open(endpoint: &str) -> Self {
        Self {
            capture: AdmissionCapture::for_call(CallSnapshot::take()),
            endpoint: endpoint.to_owned(),
        }
    }

    /// The capture the call's work runs inside (`capture::ADMISSIONS`).
    pub(crate) fn capture(&self) -> Arc<AdmissionCapture> {
        self.capture.clone()
    }

    /// Build, store and seal the receipt of exactly `bytes` — what is about
    /// to be written — BEFORE it is written. `delivery` names the transport
    /// and channel for the call's endpoint under its snapshot.
    ///
    /// - `Ok(Some(handle))`: sealed; the observation is written through the
    ///   handle after the bytes were handed over.
    /// - `Ok(None)`: no protection and nothing to observe (a receipt for
    ///   admitted sources may still have been stored, as on the MCP path).
    /// - `Err(())`: the receipt is mandatory and missing — `bytes` must not
    ///   be written.
    ///
    /// The same rule as for an MCP result applies to bytes no admission
    /// decided on: under protection they are receipted as exactly that
    /// (`delivery.no_source_decision`); only the closed content-free set
    /// (no bytes, a typed gateway notice) owes nothing.
    pub(crate) fn seal(
        self,
        project_root: &str,
        bytes: &[u8],
        is_error: bool,
        delivery: fn(&str, &CallSnapshot) -> trace::Delivery,
    ) -> Result<Option<observed::Prepared>, ()> {
        let duty = super::Duty::of(&self.capture);
        if self.capture.is_empty() && !duty.mandatory {
            return Ok(None);
        }
        // A wire document is JSON: UTF-8 by construction.
        let text = String::from_utf8_lossy(bytes);
        let delivery = delivery(&self.endpoint, duty.snapshot());
        let presenter_ref = crate::core::archive::presented::shown_presenter_reference();
        let record = super::record_delivery(
            &self.capture,
            &ReceiptSubject {
                tool: &self.endpoint,
                agent_id: None,
                presenter_ref: presenter_ref.as_deref(),
                project_root,
                delivery: &delivery,
            },
            &Delivered {
                text: &text,
                is_error,
                tokens: crate::core::tokens::count_tokens(&text) as u64,
            },
            &duty,
        );
        tracing::debug!("context gateway receipt: {}", record.summary());
        if record.withholds(duty.mandatory) {
            return Err(());
        }
        Ok(record.into_prepared())
    }
}
