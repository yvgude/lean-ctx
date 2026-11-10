// SPDX-License-Identifier: Apache-2.0
pub(crate) use lean_ctx::core::command_root::{
    detect_project_root, promote_to_git_root, require_project_root,
};
pub(crate) use lean_ctx::core::shell_history::load_shell_history;

pub(crate) fn print_savings(original: usize, sent: usize) {
    let footer = lean_ctx::core::protocol::format_savings(original, sent);
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
        Option<std::sync::Arc<lean_ctx::core::context_admission::capture::AdmissionCapture>>,
    >,
    /// The receipt duty of this run and the ONE policy snapshot it was
    /// decided under, taken where the run starts.
    duty: std::sync::OnceLock<lean_ctx::core::context_admission::receipt_store::Duty>,
    /// The receipt index the output is listed under when it is not the
    /// project's own ([`Self::receipt_index`]).
    receipt_index: Option<&'a str>,
    /// Where the command hands the output: stdout unless a builder says
    /// otherwise ([`Self::to_stderr`], [`Self::to_file`],
    /// [`Self::to_clipboard`]).
    channel: lean_ctx::core::context_admission::receipt_store::trace::Channel,
    /// The Ed25519 public key a package signature was VERIFIED with, when the
    /// document names it ([`Self::verified_signer_key`]).
    verified_signer_key: Option<&'a str>,
    /// The answer a document withheld as a whole gets on the run's channel
    /// ([`Self::refuse_with`]); `None`: its reason on stderr.
    refusal: Option<fn(&str) -> String>,
    /// The document is a list of independent records
    /// ([`Self::per_record`]); `None`: it is released as one whole.
    records: Option<Records>,
}

/// Where the independent records of a document stand, and how the document
/// reports the ones the release withheld (#1298).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Records {
    /// Object keys from the document's root to the array of records; empty:
    /// the document itself is the array.
    pub path: &'static [&'static str],
    /// Writes the number of withheld records (> 0) into the document before
    /// the document itself is released. `None`: the count is named only in
    /// the content-free notice.
    pub report: Option<fn(&mut serde_json::Value, usize)>,
}

/// The content-free notice for `count` records left out of a document.
pub(crate) fn withheld_records_notice(count: usize) -> String {
    format!("[lean-ctx gateway: {count} record(s) withheld]")
}

/// The array `path` leads to in `value` (`value` itself for an empty path).
fn records_at<'v>(
    value: &'v mut serde_json::Value,
    path: &[&str],
) -> Option<&'v mut Vec<serde_json::Value>> {
    let mut at = value;
    for key in path {
        at = at.get_mut(*key)?;
    }
    at.as_array_mut()
}

/// The content-free reason a document is withheld for when its policy view
/// could not be held: the authority changed while it was released, the
/// project could not be resolved, the policy could not be read in time, or
/// the policy itself did not verify. Every one of them withholds.
fn policy_view_refusal(error: &str) -> &'static str {
    if error.contains("changed") || error.contains("no longer active") {
        "policy.changed"
    } else if error.contains("deadline") || error.contains("busy") {
        "policy.unavailable"
    } else if error.contains("root") || error.contains("project_unresolved") {
        "policy.project_unavailable"
    } else {
        "policy.unverified"
    }
}

