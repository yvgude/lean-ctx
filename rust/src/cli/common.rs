pub(crate) fn print_savings(original: usize, sent: usize) {
    let footer = crate::core::protocol::format_savings(original, sent);
    if !footer.is_empty() {
        println!("{footer}");
    }
}

/// One tool run on behalf of a CLI command — the ONE place where the CLI does
/// what the MCP dispatcher does around a tool handler (G01, G03/G04, G05).
///
/// Order, fixed here and nowhere else in `cli/`:
///
/// 1. the handler runs inside the request scope (so the shared read path
///    admits its sources) AND inside the source capture (so every source the
///    admitted readers publish is known);
/// 2. the finished text passes the release point every MCP tool result
///    passes (`server::policy_guard::release_result`), then the legacy output
///    redaction (`redact_model_text`). The result of this step is the text
///    the command prints — a command must not change it afterwards;
/// 3. the lineage of exactly that text is recorded
///    (`core::archive::delivered::record_tool_delivery`), which decides by
///    its own rules whether anything is recorded: an undeclared tool, an
///    error, an absent or incomplete capture record nothing. Unknown stays
///    unknown; nothing is guessed here;
/// 4. the caller writes the returned text with [`print_released`].
///
/// Receipt (G08): where the receipt store is mandatory (protection is
/// active), steps 1 and 2 run inside one admission capture, and between
/// steps 2 and 3 the Decision Receipt of the output is built, validated and
/// stored. It binds exactly the bytes the writer writes — the text and its
/// final `\n`. An output whose owed receipt cannot be stored is not printed:
/// the content-free notice `receipt.not_persisted` is returned in its place,
/// as a refusal. Nothing is added to stdout either way (#498). The principal
/// is `unknown` unless the process showed a presenter capability.
///
/// One policy snapshot (G08): the run takes it once, where it starts (the
/// first of steps 1 and 2), and its capture carries it — the release point,
/// the receipt duty and the trace sidecar of the run decide under it.
///
/// PREPARED, not handed over: the receipt and its sidecar exist before a
/// byte is written and prove no write. The hand-over is observed by the ONE
/// writer, [`print_released`] (stdout) or [`exit_withheld`] (stderr and the
/// exit status), after `write_all` and `flush` of exactly the prepared bytes
/// succeeded. The receipt's own handle waits for that write on the thread
/// that prepared it; a command that prints the returned text any other way
/// leaves the delivery unconfirmed, which is what the inspector then says.
/// Variants for output that is not `text + "\n"` on stdout:
/// [`CliToolRun::deliver_json_exact`] with [`print_released_exact`] (no
/// final newline), [`CliToolRun::to_stderr`] with [`eprint_released`].
///
/// Digested bytes: the returned text, and the same text followed by the one
/// `\n` that the writer adds. A consumer that takes the CLI's stdout as a
/// tool result holds one of the two (hosts differ in whether they trim the
/// final newline); both are the same delivery from the same sources. No other
/// prefix or wrapper is added by the CLI.
pub(crate) struct CliToolRun<'a> {
    tool: &'a str,
    project_root: &'a str,
    /// The arguments of the call. The tool's own declaration for them
    /// (`McpTool::publishes_every_source`) is asked only when there is a
    /// captured source set to record — the registry is not built for a run
    /// that can record nothing (no protection, no source, an error).
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
    /// `false` for output that is not a tool-result carrier (a human-facing
    /// table, or the text wrapped into another document such as `--json`):
    /// steps 1 and 2 still apply, step 3 records nothing.
    carrier: bool,
    /// Whether step 2 ends with the legacy output redaction.
    legacy_redaction: bool,
    /// The admission capture of the output being prepared; opened only where
    /// the receipt store is mandatory, taken when the receipt is recorded.
    admissions: std::sync::Mutex<
        Option<std::sync::Arc<crate::core::context_admission::capture::AdmissionCapture>>,
    >,
    /// The receipt duty of this run and the ONE policy snapshot it was
    /// decided under, taken where the run starts.
    duty: std::sync::OnceLock<crate::core::context_admission::receipt_store::Duty>,
    /// The receipt index the output is listed under when it is not the
    /// project's own ([`Self::receipt_index`]).
    receipt_index: Option<&'a str>,
    /// Where the command hands the output: stdout unless a builder says
    /// otherwise ([`Self::to_stderr`], [`Self::to_file`],
    /// [`Self::to_clipboard`]).
    channel: crate::core::context_admission::receipt_store::trace::Channel,
    /// The Ed25519 public key a package signature was VERIFIED with, when the
    /// document names it ([`Self::verified_signer_key`]).
    verified_signer_key: Option<&'a str>,
}

/// The stdout text one handler produced, with what the helper has to know
/// about it to decide on a lineage record.
pub(crate) struct Printable {
    /// The text, without the final newline the writer adds.
    pub text: String,
    /// `false` when the handler mixed session-derived or hint text into the
    /// output: such a text is never recorded as a delivery of its sources.
    pub source_only: bool,
    /// The tool itself reports a failure (`ShellOutcome::is_error`, a non-zero
    /// exit status of the primitive): an error is never a delivery.
    pub failed: bool,
}

