// SPDX-License-Identifier: Apache-2.0

//! The ONE delivery of an Engine-interface document (G08): the project the
//! operation works on is known first, the operation runs in that project's
//! request scope inside one admission capture, and the gateway receipt of
//! exactly the bytes written is sealed before they are written.

use std::io::Write;
use std::path::PathBuf;

/// The project root an invocation names on its command line, as given:
/// `--project-root <dir>`, or `--project <dir>` of an `engine runtime`
/// operation.
pub(super) fn named_project_root(args: &[String]) -> Option<&str> {
    let runtime = args.first().map(String::as_str) == Some("runtime");
    args.windows(2)
        .find(|pair| pair[0] == "--project-root" || (runtime && pair[0] == "--project"))
        .map(|pair| pair[1].as_str())
}

/// The root an invocation's request scope and receipt index are opened for,
/// resolved the way the operations resolve their own root
/// (`std::fs::canonicalize`), so the scope names the directory they then
/// work on. A root that does not resolve is kept as given: the operation
/// refuses it itself.
pub(super) fn scope_root(root: &str) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root))
}

/// The name of the operation for the receipt's trace: the first argument
/// when it is a plain operation word, else `engine`.
pub(super) fn operation_word(args: &[String]) -> &str {
    args.first()
        .map(String::as_str)
        .filter(|word| {
            !word.is_empty()
                && word.len() <= 64
                && word.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        .unwrap_or("engine")
}

/// One Engine-interface invocation, parsed far enough to know the project it
/// works on, with the work still to run.
pub(super) struct EngineCall<'a, E> {
    /// The project the operation works on: the request scope it runs in
    /// (protection and the policy pack are that project's) and the index its
    /// receipt is listed under. `None`: the operation works on no project —
    /// `signer-info`, the host operations that only touch the host ledger
    /// and the Engine's artifact store (`context-checkpoint`,
    /// `context-outcome`, `context-outcome-receipt`), and `engine runtime …`
    /// without `--project`. Such a receipt is listed under
    /// `roots::ENGINE_KEY`; its snapshot is that of the working directory,
    /// which is what an unscoped policy read resolves.
    pub(super) project_root: Option<PathBuf>,
    pub(super) run: Box<dyn FnOnce() -> Result<String, E> + 'a>,
}

/// Why an Engine-interface invocation wrote no document.
#[derive(Debug)]
pub(super) enum DeliveryFailure<E> {
    /// The operation itself failed; nothing was sealed.
    Operation(E),
    /// The receipt is mandatory and could not be sealed: nothing was written.
    ReceiptNotPersisted,
    /// The document was sealed but could not be written: no hand-over is
    /// recorded, the receipt stays `unconfirmed`.
    Write(std::io::Error),
}

/// Run one Engine-interface operation as ONE receipted delivery (G08).
///
/// An Engine-interface answer is a wire document on stdout. It is not passed
/// through a text release gate — its content is admitted at the source seam.
/// What is sealed is the delivery, with the frame pattern of the tool
/// session (`receipt_store::frame`): one admission capture with the policy
/// snapshot taken HERE, at the operation's entry, so every source admission
/// of the operation records into it; then a gateway receipt that binds
/// exactly the bytes written (the document and its final `\n`), sealed
/// before they are written; then the observation `handed_over`, through the
/// receipt's own handle, after the write and flush succeeded.
///
/// The MCP rule for a result no admission decided on applies unchanged:
/// under protection such a document is receipted as exactly that
/// (`delivery.no_source_decision`). Without protection a document whose
/// operation admitted nothing owes nothing.
///
/// The project is known BEFORE the capture is opened ([`EngineCall`]): the
/// operation runs in the request scope of the project it works on, so the
/// snapshot sees that project's protection — a policy pack included — and
/// the receipt is indexed under that project's key.
///
/// The Engine's own evidence inside an answer (Engine receipt, invocation,
/// observation) is another receipt family and is untouched.
pub(super) fn deliver_engine_document<E>(
    operation: &str,
    call: EngineCall<'_, E>,
    out: &mut dyn Write,
) -> Result<(), DeliveryFailure<E>> {
    use crate::core::context_admission::capture::ADMISSIONS;
    use crate::core::context_admission::receipt_store::{
        frame::FrameCall,
        observed, roots,
        trace::{Channel, Delivery},
    };
    let EngineCall { project_root, run } = call;
    let index = project_root
        .as_deref()
        .map(|root| root.to_string_lossy().into_owned());
    let scoped = move || -> Result<(), DeliveryFailure<E>> {
        let frame = FrameCall::open(operation);
        let output = ADMISSIONS
            .sync_scope(Some(frame.capture()), run)
            .map_err(DeliveryFailure::Operation)?;
        let document = format!("{output}\n");
        let prepared = frame
            .seal(
                index.as_deref().unwrap_or(roots::ENGINE_KEY),
                document.as_bytes(),
                false,
                Delivery::engine_interface,
            )
            .map_err(|()| DeliveryFailure::ReceiptNotPersisted)?;
        // The handle waits for exactly this thread's next write of exactly
        // these bytes to stdout; the observation is recorded only after
        // `write_all` and `flush` of them succeeded.
        if let Some(prepared) = prepared {
            prepared.await_write();
        }
        observed::write_observed(out, Channel::Stdout, document.as_bytes(), None)
            .map_err(DeliveryFailure::Write)
    };
    match project_root {
        Some(root) => crate::core::policy::runtime::REQUEST_PROJECT
            .sync_scope(std::cell::RefCell::new(Some(root)), scoped),
        None => scoped(),
    }
}

/// The content-free code of an answer that was not written because its
/// mandatory gateway receipt could not be sealed.
const RECEIPT_NOT_PERSISTED: &str = "receipt_not_persisted";

/// End the process for an invocation that wrote no document: the operation's
/// own code or the receipt code on stderr with status 2; a failed write as
/// every CLI output reports it, with status 1.
pub(super) fn exit_unless_delivered<E>(
    label: &str,
    delivered: Result<(), DeliveryFailure<E>>,
    code: impl FnOnce(E) -> String,
) {
    let (message, status) = match delivered {
        Ok(()) => return,
        Err(DeliveryFailure::Operation(error)) => (format!("{label}: {}", code(error)), 2),
        Err(DeliveryFailure::ReceiptNotPersisted) => {
            (format!("{label}: {RECEIPT_NOT_PERSISTED}"), 2)
        }
        Err(DeliveryFailure::Write(error)) => {
            (format!("lean-ctx: output could not be written: {error}"), 1)
        }
    };
    eprintln!("{message}");
    std::process::exit(status);
}