/// The parts of `original` that masking replaced to give `masked`: the
/// text each `[REDACTED…]` marker stands for. Masking keeps everything else
/// in order, so the literal pieces between markers are found in `original`
/// one after the other and what lies between them is what was masked. A
/// masked text that does not line up that way (another marker form, a
/// notice in front) yields the one span between their common prefix and
/// suffix — wider than the secret, never narrower.
fn masked_spans(original: &str, masked: &str) -> Vec<String> {
    const MARKER: &str = "[REDACTED";
    let mut pieces = Vec::new();
    let mut rest = masked;
    while let Some(start) = rest.find(MARKER) {
        pieces.push(&rest[..start]);
        rest = rest[start..]
            .find(']')
            .map_or("", |end| &rest[start + end + 1..]);
    }
    pieces.push(rest);
    let aligned = aligned_spans(original, &pieces);
    aligned.unwrap_or_else(|| {
        let prefix = original
            .char_indices()
            .zip(masked.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map_or(0, |((index, ch), _)| index + ch.len_utf8());
        let suffix = original[prefix..]
            .chars()
            .rev()
            .zip(masked[prefix.min(masked.len())..].chars().rev())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum::<usize>();
        let end = original.len() - suffix;
        if prefix < end {
            vec![original[prefix..end].to_owned()]
        } else {
            Vec::new()
        }
    })
}

/// [`masked_spans`] when the literal pieces of the masked text line up in
/// `original`: the text between consecutive pieces. `None` otherwise.
fn aligned_spans(original: &str, pieces: &[&str]) -> Option<Vec<String>> {
    let (first, last) = (pieces.first()?, pieces.last()?);
    if pieces.len() < 2
        || pieces[1..pieces.len() - 1]
            .iter()
            .any(|piece| piece.is_empty())
        || !original.starts_with(first)
        || !original.ends_with(last)
        || original.len() < first.len() + last.len()
    {
        return None;
    }
    let tail_start = original.len() - last.len();
    let mut at = first.len();
    let mut spans = Vec::new();
    for piece in &pieces[1..pieces.len() - 1] {
        let found = at + 1 + original.get(at + 1..tail_start)?.find(piece)?;
        spans.push(original[at..found].to_owned());
        at = found + piece.len();
    }
    if at >= tail_start {
        return None;
    }
    spans.push(original[at..tail_start].to_owned());
    Some(spans)
}

/// Whether the serialized document still holds any text masking replaced
/// ([`masked_spans`]), as written or as JSON escapes it. Spans shorter than
/// four characters are not looked for: no rule masks so little, and they
/// would match ordinary text.
pub(crate) fn still_carries_a_masked_value(document: &str, originals: &[String]) -> bool {
    originals
        .iter()
        .filter(|original| original.chars().count() >= 4)
        .any(|original| {
            let escaped = serde_json::to_string(original).unwrap_or_default();
            let escaped = escaped
                .strip_prefix('"')
                .and_then(|inner| inner.strip_suffix('"'))
                .unwrap_or(&escaped);
            document.contains(original.as_str()) || document.contains(escaped)
        })
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
            channel: lean_ctx::core::context_admission::receipt_store::trace::Channel::Stdout,
            verified_signer_key: None,
            refusal: None,
            records: None,
        }
    }

    /// The document is a list of independent records, such as the facts of
    /// a knowledge listing or export. When it cannot be released as a whole,
    /// every record is released on its own under the same floor: a record
    /// the release would withhold, or that still carries a value masked in
    /// another record, is left out and counted; the others are released.
    /// The document around them is released as always, and the result must
    /// pass the release point unchanged, or it is withheld as a whole
    /// (#1298: one session id in one fact key withheld every fact).
    pub(crate) fn per_record(mut self, records: Records) -> Self {
        self.records = Some(records);
        self
    }

    /// The document names the public key the Engine has just verified a
    /// package signature with (`"signer_public_key"`). The release then runs
    /// inside [`lean_ctx::core::redaction::with_verified_signer_key`]: the
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
        self.channel = lean_ctx::core::context_admission::receipt_store::trace::Channel::Stderr;
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
        self.channel = lean_ctx::core::context_admission::receipt_store::trace::Channel::File;
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
        self.channel = lean_ctx::core::context_admission::receipt_store::trace::Channel::Clipboard;
        self
    }

    /// The local service hands this run's output to its socket transport as
    /// a response body (`lean-ctx serve --daemon`, Local Service v1): the
    /// receipt is prepared for the channel `mcp_transport`, for the text and
    /// the final `\n` the response carries, as on stdout.
    #[expect(
        clippy::wrong_self_convention,
        reason = "builder that redirects the run"
    )]
    pub(crate) fn to_transport(mut self) -> Self {
        self.channel =
            lean_ctx::core::context_admission::receipt_store::trace::Channel::McpTransport;
        self
    }

    /// A document withheld as a whole is answered on the run's channel (the
    /// transport, or stdout) with `render(reason)` (a content-free refusal
    /// document) instead of the reason on stderr. The run prepares and
    /// receipts THAT answer and its final `\n` for that channel — one
    /// receipt, for the bytes sent; [`Self::deliver_json`] still returns
    /// `Err(reason)`, and the caller sends `render(reason)` (on stdout with
    /// [`exit_refused`]).
    pub(crate) fn refuse_with(mut self, render: fn(&str) -> String) -> Self {
        self.refusal = Some(render);
        self
    }

    /// The channel this run's output is prepared for.
    fn output_channel(&self) -> lean_ctx::core::context_admission::receipt_store::trace::Channel {
        self.channel
    }

    /// Whether the writer of this run's channel adds one final `\n` to the
    /// returned text (the stream writers do; a file or the clipboard takes
    /// exactly the text).
    fn writer_adds_newline(&self) -> bool {
        use lean_ctx::core::context_admission::receipt_store::trace::Channel;
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
        use lean_ctx::core::context_admission::capture;
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
    fn duty(&self) -> &lean_ctx::core::context_admission::receipt_store::Duty {
        self.duty
            .get_or_init(lean_ctx::core::context_admission::receipt_store::Duty::at_entry)
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
        self.mandatory().then(|| {
            lean_ctx::core::context_admission::receipt_store::output_digest(text.as_bytes())
        })
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
        channel: lean_ctx::core::context_admission::receipt_store::trace::Channel,
    ) -> bool {
        use lean_ctx::core::context_admission::{capture::Delivered, receipt_store};
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
        let presenter_ref = lean_ctx::core::archive::presented::shown_presenter_reference();
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
                tokens: lean_ctx::core::tokens::count_tokens(written) as u64,
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
    ) -> (
        T,
        Option<lean_ctx::core::archive::authority::ArchiveAuthority>,
    ) {
        lean_ctx_server::tools::admitted_sources::as_agent_acquisition(self.project_root, || {
            self.captured(|| lean_ctx::core::archive::authority::capture_sync(handler))
        })
    }

    /// Steps 2 and 3 for a text acquired under [`Self::acquire`]: release,
    /// redaction, lineage record of the returned text. The caller prints the
    /// result with [`print_released`] and changes nothing in it.
    pub(crate) fn finish(
        &self,
        printable: Printable,
        capture: Option<lean_ctx::core::archive::authority::ArchiveAuthority>,
    ) -> String {
        self.finish_checked(printable, capture, false).0
    }

    /// The one body of steps 2 and 3, for [`Self::finish`] and
    /// [`Self::run_checked`]: the text to print, and whether the release
    /// point refused it.
    fn finish_checked(
        &self,
        printable: Printable,
        capture: Option<lean_ctx::core::archive::authority::ArchiveAuthority>,
        refusal_on_stderr: bool,
    ) -> (String, bool) {
        lean_ctx_server::tools::admitted_sources::as_agent_acquisition(self.project_root, || {
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
            let stderr = lean_ctx::core::context_admission::receipt_store::trace::Channel::Stderr;
            let (handed, channel) = if refused && (refusal_on_stderr || !self.writer_adds_newline())
            {
                (with_newline.as_str(), stderr)
            } else if self.writer_adds_newline() {
                (with_newline.as_str(), self.output_channel())
            } else {
                (printed.as_str(), self.output_channel())
            };
            if !self.receipted(handed, failed, prepared, channel) {
                printed = lean_ctx::core::context_admission::receipt_store::not_persisted_notice();
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
                    && lean_ctx_server::tools::registered::ctx_call::declares_every_source(
                        &lean_ctx_server::server::registry::build_registry(),
                        self.tool,
                        self.arguments.as_ref(),
                    );
                lean_ctx::core::archive::delivered::record_tool_delivery(
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
        let released = lean_ctx_server::server::policy_guard::release_result(
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
            lean_ctx_server::server::policy_guard::redact_model_text(&released_text)
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
        use lean_ctx::core::context_admission::receipt_store::trace::Channel;
        let (document, _capture) = self.acquire(handler);
        let prepared = lean_ctx_server::tools::admitted_sources::as_agent_acquisition(
            self.project_root,
            || self.prepared_digest(&document),
        );
        let released = self.release_document(document);
        // G08: the receipt binds exactly the bytes the writer writes — the
        // document, and the final `\n` where the writer adds one. A
        // withheld document prepares nothing of it: its reason is prepared
        // for stderr, or — with [`Self::refuse_with`] — the refusal document
        // for the run's own channel. That those bytes were written, and the
        // status the command ends with, is recorded by [`exit_withheld`] /
        // [`exit_refused`] when they have written them — never inferred
        // from this `Err`.
        let (written, is_error, channel) = match &released {
            Ok((document, _)) if final_newline => {
                (format!("{document}\n"), false, self.output_channel())
            }
            Ok((document, _)) => (document.clone(), false, self.output_channel()),
            Err(reason) => match self.refusal {
                Some(render) => (format!("{}\n", render(reason)), true, self.output_channel()),
                None => (format!("{reason}\n"), true, Channel::Stderr),
            },
        };
        let receipted = lean_ctx_server::tools::admitted_sources::as_agent_acquisition(
            self.project_root,
            || self.receipted(&written, is_error, prepared, channel),
        );
        if !receipted {
            return Err(lean_ctx::core::context_admission::receipt_store::not_persisted_notice());
        }
        let (document, notice) = released?;
        // A transport run's reader learns of a masked value from its caller
        // (the local service), not from the daemon's stderr.
        if let Some(notice) = notice
            && self.channel != Channel::McpTransport
        {
            eprintln!("{notice}");
        }
        Ok(document)
    }

    /// Whether `document` passes this run's release point, asked BEFORE a
    /// write runs (G08: a write the release would withhold writes nothing).
    /// No receipt and no output: the run's one receipt binds the bytes it
    /// sends afterwards. Call it inside the handler of
    /// [`Self::deliver_json`], so it reads the run's one policy snapshot.
    pub(crate) fn would_release(&self, document: String) -> Result<(), String> {
        self.release_document(document).map(|_| ())
    }

    /// [`Self::would_release`] with the released document: the bytes
    /// [`Self::deliver_json`] would hand out for `document`, without a
    /// receipt and without output. For a caller that decides which of
    /// several documents it delivers before the first one is receipted.
    pub(crate) fn released_preview(&self, document: String) -> Result<String, String> {
        self.release_document(document)
            .map(|(released, _)| released)
    }

    /// [`Self::deliver_json`] without the handler run and without stderr: the
    /// released document and, when something was masked in it, the notice.
    /// Runs inside the run's admission capture (G08).
    fn release_document(&self, document: String) -> Result<(String, Option<String>), String> {
        self.in_release_scope(|| self.release_document_scoped(document))
    }

    /// The texts the release would mask or withhold anywhere in the JSON
    /// `document` ([`Self::masked_values`]; a text that is not JSON is one
    /// value), asked under this run's policy view and capture as
    /// [`Self::would_release`] asks: no receipt, no output. For documents
    /// released one by one (a `jsonl` export): what one of them hides must
    /// not reach the reader through another.
    pub(crate) fn masked_values_of(&self, document: &str) -> Result<Vec<String>, String> {
        self.in_release_scope(|| {
            let gate = lean_ctx::core::context_admission::carrier::ReleaseGate::current();
            let value = serde_json::from_str::<serde_json::Value>(document)
                .unwrap_or_else(|_| serde_json::Value::String(document.to_owned()));
            let mut out = Vec::new();
            self.masked_values(&gate, &value, 0, &mut out);
            Ok(out)
        })
    }

    /// Run `operation` where the release point runs: inside the request
    /// scope, the run's admission capture, the verified signer key (if any)
    /// and one verified policy view of the project.
    fn in_release_scope<T>(
        &self,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let release = || {
            lean_ctx_server::tools::admitted_sources::as_agent_acquisition(
                self.project_root,
                || self.captured(operation),
            )
        };
        let release = || match self.verified_signer_key {
            Some(key) => lean_ctx::core::redaction::with_verified_signer_key(key, release),
            None => release(),
        };
        // One verified policy snapshot for the whole document. A document
        // that needs masking is released value by value, and every release
        // asked the policy worker for a fresh snapshot (re-reading policy,
        // trust store, marker and Hive filters): 44,660 string values of a
        // 3,504-fact knowledge export took 100–200 s. Inside the view the
        // pack rules are read once; the view is verified again before the
        // document is handed out, and a change in between withholds it.
        if !std::path::Path::new(self.project_root).is_dir() {
            return release();
        }
        lean_ctx::core::policy::runtime::with_project_source_view(self.project_root, release)
            .unwrap_or_else(|error| {
                Err(lean_ctx::core::context_admission::carrier::Withheld::new(&[
                    policy_view_refusal(&error),
                ])
                .notice())
            })
    }

    fn release_document_scoped(
        &self,
        document: String,
    ) -> Result<(String, Option<String>), String> {
        use lean_ctx::core::context_admission::carrier::{ReleaseGate, Withheld};
        lean_ctx_server::tools::admitted_sources::as_agent_acquisition(self.project_root, || {
            // The whole document through the one release sequence first: an
            // admitted document passes unchanged and keeps its bytes (#498).
            let (whole, refused) = self.released(document.clone());
            if !refused && whole == document {
                return Ok((document, None));
            }
            // Something has to be masked. Masking the serialized text could
            // cut through its syntax, so every string value is released on
            // its own and the document is serialized again around them.
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&document) else {
                return Err(if refused {
                    whole
                } else {
                    Withheld::new(&["release.unadmitted_content"]).notice()
                });
            };
            let gate = ReleaseGate::current();
            let pretty = document.contains('\n');
            let (masked, notice) = if let Some(records) = self.records {
                self.release_records(&gate, value, pretty, records)?
            } else {
                let (masked, notice, _) = self.release_masked(&gate, value, pretty)?;
                (masked, notice)
            };
            let notice =
                notice.unwrap_or_else(|| "[lean-ctx gateway: content redacted]".to_string());
            Ok((masked, Some(notice)))
        })
    }

    /// Release `value` value by value and serialize it again: the masked
    /// document, the notice of the first masking (if any) and the original
    /// texts masking replaced. A masked original left anywhere in the
    /// document, or a result that does not pass the release point
    /// unchanged, withholds it. Must be called inside the request scope.
    fn release_masked(
        &self,
        gate: &lean_ctx::core::context_admission::carrier::ReleaseGate,
        mut value: serde_json::Value,
        pretty: bool,
    ) -> Result<(String, Option<String>, Vec<String>), String> {
        use lean_ctx::core::context_admission::carrier::Withheld;
        let unadmitted = || Withheld::new(&["release.unadmitted_content"]).notice();
        let mut notice = None;
        let mut originals = Vec::new();
        self.mask_leaves(gate, &mut value, 0, &mut notice, &mut originals)?;
        let serialized = if pretty {
            serde_json::to_string_pretty(&value)
        } else {
            serde_json::to_string(&value)
        };
        let Ok(masked) = serialized else {
            return Err(unadmitted());
        };
        // A value is masked where a rule found it; the same text under
        // another name (`{"token": S, "copy": S}`) or inside another
        // value (`"id": "decision/S"`) would still carry it. Any masked
        // original that remains anywhere withholds the document.
        if still_carries_a_masked_value(&masked, &originals) {
            return Err(unadmitted());
        }
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
        Ok((masked, notice, originals))
    }

    /// [`Self::release_masked`] record by record ([`Self::per_record`]).
    /// Every record passes [`Self::release_masked`] on its own; one that is
    /// withheld there, or that carries a value masked in another record, is
    /// left out. The document with the released records (and the count its
    /// `report` writes) then passes [`Self::release_masked`] as a whole, so
    /// nothing reaches the reader that the release point would not deliver.
    /// A document without the records array is released as a whole.
    fn release_records(
        &self,
        gate: &lean_ctx::core::context_admission::carrier::ReleaseGate,
        mut value: serde_json::Value,
        pretty: bool,
        records: Records,
    ) -> Result<(String, Option<String>), String> {
        use lean_ctx::core::context_admission::carrier::Withheld;
        let Some(items) = records_at(&mut value, records.path).map(std::mem::take) else {
            let (masked, notice, _) = self.release_masked(gate, value, pretty)?;
            return Ok((masked, notice));
        };
        let mut withheld = 0usize;
        let mut notice = None;
        let mut originals = Vec::new();
        let mut released = Vec::with_capacity(items.len());
        for item in items {
            let outcome = self
                .release_masked(gate, item.clone(), false)
                .ok()
                .and_then(|(text, line, spans)| {
                    let record = serde_json::from_str::<serde_json::Value>(&text).ok()?;
                    Some((record, text, line, spans))
                });
            if let Some((record, text, line, spans)) = outcome {
                if let Some(line) = line {
                    notice.get_or_insert(line);
                }
                originals.extend(spans);
                released.push((record, text));
            } else {
                // What the release masked or withheld in a record it left
                // out must not reach the reader through another record.
                self.masked_values(gate, &item, 0, &mut originals);
                withheld += 1;
            }
        }
        // A value masked in one record must not stand unmasked in another.
        let before = released.len();
        released.retain(|(_, text)| !still_carries_a_masked_value(text, &originals));
        withheld += before - released.len();
        let Some(slot) = records_at(&mut value, records.path) else {
            return Err(Withheld::new(&["release.unadmitted_content"]).notice());
        };
        *slot = released.into_iter().map(|(record, _)| record).collect();
        if withheld > 0
            && let Some(report) = records.report
        {
            report(&mut value, withheld);
        }
        let (masked, line, spans) = self.release_masked(gate, value, pretty)?;
        originals.extend(spans);
        if still_carries_a_masked_value(&masked, &originals) {
            return Err(Withheld::new(&["release.unadmitted_content"]).notice());
        }
        let notice = match (withheld, line.or(notice)) {
            (0, notice) => notice,
            (count, None) => Some(withheld_records_notice(count)),
            (count, Some(line)) => Some(format!("{line}\n{}", withheld_records_notice(count))),
        };
        Ok((masked, notice))
    }

    /// Every original text the release would mask or withhold anywhere in
    /// `value` — in a value, in a value read with the name in front of it
    /// ([`Self::mask_member`]), or in a member name — collected without
    /// stopping at the first refusal, unlike [`Self::mask_leaves`]. A text
    /// the release withholds is collected whole. For a record that is left
    /// out: none of these texts may reach the reader through another record.
    fn masked_values(
        &self,
        gate: &lean_ctx::core::context_admission::carrier::ReleaseGate,
        value: &serde_json::Value,
        depth: usize,
        out: &mut Vec<String>,
    ) {
        use serde_json::Value;
        const MAX_DEPTH: usize = 64;
        if depth > MAX_DEPTH {
            return;
        }
        match value {
            Value::String(text) => self.masked_text_values(gate, text, out),
            Value::Array(items) => {
                for item in items {
                    self.masked_values(gate, item, depth + 1, out);
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    self.masked_text_values(gate, key, out);
                    if let Value::String(text) = item {
                        let mut masked = text.clone();
                        if self
                            .mask_member(gate, key, &mut masked, &mut None, out)
                            .is_err()
                        {
                            out.push(text.clone());
                        }
                    }
                    self.masked_values(gate, item, depth + 1, out);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    /// [`Self::masked_values`] for one text: the spans masking replaced, or
    /// the whole text when the release withholds it.
    fn masked_text_values(
        &self,
        gate: &lean_ctx::core::context_admission::carrier::ReleaseGate,
        text: &str,
        out: &mut Vec<String>,
    ) {
        use lean_ctx::core::context_admission::carrier::Release;
        let released = match gate.text(text) {
            Release::Unchanged => text.to_owned(),
            Release::Redacted { text: masked, .. } => masked,
            Release::Withheld(_) => {
                out.push(text.to_owned());
                return;
            }
        };
        let released = if self.legacy_redaction {
            lean_ctx_server::server::policy_guard::redact_model_text(&released)
        } else {
            released
        };
        if released != text {
            out.extend(masked_spans(text, &released));
        }
    }

    /// Release every string value of `value` in place. A key is never
    /// rewritten (that would change the schema): a finding in a key, a
    /// withheld value or a document nested too deep to inspect withholds the
    /// whole document with a content-free reason.
    fn mask_leaves(
        &self,
        gate: &lean_ctx::core::context_admission::carrier::ReleaseGate,
        value: &mut serde_json::Value,
        depth: usize,
        notice: &mut Option<String>,
        originals: &mut Vec<String>,
    ) -> Result<(), String> {
        use lean_ctx::core::context_admission::carrier::{Release, Withheld};
        use serde_json::Value;
        const MAX_DEPTH: usize = 64;
        if depth > MAX_DEPTH {
            return Err(Withheld::new(&["coverage.partial"]).notice());
        }
        match value {
            Value::String(text) => {
                let original = text.clone();
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
                    *text = lean_ctx_server::server::policy_guard::redact_model_text(text);
                }
                if *text != original {
                    originals.extend(masked_spans(&original, text));
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.mask_leaves(gate, item, depth + 1, notice, originals)?;
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    if !matches!(gate.text(key), Release::Unchanged) {
                        return Err(Withheld::new(&["release.unadmitted_content"]).notice());
                    }
                    if let Value::String(text) = item {
                        self.mask_member(gate, key, text, notice, originals)?;
                    }
                    self.mask_leaves(gate, item, depth + 1, notice, originals)?;
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
        Ok(())
    }

    /// Release a string value together with the name of its member, as the
    /// one-member object `{"<name>":"<value>"}`. A rule that reads a value by
    /// the name in front of it (`key: …`, `"password": "…"`) finds that pair
    /// in the whole document but never in the value alone: masking values
    /// one by one left the finding in place, the re-check of the whole
    /// document differed, and the document was withheld — every knowledge
    /// export whose facts carry a long `key`. Here the pair is released, and
    /// the value it masked stands in the document; the name is never
    /// rewritten. A pair that is withheld, or whose released form is not
    /// that one member with a string value, withholds the document.
    fn mask_member(
        &self,
        gate: &lean_ctx::core::context_admission::carrier::ReleaseGate,
        name: &str,
        text: &mut String,
        notice: &mut Option<String>,
        originals: &mut Vec<String>,
    ) -> Result<(), String> {
        use lean_ctx::core::context_admission::carrier::{Release, Withheld};
        use serde_json::Value;
        let unadmitted = || Withheld::new(&["release.unadmitted_content"]).notice();
        let fragment = Value::Object(serde_json::Map::from_iter([(
            name.to_owned(),
            Value::String(text.clone()),
        )]))
        .to_string();
        let mut released = match gate.text(&fragment) {
            Release::Unchanged => fragment.clone(),
            Release::Redacted {
                text: masked,
                notice: line,
            } => {
                notice.get_or_insert(line);
                masked
            }
            Release::Withheld(withheld) => return Err(withheld.notice()),
        };
        if self.legacy_redaction {
            released = lean_ctx_server::server::policy_guard::redact_model_text(&released);
        }
        if released == fragment {
            return Ok(());
        }
        let Ok(Value::Object(mut masked)) = serde_json::from_str::<Value>(&released) else {
            return Err(unadmitted());
        };
        match (masked.len(), masked.remove(name)) {
            (1, Some(Value::String(value))) => {
                let original = std::mem::replace(text, value);
                originals.extend(masked_spans(&original, text));
                notice.get_or_insert_with(|| "[lean-ctx gateway: content redacted]".to_owned());
                Ok(())
            }
            _ => Err(unadmitted()),
        }
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
    use lean_ctx::core::context_admission::receipt_store::{observed, trace::Channel};
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
    use lean_ctx::core::context_admission::receipt_store::{observed, trace::Channel};
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
    use lean_ctx::core::context_admission::receipt_store::{observed, trace::Channel};
    observed::handed_over(Channel::File, bytes, None);
}

/// The command handed exactly `text` to the system clipboard and the
/// hand-off SUCCEEDED: record `handed_over` (channel `clipboard`) for the
/// receipt this thread prepared with [`CliToolRun::to_clipboard`]. Call it
/// only after that success.
pub(crate) fn confirm_copied_to_clipboard(text: &str) {
    use lean_ctx::core::context_admission::receipt_store::{observed, trace::Channel};
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

/// For a run with [`CliToolRun::refuse_with`]: write the refusal `document`
/// the run prepared for stdout (and its final `\n`), record the hand-over
/// with exit status 1, and end the process with it.
pub(crate) fn exit_refused(document: &str) -> ! {
    use lean_ctx::core::context_admission::receipt_store::{observed, trace::Channel};
    let line = format!("{document}\n");
    let _ = observed::write_observed(
        &mut std::io::stdout().lock(),
        Channel::Stdout,
        line.as_bytes(),
        Some(1),
    );
    std::process::exit(1);
}

/// Strip savings footers from daemon output when the CLI client has footer suppressed.
#[cfg(unix)]
pub(crate) fn filter_daemon_output(text: &str) -> String {
    if lean_ctx::core::protocol::savings_footer_visible() {
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

pub(crate) fn daemon_fallback_hint() {
    use std::sync::Once;
    static HINT: Once = Once::new();
    HINT.call_once(|| {
        if lean_ctx::core::protocol::meta_visible() {
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

/// Shared set-up for the tests of the CLI commands that run through
/// [`CliToolRun`]. Every test starts with the isolation lines
/// (`isolated_data_dir`, `TestPolicyOverride`) itself.
#[cfg(test)]
pub(crate) mod carrier_tests_support {
    /// A value the gateway masks in every mode.
    pub(crate) const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    pub(crate) use lean_ctx::core::config::test_support::governed;

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
        lean_ctx_server::tools::ctx_read::read_file_for_tool_rooted(path, root, "ctx_read")
            .expect("admitted source");
    }

    /// The lineage on record for exactly `printed`, looked up and judged
    /// inside the project scope, as the proxy does. `None`: not registered.
    pub(crate) fn lineage(root: &str, printed: &str) -> Option<bool> {
        lean_ctx_server::tools::admitted_sources::as_agent_acquisition(root, || {
            lean_ctx::core::archive::delivered::lineage_of(
                &lean_ctx::core::archive::delivered::digest(printed),
            )
            .map(|lineage| lineage.still_admitted().is_ok())
        })
    }

    /// The daemon route switched off for the lifetime of the guard.
    pub(crate) struct NoDaemon;

    impl NoDaemon {
        pub(crate) fn set() -> Self {
            lean_ctx::test_env::set_var("__LEAN_CTX_NO_DAEMON", "1");
            Self
        }
    }

    impl Drop for NoDaemon {
        fn drop(&mut self) {
            lean_ctx::test_env::remove_var("__LEAN_CTX_NO_DAEMON");
        }
    }
}

#[cfg(test)]
#[path = "common_tests.rs"]
mod tests;
