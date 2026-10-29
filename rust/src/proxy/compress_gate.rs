// SPDX-License-Identifier: Apache-2.0
//! The gate of every change the proxy makes to a block outside the
//! tool-result compressor, and the record of the stubs one request issued.
//!
//! Under protection ([`protected`]) a block changes only if its original can
//! be recovered through a copy bound to the block's source lineage, or if the
//! change loses nothing. Each site asks one of the functions here; none
//! restates the conditions. Unprotected, every function is the identity on
//! the site's own result.
//!
//! - text replaced by a shorter form: [`shortened`], [`rewrite`];
//! - something that is not a text block (a tool-call input, a span of
//!   turns): [`keeps_whole`];
//! - a JSON text whose inner strings were shortened: [`rewrapped`].
//!
//! [`Issued`] holds, for one request, each block that left a pass with a
//! stub next to its original. Two decisions need both after the passes are
//! over: a stub leaves only in a document that still lists the tool it names
//! ([`Issued::settle`]) and only if the final admission passes it unaltered
//! ([`Issued::restore_unadmitted`]). Otherwise the original goes back in.

#[cfg(feature = "http-server")]
use std::cell::RefCell;

#[cfg(feature = "http-server")]
use serde_json::Value;

#[cfg(feature = "http-server")]
use super::{DERIVED_NOT_RECORDED, HANDLE_NOT_FOLLOWABLE, KeptWhole, NO_PROJECT};
use super::{
    UNKNOWN_LINEAGE, carries_recovery_stub, note_kept_whole, protected, shorten_with_copy,
};
#[cfg(feature = "http-server")]
use crate::core::archive::delivered;
#[cfg(feature = "http-server")]
use crate::proxy::request_project;

/// The final admission altered or withheld the stub of a block: the block
/// left whole instead, admitted as its original.
#[cfg(feature = "http-server")]
pub(crate) const STUB_NOT_ADMISSIBLE: &str = "recovery.stub_not_admissible";

/// The code of a block that is not text at all (a tool-call input the model
/// wrote): the Engine delivered no such text, so no lineage can back a
/// shorter form of it.
pub(crate) const NOT_ENGINE_TEXT: &str = UNKNOWN_LINEAGE;

/// `candidate` is what a site would send instead of `original`. Unprotected
/// it is sent. Under protection it is sent only with the stub of a recovery
/// copy of `original` ([`shorten_with_copy`]); `None` keeps the original,
/// noted with its code. Text that already ends with a way back is left as it
/// is: a second cut could drop the handle.
pub(crate) fn shortened(original: &str, candidate: String) -> Option<String> {
    if !protected() {
        return Some(candidate);
    }
    if carries_recovery_stub(original) {
        return None;
    }
    shorten_with_copy(original, &candidate)
}

/// Apply `shorten` to `text` under the rule of [`shortened`]. Several steps
/// of one site on the same block are one change: they run on a copy and the
/// result is gated once, against the block as it arrived.
#[cfg(feature = "http-server")]
pub(crate) fn rewrite(text: &mut String, shorten: impl FnOnce(&mut String)) {
    if !protected() {
        shorten(text);
        return;
    }
    let mut candidate = text.clone();
    shorten(&mut candidate);
    if candidate != *text
        && let Some(sent) = shortened(text, candidate)
    {
        *text = sent;
    }
}

/// For a site that replaces something no recovery copy can stand for:
/// whether `block` (its serialized form, held only to count it once) stays
/// whole. Under protection it does, noted with `code`.
pub(crate) fn keeps_whole(block: &str, code: &'static str) -> bool {
    if !protected() {
        return false;
    }
    note_kept_whole(block, code);
    true
}

/// `rewritten` is the JSON text `outer` after the strings `parts` inside it
/// were shortened. Unprotected it is sent. Under protection each part left
/// with a stub of its own, but the outer text is a new text of unknown
/// lineage for every later turn — unless it is put on record as derived
/// (`delivered::record_derived`). The three guarantees that record needs:
///
/// - every lineage comes from `lineage_of` for an exact original — the outer
///   text and each shortened part — in this request, in the proxy's project
///   scope;
/// - `rewritten` derives from the outer text alone (its parts are in it)
///   plus the fixed text of the stubs;
/// - the lineages of the outer text and of every part are united; if one is
///   missing there is no record.
///
/// Without the record the outer text stays whole, noted with its code. The
/// lineage of a part alone never stands for the outer text: what surrounds
/// the part in it is content of unknown origin.
#[cfg(feature = "http-server")]
pub(crate) fn rewrapped(outer: &str, rewritten: String, parts: &[String]) -> Option<String> {
    if !protected() {
        return Some(rewritten);
    }
    let Some(project) = request_project::current_source_project() else {
        note_kept_whole(outer, NO_PROJECT);
        return None;
    };
    let recorded = request_project::in_project(&project, || {
        let known = |text: &str| {
            delivered::lineage_of(&delivered::digest(text)).filter(delivered::Lineage::is_complete)
        };
        let mut lineage = known(outer)?;
        for part in parts {
            lineage.join(known(part)?);
        }
        Some(delivered::record_derived(
            &delivered::digest(&rewritten),
            &lineage,
        ))
    });
    match recorded {
        Some(true) => {
            note_issued(outer, &rewritten);
            Some(rewritten)
        }
        Some(false) => {
            note_kept_whole(outer, DERIVED_NOT_RECORDED);
            None
        }
        None => {
            note_kept_whole(outer, UNKNOWN_LINEAGE);
            None
        }
    }
}

