// SPDX-License-Identifier: Apache-2.0
//! Admission of a hook payload, decided once on the ORIGINAL payload and
//! before anything is derived from it (G01).
//!
//! A host hands `hook observe` the full native tool input and output. Every
//! text-bearing derivation — solution decisions, findings, the radar event's
//! `detail` and `content`, echo analysis — depends on this one decision and
//! inherits it:
//!
//! - withheld original → nothing text-bearing is derived or persisted, only
//!   the typed reason;
//! - redacted original → derivations see the redacted payload only;
//! - a payload the detectors could not fully inspect, or a detector failure:
//!   withheld in governed and sovereign mode, delivered with the mode's note
//!   in developer mode (the gateway's own rule).
//!
//! Content-free facts (event type, tool name, sizes, token estimates, LOC
//! counts, hashes, session ids) never depend on it: the hook keeps working
//! for native editing and metrics when the content is withheld.
//!
//! Provenance: a hook payload is supplied by the host. There is no source
//! lean-ctx read and could re-authorize later, so what is derived from it is
//! "caller-supplied, no source" — governed by content rules only.

use serde_json::Value;

use crate::core::context_admission::carrier::{Withheld, admit_carrier_text};

/// The decision for one hook payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PayloadAdmission {
    /// Nothing to change: derive from the payload as it is.
    Unchanged,
    /// Derive from this payload instead (same structure, redacted strings).
    Rewritten(String),
    Withheld(Withheld),
}

impl PayloadAdmission {
    /// The payload text-bearing derivations may read, if any.
    pub(super) fn derivable<'a>(&'a self, original: &'a str) -> Option<&'a str> {
        match self {
            Self::Unchanged => Some(original),
            Self::Rewritten(payload) => Some(payload),
            Self::Withheld(_) => None,
        }
    }
}

/// Admit `input` (the sanitized hook JSON) as a whole.
///
/// The decision is taken over all decoded string values together, so a
/// classification marking in one field (`content`) governs a derivation from
/// another (`new_string`). Only when something must be masked are the strings
/// admitted one by one to build the redacted payload.
pub(super) fn admit_payload(input: &str) -> PayloadAdmission {
    let Ok(mut payload) = serde_json::from_str::<Value>(input) else {
        // Not JSON: no handler derives structured text from it, but the raw
        // text is still the original and decides.
        return match admit_storable(input) {
            Ok(admitted) if admitted == input => PayloadAdmission::Unchanged,
            Ok(admitted) => PayloadAdmission::Rewritten(admitted),
            Err(withheld) => PayloadAdmission::Withheld(withheld),
        };
    };
    let mut leaves = Vec::new();
    collect(&payload, &mut leaves);
    let joined = leaves.join("\n");
    match admit_storable(&joined) {
        Err(withheld) => return PayloadAdmission::Withheld(withheld),
        Ok(admitted) if admitted == joined => return PayloadAdmission::Unchanged,
        Ok(_) => {}
    }
    match rewrite(&mut payload) {
        Ok(()) => PayloadAdmission::Rewritten(payload.to_string()),
        Err(withheld) => PayloadAdmission::Withheld(withheld),
    }
}

/// Restricted content is never persisted, and every derivation of a hook
/// payload is a persisted artefact.
fn admit_storable(text: &str) -> Result<String, Withheld> {
    let admission = admit_carrier_text(text)?;
    if admission.storable {
        Ok(admission.text)
    } else {
        Err(Withheld::new(&["store.restricted_content"]))
    }
}

fn collect<'a>(value: &'a Value, leaves: &mut Vec<&'a str>) {
    match value {
        Value::String(text) if !text.is_empty() => leaves.push(text),
        Value::Array(items) => items.iter().for_each(|item| collect(item, leaves)),
        Value::Object(map) => map.values().for_each(|child| collect(child, leaves)),
        _ => {}
    }
}

fn rewrite(value: &mut Value) -> Result<(), Withheld> {
    match value {
        Value::String(text) if !text.is_empty() => {
            *text = admit_storable(text)?;
            Ok(())
        }
        Value::Array(items) => items.iter_mut().try_for_each(rewrite),
        Value::Object(map) => map.values_mut().try_for_each(rewrite),
        _ => Ok(()),
    }
}