impl Printable {
    /// A successful tool text that consists of the tool's own output only.
    pub(crate) fn tool_text(text: String) -> Self {
        Self {
            text,
            source_only: true,
            failed: false,
        }
    }
}

impl<'a> CliToolRun<'a> {
    /// A run of the registered tool `tool` with `arguments` for `project_root`.
    /// Whether the tool declares every source is asked of the tool itself,
    /// for these arguments; an unknown tool declares nothing.
    pub(crate) fn new(
        tool: &'a str,
        project_root: &'a str,
        arguments: Option<&serde_json::Map<String, serde_json::Value>>,
    ) -> Self {
        Self {
            tool,
            project_root,
            arguments: arguments.cloned(),
            carrier: true,
            legacy_redaction: true,
            admissions: std::sync::Mutex::new(None),
            duty: std::sync::OnceLock::new(),
            receipt_index: None,
            channel: crate::core::context_admission::receipt_store::trace::Channel::Stdout,
            verified_signer_key: None,
        }
    }

    /// The document names the public key the Engine has just verified a
    /// package signature with (`"signer_public_key"`). The release then runs
    /// inside [`crate::core::redaction::with_verified_signer_key`]: the
    /// ORIGINAL document, real key included, passes the policy pack, the
    /// configured redaction patterns, the gateway and the redactor as always;
    /// only the built-in generic-secret rule does not take that one member
    /// for a secret. Nothing is substituted and nothing else is exempt.
    pub(crate) fn verified_signer_key(mut self, key: Option<&'a str>) -> Self {
        self.verified_signer_key = key;
        self
    }