/// A block as it left a pass with a recovery stub, and what it was.
#[derive(Debug)]
#[cfg(feature = "http-server")]
struct Stub {
    sent: String,
    original: String,
}

/// The blocks of ONE request that left a pass with a recovery stub. Holds
/// content, for the duration of the request only: it is never logged,
/// counted or sent anywhere, and ends with the request.
#[derive(Debug, Default)]
#[cfg(feature = "http-server")]
pub(crate) struct Issued {
    stubs: Vec<Stub>,
}

#[cfg(feature = "http-server")]
thread_local! {
    /// The stubs of the request whose pass runs on this thread.
    static ISSUED: RefCell<Option<Issued>> = const { RefCell::new(None) };
}

/// Run one synchronous pass of a request and add the stubs it issues to
/// `so_far`. The passes of a request are separated by awaits and may run on
/// different threads; the caller carries the value from one into the next.
#[cfg(feature = "http-server")]
pub(crate) fn collecting<T>(so_far: &mut Issued, pass: impl FnOnce() -> T) -> T {
    struct Restore(Option<Issued>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let outer = self.0.take();
            ISSUED.with(|slot| *slot.borrow_mut() = outer);
        }
    }
    let restore = Restore(ISSUED.with(|slot| slot.replace(Some(std::mem::take(so_far)))));
    let result = pass();
    if let Some(known) = ISSUED.with(|slot| slot.borrow_mut().take()) {
        *so_far = known;
    }
    drop(restore);
    result
}

/// `original` left the pass on this thread as `sent`, which ends with a
/// stub. Outside a request's pass nothing is held.
pub(super) fn note_issued(original: &str, sent: &str) {
    // Without the server no pass collects stubs (`collecting` is server-only).
    #[cfg(not(feature = "http-server"))]
    let _ = (original, sent);
    #[cfg(feature = "http-server")]
    ISSUED.with(|slot| {
        if let Some(issued) = slot.borrow_mut().as_mut() {
            issued.stubs.push(Stub {
                sent: sent.to_owned(),
                original: original.to_owned(),
            });
        }
    });
}

#[cfg(feature = "http-server")]
impl Issued {
    /// Before the final admission, on the document that is about to leave:
    /// a block of it that carries a stub of this request leaves only if the
    /// document still lists the tool the stub names, as the host registered
    /// it (`followable`: `sticky_tools::HandleUse::follows`). The proxy
    /// declares nothing and never touches the tool list. Every pass decides
    /// this before it writes a stub, so this is the check of that invariant
    /// on the bytes that leave: where it does not hold, every stubbed block
    /// goes back to its original, noted as `recovery.handle_not_followable`.
    /// Returns whether `doc` changed.
    pub(crate) fn settle(&self, doc: &mut Value, followable: bool, kept: &mut KeptWhole) -> bool {
        !followable
            && !self.stubs.is_empty()
            && self.put_back(doc, None, HANDLE_NOT_FOLLOWABLE, kept)
    }

    /// After the final admission of `sent`: `admitted` is the document that
    /// admission would send instead (`None`: it refused the request). If
    /// every stub of this request is still in it byte for byte, nothing is
    /// to do (`None`). Otherwise the result is `sent` with each block whose
    /// stub was altered or withheld — every stubbed block, after a refusal —
    /// back to its original, noted as [`STUB_NOT_ADMISSIBLE`]; the caller
    /// admits that document instead, once. Blocks are compared at the same
    /// place of both documents: a place the admission moved or removed
    /// counts as altered.
    pub(crate) fn restore_unadmitted(
        &self,
        sent: &Value,
        admitted: Option<&Value>,
        kept: &mut KeptWhole,
    ) -> Option<Value> {
        if self.stubs.is_empty() {
            return None;
        }
        let mut restored = sent.clone();
        self.put_back(
            &mut restored,
            Some(admitted.unwrap_or(&NOWHERE)),
            STUB_NOT_ADMISSIBLE,
            kept,
        )
        .then_some(restored)
    }

    /// Put the original back for every stubbed block of `node` — with
    /// `admitted`, only where the same place of that document does not hold
    /// the identical block. Each block put back is noted with `code`.
    fn put_back(
        &self,
        node: &mut Value,
        admitted: Option<&Value>,
        code: &'static str,
        kept: &mut KeptWhole,
    ) -> bool {
        match node {
            Value::String(text) => {
                let Some(stub) = self.stubs.iter().find(|stub| stub.sent == *text) else {
                    return false;
                };
                if admitted.and_then(Value::as_str) == Some(text.as_str()) {
                    return false;
                }
                kept.note(&stub.original, code);
                text.clone_from(&stub.original);
                true
            }
            Value::Array(items) => {
                let mut changed = false;
                for (index, item) in items.iter_mut().enumerate() {
                    let place = admitted.map(|admitted| admitted.get(index).unwrap_or(&NOWHERE));
                    changed |= self.put_back(item, place, code, kept);
                }
                changed
            }
            Value::Object(fields) => {
                let mut changed = false;
                for (key, field) in fields.iter_mut() {
                    let place = admitted.map(|admitted| admitted.get(key).unwrap_or(&NOWHERE));
                    changed |= self.put_back(field, place, code, kept);
                }
                changed
            }
            _ => false,
        }
    }
}

/// What a place holds that the admitted document does not have.
#[cfg(feature = "http-server")]
static NOWHERE: Value = Value::Null;