    /// The command writes this run's output to stderr
    /// ([`eprint_released`]): the receipt is prepared for `Channel::Stderr`,
    /// for the text and its final `\n`. Everything else is unchanged.
    #[expect(
        clippy::wrong_self_convention,
        reason = "builder that redirects the run"
    )]
    pub(crate) fn to_stderr(mut self) -> Self {
        self.channel = crate::core::context_admission::receipt_store::trace::Channel::Stderr;
        self
    }

    /// The command writes this run's output to a FILE instead of a stream:
    /// the receipt is prepared for the channel `file`, for exactly the
    /// returned text (no `\n` appended). The path is not recorded. After the
    /// file was written successfully the command calls
    /// [`confirm_written_to_file`] with the bytes it wrote.
    #[expect(
        clippy::wrong_self_convention,
        reason = "builder that redirects the run"
    )]
    pub(crate) fn to_file(mut self) -> Self {
        self.channel = crate::core::context_admission::receipt_store::trace::Channel::File;
        self
    }

    /// The command hands this run's output to the system CLIPBOARD: the
    /// receipt is prepared for the channel `clipboard`, for exactly the
    /// returned text. After the hand-off succeeded the command calls
    /// [`confirm_copied_to_clipboard`] with the text it copied.
    #[expect(
        clippy::wrong_self_convention,
        reason = "builder that redirects the run"
    )]
    pub(crate) fn to_clipboard(mut self) -> Self {
        self.channel = crate::core::context_admission::receipt_store::trace::Channel::Clipboard;
        self
    }

    /// The channel this run's output is prepared for.
    fn output_channel(&self) -> crate::core::context_admission::receipt_store::trace::Channel {
        self.channel
    }

    /// Whether the writer of this run's channel adds one final `\n` to the
    /// returned text (the stream writers do; a file or the clipboard takes
    /// exactly the text).
    fn writer_adds_newline(&self) -> bool {
        use crate::core::context_admission::receipt_store::trace::Channel;
        matches!(self.channel, Channel::Stdout | Channel::Stderr)
    }

    /// List the output's receipt under `key` instead of the project's recent
    /// deliveries. For a command that reports ON those deliveries (`lean-ctx
    /// inspect`): its own output is receipted like any other, but must not
    /// displace the deliveries it shows. The receipt itself is unchanged.
    pub(crate) fn receipt_index(mut self, key: &'a str) -> Self {
        self.receipt_index = Some(key);
        self
    }

    /// Run `operation` inside this run's admission capture, so every gateway
    /// decision about the output reaches its receipt (G08). A capture that
    /// is already open (the run is part of a receipted call) is kept. The
    /// capture carries the run's policy snapshot, so everything inside it —
    /// the release point included — decides under the rules of the run's
    /// start. Must be called inside the request scope.
    ///
    /// The capture is opened in every mode, as the MCP call does: without
    /// protection a run whose capture describes an admitted object — a
    /// source it read, the text its release point admitted — owes the same
    /// plain receipt (`receipt_store::record_delivery`), and a run whose
    /// capture recorded nothing owes none ([`Self::receipted`]).
    /// Opening it only under protection left the decision of a short-lived
    /// `lean-ctx read` unrecorded: `lean-ctx inspect` could not show it.
    fn captured<T>(&self, operation: impl FnOnce() -> T) -> T {
        use crate::core::context_admission::capture;
        if capture::current().is_some() {
            return operation();
        }
        let admissions = self
            .admissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_with(|| {
                capture::AdmissionCapture::for_call(self.duty().snapshot().clone())
            })
            .clone();
        capture::ADMISSIONS.sync_scope(Some(admissions), operation)
    }

    /// The receipt duty of this run under its ONE policy snapshot. The first
    /// call takes the snapshot — the start of the run; every later use is
    /// that same snapshot. Must be called inside the request scope.
    fn duty(&self) -> &crate::core::context_admission::receipt_store::Duty {
        self.duty
            .get_or_init(crate::core::context_admission::receipt_store::Duty::at_entry)
    }

    /// Whether the receipt store is mandatory for this run: decided once,
    /// under the run's snapshot, for acquisition, release and receipt.
    /// Must be called inside the request scope.
    fn mandatory(&self) -> bool {
        self.duty().mandatory
    }

    /// The digest of `text` as it is before the release point, for the trace
    /// sidecar; computed only where a receipt will be recorded.
    fn prepared_digest(&self, text: &str) -> Option<lean_ctx_protocol::Sha256Digest> {
        self.mandatory()
            .then(|| crate::core::context_admission::receipt_store::output_digest(text.as_bytes()))
    }

    /// Build, validate and store the receipt of exactly `written` — the
    /// bytes PREPARED for `channel` — before a byte of it is written, with
    /// its trace sidecar (transport CLI, digest before the release point,
    /// intended channel, digest of the prepared bytes) and audit anchor.
    /// Nothing here says the bytes were written: that is observed by the
    /// writer ([`print_released`], [`exit_withheld`]). `false`: the receipt
    /// is owed and missing, so `written` must not be written. Without a
    /// capture (a run inside a receipted call) there is nothing to record.
    /// Without protection a run whose capture recorded nothing owes nothing,
    /// and one that admitted something gets the plain receipt — no sidecar, no anchor
    /// duty, never a withheld output: a failed write is logged and the text
    /// is printed. Must be called inside the request scope and outside the
    /// run's capture.
    fn receipted(
        &self,
        written: &str,
        is_error: bool,
        before_release: Option<lean_ctx_protocol::Sha256Digest>,
        channel: crate::core::context_admission::receipt_store::trace::Channel,
    ) -> bool {
        use crate::core::context_admission::{capture::Delivered, receipt_store};
        let Some(admissions) = self
            .admissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return true;
        };
        let Some(duty) = self.duty.get() else {
            return true;
        };
        // The MCP call's rule (`finish_gateway_receipt`): without protection
        // a run whose capture recorded nothing owes nothing.
        if admissions.is_empty() && !duty.mandatory {
            return true;
        }
        let delivery = receipt_store::trace::Delivery::cli(self.tool, channel, duty.snapshot())
            .before_release(before_release);
        let presenter_ref = crate::core::archive::presented::shown_presenter_reference();
        let record = receipt_store::record_delivery(
            &admissions,
            &receipt_store::ReceiptSubject {
                tool: self.tool,
                agent_id: None,
                presenter_ref: presenter_ref.as_deref(),
                project_root: self.receipt_index.unwrap_or(self.project_root),
                delivery: &delivery,
            },
            &Delivered {
                text: written,
                is_error,
                tokens: crate::core::tokens::count_tokens(written) as u64,
            },
            duty,
        );
        tracing::debug!("context gateway receipt: {}", record.summary());
        let released = !record.withholds(duty.mandatory);
        // The receipt's own handle waits for this thread's next write of
        // exactly these bytes to the channel; no other write confirms it.
        if let Some(prepared) = record.into_prepared() {
            prepared.await_write();
        }
        released
    }

    /// The printed output is not a tool-result carrier: record no lineage.
    pub(crate) fn human_facing(mut self) -> Self {
        self.carrier = false;
        self
    }

    /// The command prints text that its own admitted reader produced and has
    /// never passed the legacy output redactor (`read`, `grep`, `glob`,
    /// `ls`). Step 2 then ends after the release point: running the legacy
    /// redactor on top could change bytes of an already admitted read, and
    /// those bytes are pinned (#498). Everything else is unchanged.
    pub(crate) fn admitted_reader_output(mut self) -> Self {
        self.legacy_redaction = false;
        self
    }

    /// The whole sequence for a handler that also returns a value of its own
    /// (an exit status, a recording): steps 1–3 above. `handler` returns its
    /// value and, when it produced stdout text, that text as a [`Printable`].
    /// The returned text is what the caller writes with [`print_released`]; `None`
    /// means nothing is printed, released or recorded.
    pub(crate) fn run<T>(
        &self,
        handler: impl FnOnce() -> (T, Option<Printable>),
    ) -> (T, Option<String>) {
        let ((value, printable), capture) = self.acquire(handler);
        let printed = printable.map(|printable| self.finish_checked(printable, capture, false).0);
        (value, printed)
    }

    /// [`Self::run`] that also says whether the release point refused the
    /// text: `(text, true)` is the content-free notice of a refusal, not the
    /// handler's output. For a command that also sends the released text to a
    /// destination of its own (a file, the clipboard) and must write no
    /// notice there. Steps 2 and 3 are those of [`Self::finish`].
    pub(crate) fn run_checked<T>(
        &self,
        handler: impl FnOnce() -> (T, Option<Printable>),
    ) -> (T, Option<(String, bool)>) {
        let ((value, printable), capture) = self.acquire(handler);
        // A caller that asks whether the text was refused does not print the
        // notice in place of the output: it ends through [`exit_withheld`],
        // which writes it to stderr. The refusal is prepared for that.
        let printed = printable.map(|printable| self.finish_checked(printable, capture, true));
        (value, printed)
    }

    /// Step 1 on its own, for a command whose acquisition and publication are
    /// separate phases (the protected CLI path prepares its output, verifies
    /// its authority and only then publishes): run `handler` inside the
    /// request scope and the source capture, and hand back what it acquired.
    /// The capture goes to [`Self::finish`] unchanged.
    pub(crate) fn acquire<T>(
        &self,
        handler: impl FnOnce() -> T,
    ) -> (T, Option<crate::core::archive::authority::ArchiveAuthority>) {
        crate::tools::admitted_sources::as_agent_acquisition(self.project_root, || {
            self.captured(|| crate::core::archive::authority::capture_sync(handler))
        })
    }

    /// Steps 2 and 3 for a text acquired under [`Self::acquire`]: release,
    /// redaction, lineage record of the returned text. The caller prints the
    /// result with [`print_released`] and changes nothing in it.
    pub(crate) fn finish(
        &self,
        printable: Printable,
        capture: Option<crate::core::archive::authority::ArchiveAuthority>,
    ) -> String {
        self.finish_checked(printable, capture, false).0
    }

    /// The one body of steps 2 and 3, for [`Self::finish`] and
    /// [`Self::run_checked`]: the text to print, and whether the release
    /// point refused it.
    fn finish_checked(
        &self,
        printable: Printable,
        capture: Option<crate::core::archive::authority::ArchiveAuthority>,
        refusal_on_stderr: bool,
    ) -> (String, bool) {
        crate::tools::admitted_sources::as_agent_acquisition(self.project_root, || {
            let prepared = self.prepared_digest(&printable.text);
            let (mut printed, mut refused) = self.captured(|| self.released(printable.text));
            let mut with_newline = format!("{printed}\n");
            // G08: the receipt of the bytes prepared for the run's stream is
            // stored before they are written; without it they are not
            // written. A refusal of the release point is printed like any
            // text: its bytes are bound in the trace sidecar (V1 names no
            // output for it).
            let failed = printable.failed || refused;
            // Prepared for the channel the bytes are really written to. A
            // stream run writes text + `\n` to its stream, a refusal too. A
            // file or clipboard run hands over exactly the text — but its
            // REFUSAL never goes to the file or the clipboard: the command
            // ends through [`exit_withheld`], which writes the notice and
            // `\n` to stderr. So that refusal is prepared for stderr — and so
            // is the refusal of any run whose caller ends that way
            // (`refusal_on_stderr`, [`Self::run_checked`]).
            let stderr = crate::core::context_admission::receipt_store::trace::Channel::Stderr;
            let (handed, channel) = if refused && (refusal_on_stderr || !self.writer_adds_newline())
            {
                (with_newline.as_str(), stderr)
            } else if self.writer_adds_newline() {
                (with_newline.as_str(), self.output_channel())
            } else {
                (printed.as_str(), self.output_channel())
            };
            if !self.receipted(handed, failed, prepared, channel) {
                printed = crate::core::context_admission::receipt_store::not_persisted_notice();
                with_newline = format!("{printed}\n");
                refused = true;
            }
            let is_error = printable.failed || refused;
            if self.carrier {
                // Session or hint text mixed into the output is expressed the
                // way `record_tool_delivery` expects it: the call then does
                // not declare every source.
                let declares = printable.source_only
                    && !is_error
                    && capture.is_some()
                    && crate::core::archive::delivered::declares_every_source(
                        &crate::server::registry::build_registry(),
                        self.tool,
                        self.arguments.as_ref(),
                    );
                crate::core::archive::delivered::record_tool_delivery(
                    declares,
                    is_error,
                    capture,
                    &[printed.as_str(), with_newline.as_str()],
                );
            }
            (printed, refused)
        })
    }

    /// Step 2 for one text: the release point, then (unless the command
    /// prints an admitted reader's output) the legacy output redaction.
    /// Returns the text to print and whether the release point refused it.
    /// Must be called inside the request scope.
    fn released(&self, text: String) -> (String, bool) {
        use rmcp::model::{CallToolResult, ContentBlock};
        let released = crate::server::policy_guard::release_result(
            self.tool,
            CallToolResult::success(vec![ContentBlock::text(text)]),
        );
        let refused = released.is_error == Some(true);
        let released_text = released
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|text| text.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        let printed = if self.legacy_redaction {
            crate::server::policy_guard::redact_model_text(&released_text)
        } else {
            released_text
        };
        (printed, refused)
    }

    /// The sequence for a machine-readable document (`--json`): the handler
    /// runs inside the request scope, and the document it returns is released
    /// WITHOUT ever breaking it. A document is never recorded as a delivery of
    /// its sources — it wraps the tool's text into another representation.
    ///
    /// - `Ok(document)`: print it. An admitted document keeps its bytes. When
    ///   a value had to be masked, the masked value stands in its place in a
    ///   still valid document, and the content-free notice went to stderr —
    ///   the schema is not extended and nothing is prefixed to stdout.
    /// - `Err(reason)`: the document is withheld as a whole; `reason` is the
    ///   content-free notice for stderr. Nothing is printed to stdout.
    pub(crate) fn deliver_json(&self, handler: impl FnOnce() -> String) -> Result<String, String> {
        self.deliver_document(handler, true)
    }

    /// [`Self::deliver_json`] for a command that writes the document WITHOUT
    /// a final newline ([`print_released_exact`]): the receipt binds exactly
    /// the document's bytes, nothing appended.
    pub(crate) fn deliver_json_exact(
        &self,
        handler: impl FnOnce() -> String,
    ) -> Result<String, String> {
        self.deliver_document(handler, false)
    }

    /// The one body of [`Self::deliver_json`] and
    /// [`Self::deliver_json_exact`]. `final_newline`: the writer adds one
    /// `\n` after the document, so the receipt binds it too.
    fn deliver_document(
        &self,
        handler: impl FnOnce() -> String,
        final_newline: bool,
    ) -> Result<String, String> {
        use crate::core::context_admission::receipt_store::trace::Channel;
        let (document, _capture) = self.acquire(handler);
        let prepared =
            crate::tools::admitted_sources::as_agent_acquisition(self.project_root, || {
                self.prepared_digest(&document)
            });
        let released = self.release_document(document);
        // G08: the receipt binds exactly the bytes the writer writes — the
        // document, and the final `\n` where the writer adds one. A
        // withheld document prepares nothing for stdout: its reason is
        // prepared for stderr. That those stderr bytes were written, and the
        // status the command ends with, is recorded by [`exit_withheld`]
        // when it has written them — never inferred from this `Err`.
        let (written, is_error, channel) = match &released {
            Ok((document, _)) if final_newline => {
                (format!("{document}\n"), false, self.output_channel())
            }
            Ok((document, _)) => (document.clone(), false, self.output_channel()),
            Err(reason) => (format!("{reason}\n"), true, Channel::Stderr),
        };
        let receipted =
            crate::tools::admitted_sources::as_agent_acquisition(self.project_root, || {
                self.receipted(&written, is_error, prepared, channel)
            });
        if !receipted {
            return Err(crate::core::context_admission::receipt_store::not_persisted_notice());
        }
        let (document, notice) = released?;
        if let Some(notice) = notice {
            eprintln!("{notice}");
        }
        Ok(document)
    }

    /// [`Self::deliver_json`] without the handler run and without stderr: the
    /// released document and, when something was masked in it, the notice.
    /// Runs inside the run's admission capture (G08).
    fn release_document(&self, document: String) -> Result<(String, Option<String>), String> {
        let release = || {
            crate::tools::admitted_sources::as_agent_acquisition(self.project_root, || {
                self.captured(|| self.release_document_scoped(document))
            })
        };
        match self.verified_signer_key {
            Some(key) => crate::core::redaction::with_verified_signer_key(key, release),
            None => release(),
        }
    }

    fn release_document_scoped(
        &self,
        document: String,
    ) -> Result<(String, Option<String>), String> {
        use crate::core::context_admission::carrier::{ReleaseGate, Withheld};
        crate::tools::admitted_sources::as_agent_acquisition(self.project_root, || {
            // The whole document through the one release sequence first: an
            // admitted document passes unchanged and keeps its bytes (#498).
            let (whole, refused) = self.released(document.clone());
            if !refused && whole == document {
                return Ok((document, None));
            }
            let unadmitted = || Withheld::new(&["release.unadmitted_content"]).notice();
            // Something has to be masked. Masking the serialized text could
            // cut through its syntax, so every string value is released on
            // its own and the document is serialized again around them.
            let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&document) else {
                return Err(if refused { whole } else { unadmitted() });
            };
            let gate = ReleaseGate::current();
            let mut notice = None;
            self.mask_leaves(&gate, &mut value, 0, &mut notice)?;
            let serialized = if document.contains('\n') {
                serde_json::to_string_pretty(&value)
            } else {
                serde_json::to_string(&value)
            };
            let Ok(masked) = serialized else {
                return Err(unadmitted());
            };
            // The result must itself pass the release sequence unchanged;
            // what value-wise masking cannot settle (a policy-pack refusal,
            // a finding in a key) withholds the document as a whole.
            let (verified, refused) = self.released(masked.clone());
            if refused {
                return Err(verified);
            }
            if verified != masked {
                return Err(unadmitted());
            }
            let notice =
                notice.unwrap_or_else(|| "[lean-ctx gateway: content redacted]".to_string());
            Ok((masked, Some(notice)))
        })
    }

    /// Release every string value of `value` in place. A key is never
    /// rewritten (that would change the schema): a finding in a key, a
    /// withheld value or a document nested too deep to inspect withholds the
    /// whole document with a content-free reason.
    fn mask_leaves(
        &self,
        gate: &crate::core::context_admission::carrier::ReleaseGate,
        value: &mut serde_json::Value,
        depth: usize,
        notice: &mut Option<String>,
    ) -> Result<(), String> {
        use crate::core::context_admission::carrier::{Release, Withheld};
        use serde_json::Value;
        const MAX_DEPTH: usize = 64;
        if depth > MAX_DEPTH {
            return Err(Withheld::new(&["coverage.partial"]).notice());
        }
        match value {
            Value::String(text) => {
                match gate.text(text) {
                    Release::Unchanged => {}
                    Release::Redacted {
                        text: masked,
                        notice: line,
                    } => {
                        *text = masked;
                        notice.get_or_insert(line);
                    }
                    Release::Withheld(withheld) => return Err(withheld.notice()),
                }
                if self.legacy_redaction {
                    *text = crate::server::policy_guard::redact_model_text(text);
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.mask_leaves(gate, item, depth + 1, notice)?;
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    if !matches!(gate.text(key), Release::Unchanged) {
                        return Err(Withheld::new(&["release.unadmitted_content"]).notice());
                    }
                    self.mask_leaves(gate, item, depth + 1, notice)?;
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        Ok(())
    }

    /// Run `handler` and return the text to print (steps 1–3 above). A
    /// handler error is passed through; nothing is released or recorded for
    /// it.
    pub(crate) fn deliver<E>(
        &self,
        handler: impl FnOnce() -> Result<String, E>,
    ) -> Result<String, E> {
        let (outcome, printed) = self.run(|| match handler() {
            Ok(text) => (Ok(()), Some(Printable::tool_text(text))),
            Err(error) => (Err(error), None),
        });
        outcome.map(|()| printed.unwrap_or_default())
    }

    /// [`Self::deliver`] for a handler that cannot fail.
    pub(crate) fn deliver_text(&self, handler: impl FnOnce() -> String) -> String {
        match self.deliver(|| Ok::<String, std::convert::Infallible>(handler())) {
            Ok(text) => text,
            Err(never) => match never {},
        }
    }
}

/// The ONE stdout writer for a text a [`CliToolRun`] returned: `text` and
/// its final `\n`, written and flushed, and — only after both succeeded —
/// the observation that exactly the bytes a receipt was prepared for were
/// handed to stdout (G08, `receipt_store::observed`). Text no receipt was
/// prepared for (no protection) is written the same way and records
/// nothing. A failed write ends the command with status 1; it records no
/// hand-over.
pub(crate) fn print_released(text: &str) {
    write_stdout_observed(format!("{text}\n").as_bytes());
}

/// [`print_released`] without the final `\n`: exactly `text` is written. For
/// output whose receipt was prepared for exactly its bytes
/// ([`CliToolRun::deliver_json_exact`]).
pub(crate) fn print_released_exact(text: &str) {
    write_stdout_observed(text.as_bytes());
}

fn write_stdout_observed(bytes: &[u8]) {
    use crate::core::context_admission::receipt_store::{observed, trace::Channel};
    let written =
        observed::write_observed(&mut std::io::stdout().lock(), Channel::Stdout, bytes, None);
    if let Err(error) = written {
        eprintln!("lean-ctx: output could not be written: {error}");
        std::process::exit(1);
    }
}

/// The ONE stderr writer for a text a [`CliToolRun`] prepared for stderr
/// ([`CliToolRun::to_stderr`]): `text` and its final `\n`, written and
/// flushed, then the observation for the receipt this thread prepared for
/// exactly those bytes. Returns to the caller. `exit_status`: the status the
/// caller ends the process with right after, when it does; it is recorded
/// with the observation. A failed write records nothing.
pub(crate) fn eprint_released(text: &str, exit_status: Option<i32>) {
    use crate::core::context_admission::receipt_store::{observed, trace::Channel};
    let line = format!("{text}\n");
    let _ = observed::write_observed(
        &mut std::io::stderr().lock(),
        Channel::Stderr,
        line.as_bytes(),
        exit_status,
    );
}

/// The command wrote exactly `bytes` to the file it was asked for and the
/// write SUCCEEDED: record `handed_over` (channel `file`) for the receipt
/// this thread prepared with [`CliToolRun::to_file`], if these are the
/// prepared bytes. Call it only after the successful write; other bytes, or
/// no prepared receipt, record nothing.
pub(crate) fn confirm_written_to_file(bytes: &[u8]) {
    use crate::core::context_admission::receipt_store::{observed, trace::Channel};
    observed::handed_over(Channel::File, bytes, None);
}

/// The command handed exactly `text` to the system clipboard and the
/// hand-off SUCCEEDED: record `handed_over` (channel `clipboard`) for the
/// receipt this thread prepared with [`CliToolRun::to_clipboard`]. Call it
/// only after that success.
pub(crate) fn confirm_copied_to_clipboard(text: &str) {
    use crate::core::context_admission::receipt_store::{observed, trace::Channel};
    observed::handed_over(Channel::Clipboard, text.as_bytes(), None);
}

/// End a command whose machine-readable document was withheld as a whole
/// ([`CliToolRun::deliver_json`]): the content-free reason on stderr, exit 1,
/// nothing on stdout. The ONE writer of that reason: after the bytes were
/// written and flushed, the observation binds them and the exit status the
/// process is ended with on the next line (G08). A failed write records
/// nothing; the process ends with status 1 either way.
pub(crate) fn exit_withheld(reason: &str) -> ! {
    eprint_released(reason, Some(1));
    std::process::exit(1);
}

/// Strip savings footers from daemon output when the CLI client has footer suppressed.
#[cfg(unix)]
pub(crate) fn filter_daemon_output(text: &str) -> String {
    if crate::core::protocol::savings_footer_visible() {
        return text.to_string();
    }
    text.lines()
        .filter(|l| {
            let t = l.trim();
            !(t.starts_with('[')
                && t.contains("tok")
                && t.ends_with(']')
                && (t.contains("tok saved") || t.contains("lean-ctx:") || t.contains("vs native")))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn load_shell_history_pub() -> Vec<String> {
    load_shell_history()
}

pub(crate) fn load_shell_history() -> Vec<String> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    let history_file = if shell.contains("zsh") {
        home.join(".zsh_history")
    } else if shell.contains("fish") {
        home.join(".local/share/fish/fish_history")
    } else if cfg!(windows) && shell.is_empty() {
        home.join("AppData")
            .join("Roaming")
            .join("Microsoft")
            .join("Windows")
            .join("PowerShell")
            .join("PSReadLine")
            .join("ConsoleHost_history.txt")
    } else {
        home.join(".bash_history")
    };

    // Shell history files (especially zsh's metafied format) frequently contain
    // non-UTF-8 bytes; `read_to_string` would reject the whole file. Read raw and
    // decode lossily so a single bad byte never hides 900 lines of real history.
    match std::fs::read(&history_file) {
        Ok(bytes) => admitted_history(&String::from_utf8_lossy(&bytes))
            .lines()
            .filter_map(|l| {
                let trimmed = l.trim();
                if trimmed.starts_with(':') {
                    trimmed
                        .split(';')
                        .nth(1)
                        .map(std::string::ToString::to_string)
                } else {
                    Some(trimmed.to_string())
                }
            })
            .filter(|l| !l.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Shell history is command lines the user typed — tokens, passwords and
/// connection strings included. Before anything is counted, ranked or printed
/// from it (`discover`, `session` statistics), the whole text passes
/// admission as carrier text: a value the gateway masks is masked in every
/// derived line, and a history the policy withholds contributes no line at
/// all. Redaction keeps the line structure, so the per-line parsing below is
/// unaffected for clean history. Nothing is written back.
fn admitted_history(raw: &str) -> String {
    crate::core::context_admission::carrier::admit_carrier_text(raw)
        .map(|admission| admission.text)
        .unwrap_or_default()
}

pub(crate) fn daemon_fallback_hint() {
    use std::sync::Once;
    static HINT: Once = Once::new();
    HINT.call_once(|| {
        if crate::core::protocol::meta_visible() {
            eprintln!("\x1b[2;33mhint: daemon not running — stats tracked locally (lean-ctx serve -d for full tracking)\x1b[0m");
        }
    });
}

pub(crate) fn format_tokens_cli(tokens: u64) -> String {
    if tokens >= 1_000_000_000_000 {
        format!("{:.2}T", tokens as f64 / 1_000_000_000_000.0)
    } else if tokens >= 1_000_000_000 {
        // Heavy users cross 1B; keep growing visibly instead of "1310.0M".
        format!("{:.2}B", tokens as f64 / 1_000_000_000.0)
    } else if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}

pub(crate) fn detect_project_root(args: &[String]) -> String {
    detect_project_root_in(args, std::env::current_dir().ok().as_deref())
}

/// The project a command that writes project-scoped state acts for; `Err`
/// instead of `~`, `/` or an agent directory (AP04-G10).
pub(crate) fn require_project_root() -> Result<String, String> {
    require_project_root_in(std::env::current_dir().ok().as_deref())
}

fn require_project_root_in(cwd: Option<&std::path::Path>) -> Result<String, String> {
    let root = detect_project_root_in(&[], cwd);
    if crate::core::pathutil::is_broad_or_unsafe_root(std::path::Path::new(&root)) {
        return Err(format!(
            "{root} is not a project — run inside a project or set LEAN_CTX_PROJECT_ROOT"
        ));
    }
    Ok(root)
}

fn detect_project_root_in(args: &[String], cwd: Option<&std::path::Path>) -> String {
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--root=")
            && !v.trim().is_empty()
        {
            return normalize_explicit_root(v);
        }
        if let Some(v) = a.strip_prefix("--project-root=")
            && !v.trim().is_empty()
        {
            return normalize_explicit_root(v);
        }
        if (a == "--root" || a == "--project-root")
            && let Some(v) = it.peek()
            && !v.starts_with("--")
            && !v.trim().is_empty()
        {
            return normalize_explicit_root(v);
        }
    }
    let cwd_root = cwd.map(|cwd| promote_to_git_root(&cwd.to_string_lossy()));
    if let Ok(root) = std::env::var("LEAN_CTX_PROJECT_ROOT")
        && !root.trim().is_empty()
    {
        return crate::server::roots::reconcile_pinned_root(
            normalize_explicit_root(&root),
            cwd_root,
        );
    }
    cwd_root.unwrap_or_else(|| ".".to_string())
}

fn normalize_explicit_root(path: &str) -> String {
    let expanded = expand_home(path.trim());
    crate::core::index_paths::normalize_project_root(&expanded)
}

fn expand_home(path: &str) -> String {
    if path == "~" {
        return dirs::home_dir().map_or_else(
            || path.to_string(),
            |home| home.to_string_lossy().to_string(),
        );
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir().map_or_else(
            || path.to_string(),
            |home| home.join(rest).to_string_lossy().to_string(),
        );
    }
    path.to_string()
}

pub(crate) fn promote_to_git_root(path: &str) -> String {
    let mut p = std::path::Path::new(path);
    loop {
        // A dotfiles repository at `~` is not the project of everything below.
        if p.join(".git").exists() && !crate::core::pathutil::is_broad_or_unsafe_root(p) {
            return p.to_string_lossy().to_string();
        }
        match p.parent() {
            Some(parent) => p = parent,
            None => return path.to_string(),
        }
    }
}

/// Shared set-up for the tests of the CLI commands that run through
/// [`CliToolRun`]. Every test starts with the isolation lines
/// (`isolated_data_dir`, `TestPolicyOverride`) itself.
#[cfg(test)]
pub(crate) mod carrier_tests_support {
    /// A value the gateway masks in every mode.
    pub(crate) const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    /// Governed mode without a pack: protection is active, so the admitted
    /// readers publish their sources into the capture. Call after
    /// `isolated_data_dir()`.
    pub(crate) fn governed() {
        let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::create_dir_all(&dir).expect("config dir");
        std::fs::write(
            dir.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("config");
    }

    /// A project directory and its canonical root.
    pub(crate) fn project() -> (tempfile::TempDir, String) {
        let project = tempfile::tempdir().expect("project dir");
        let root = std::fs::canonicalize(project.path())
            .expect("root")
            .to_string_lossy()
            .into_owned();
        (project, root)
    }

    /// Write `content` to `name` under `root`; returns the absolute path.
    pub(crate) fn source_file(root: &str, name: &str, content: &str) -> String {
        let path = std::path::Path::new(root).join(name);
        std::fs::write(&path, content).expect("fixture");
        path.to_string_lossy().into_owned()
    }

    /// What a handler does that derives its output from a project source:
    /// one read through the admitted reader, which publishes the source.
    pub(crate) fn acquire_source(root: &str, path: &str) {
        crate::tools::ctx_read::read_file_for_tool_rooted(path, root, "ctx_read")
            .expect("admitted source");
    }

    /// The lineage on record for exactly `printed`, looked up and judged
    /// inside the project scope, as the proxy does. `None`: not registered.
    pub(crate) fn lineage(root: &str, printed: &str) -> Option<bool> {
        crate::tools::admitted_sources::as_agent_acquisition(root, || {
            crate::core::archive::delivered::lineage_of(&crate::core::archive::delivered::digest(
                printed,
            ))
            .map(|lineage| lineage.still_admitted().is_ok())
        })
    }

    /// The daemon route switched off for the lifetime of the guard.
    pub(crate) struct NoDaemon;

    impl NoDaemon {
        pub(crate) fn set() -> Self {
            crate::test_env::set_var("__LEAN_CTX_NO_DAEMON", "1");
            Self
        }
    }

    impl Drop for NoDaemon {
        fn drop(&mut self) {
            crate::test_env::remove_var("__LEAN_CTX_NO_DAEMON");
        }
    }
}

#[cfg(test)]
#[path = "common_tests.rs"]
mod tests;
