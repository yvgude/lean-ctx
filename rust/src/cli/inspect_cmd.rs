// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx inspect`: why the Context Gateway delivered, changed or withheld
//! what it did in the latest rounds of this project. Reads only stored,
//! digest-verified Decision Receipts — never content, never a guess.
//!
//! One delivery can be followed from its receipt (G08): the sources that were
//! considered (by digest), the policy that decided, every transformation and
//! refusal with its typed reason, the principal, the destination, the digest
//! of the final output and the receipt's anchor in the signed audit trail. A
//! receipt is found as the latest of a project, by its digest, by its task,
//! or by the delivered bytes themselves (`--output`), which are only hashed.
//! The inspector's own output passes the CLI release point like every other.

use lean_ctx_protocol::Sha256Digest;
use lean_ctx_protocol::context_gateway::{
    ContextDecisionReceiptV1, ContextDecisionV1, CoverageKindV1, DetectorSignalV1, GatewayModeV1,
    PrincipalKindV1,
};

use crate::core::context_admission::receipt_store::trace;
use crate::core::context_admission::receipt_store::{
    self, AuditAnchors, LoadError, SignatureState, StoredReceipt,
};

use crate::core::context_admission::receipt_store::view::{
    egress_lines, observation_evidence, observed_finding, observed_json, trace_finding,
    transport_observation_line,
};

/// The receipt index of the inspector's own outputs: they are receipted like
/// every CLI output, but never listed among the deliveries they report on.
const INSPECT_RECEIPT_KEY: &str = "<inspect>";
/// Shortest digest prefix `--receipt` resolves among the recent receipts.
const MIN_DIGEST_PREFIX: usize = 8;
/// Upper bound of the bytes `--output` hashes.
const MAX_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;

const DEFAULT_LAST: usize = 1;
const MAX_LAST: usize = receipt_store::LATEST_CAPACITY;

fn usage() {
    println!(
        "Why the Context Gateway delivered, changed or withheld what it did.\n\n\
         Shows the latest Decision Receipts of this project (newest first), each\n\
         verified against the digest it is stored under. Receipts hold decisions,\n\
         counts and digests only — never content.\n\n\
         Usage: lean-ctx inspect [--latest | --last <N>] [--project <DIR> | --proxy] [--json]\n\
         \x20      lean-ctx inspect --receipt <DIGEST> [--project <DIR> | --proxy] [--json]\n\
         \x20      lean-ctx inspect --output <FILE | -> [--project <DIR> | --proxy] [--json]\n\
         \x20      lean-ctx inspect --task <TASK_ID> [--project-id <ID>] [--tenant-id <ID>] [--json]\n\
         \x20      lean-ctx inspect --maintain\n\n\
         Options:\n  \
           --latest         the newest receipt (the default)\n  \
           --last <N>       show the N newest receipts (default 1, max {MAX_LAST})\n  \
           --receipt <DIGEST> one receipt by its digest (sha256:<hex>, or a prefix of a recent one)\n  \
           --output <FILE>  the receipt that binds these delivered bytes (`-` reads stdin);\n  \
           \x20                the bytes are hashed, never stored or shown\n  \
           --project <DIR>  project root (default: the current project)\n  \
           --proxy          requests the BYOK proxy forwarded to a model provider\n  \
           --task <ID>      one task's lineage: plan, deliveries, cost, outcome\n  \
           --project-id <ID> the task's project (default: the current project root)\n  \
           --tenant-id <ID> the task's tenant, for organization-admitted tasks\n  \
           --json           machine-readable output\n  \
           --maintain       pack older receipts and remove expired ones now\n\n\
         Receipts are stored packed and kept at most {retention} days and under the store's\n\
         ceiling, oldest first; the last {protect} hours, the latest receipts of every project\n\
         and the receipts of tasks in progress are always kept.\n",
        retention = crate::core::context_admission::receipt_store::retention::RETENTION_DAYS,
        protect = crate::core::context_admission::receipt_store::retention::PROTECT_HOURS,
    );
}

struct Options {
    last: usize,
    project: Option<String>,
    receipt: Option<String>,
    output: Option<String>,
    task: Option<String>,
    project_id: Option<lean_ctx_protocol::ProjectId>,
    tenant_id: Option<lean_ctx_protocol::TenantId>,
    json: bool,
    maintain: bool,
}

fn parse(args: &[String]) -> Result<Option<Options>, String> {
    let mut options = Options {
        last: DEFAULT_LAST,
        project: None,
        receipt: None,
        output: None,
        task: None,
        project_id: None,
        tenant_id: None,
        json: false,
        maintain: false,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--json" => options.json = true,
            "--maintain" => options.maintain = true,
            "--latest" => options.last = DEFAULT_LAST,
            "--receipt" => {
                let value = args.next().ok_or("--receipt needs a digest")?;
                let hex = value.strip_prefix("sha256:").unwrap_or(value);
                if !(MIN_DIGEST_PREFIX..=64).contains(&hex.len())
                    || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                {
                    return Err(format!(
                        "--receipt needs sha256:<hex> or at least {MIN_DIGEST_PREFIX} hex digits of it"
                    ));
                }
                options.receipt = Some(hex.to_owned());
            }
            "--output" => {
                options.output = Some(args.next().ok_or("--output needs a file or -")?.clone());
            }
            "--last" => {
                let value = args.next().ok_or("--last needs a number")?;
                options.last = value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=MAX_LAST).contains(n))
                    .ok_or(format!("--last must be between 1 and {MAX_LAST}"))?;
            }
            "--project" => {
                options.project = Some(args.next().ok_or("--project needs a directory")?.clone());
            }
            "--proxy" => {
                options.project =
                    Some(crate::core::context_admission::egress::PROXY_RECEIPT_KEY.to_owned());
            }
            "--task" => {
                let value = args.next().ok_or("--task needs a task id")?;
                lean_ctx_protocol::TaskId::new(value.clone())
                    .map_err(|_| "--task is not a valid task id".to_owned())?;
                options.task = Some(value.clone());
            }
            "--project-id" => {
                let value = args.next().ok_or("--project-id needs an id")?;
                options.project_id = Some(
                    lean_ctx_protocol::ProjectId::new(value.clone())
                        .map_err(|_| "--project-id is not a valid project id".to_owned())?,
                );
            }
            "--tenant-id" => {
                let value = args.next().ok_or("--tenant-id needs an id")?;
                options.tenant_id = Some(
                    lean_ctx_protocol::TenantId::new(value.clone())
                        .map_err(|_| "--tenant-id is not a valid tenant id".to_owned())?,
                );
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if options.task.is_none() && (options.project_id.is_some() || options.tenant_id.is_some()) {
        return Err("--project-id and --tenant-id only apply with --task".to_owned());
    }
    let lookups = [
        options.receipt.is_some(),
        options.output.is_some(),
        options.task.is_some(),
    ];
    if lookups.iter().filter(|chosen| **chosen).count() > 1 {
        return Err("--receipt, --output and --task are separate lookups".to_owned());
    }
    Ok(Some(options))
}

/// One maintenance pass of the gateway evidence store, in the foreground.
fn maintain() {
    use crate::core::context_admission::receipt_store::retention;
    let Some(dir) = crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|dir| dir.join("gateway"))
    else {
        eprintln!("lean-ctx inspect: no lean-ctx data directory");
        std::process::exit(1);
    };
    match retention::maintain_in(
        &dir,
        std::time::SystemTime::now(),
        retention::Policy::current(),
    ) {
        Ok(done) if done.skipped => println!("Another process is maintaining the receipt store."),
        Ok(done) => println!(
            "Receipt store: {} packed, {} receipts and {} task indexes removed, {} MB freed, {} MB held{}.",
            done.packed,
            done.removed,
            done.removed_tasks,
            done.freed_bytes / 1_048_576,
            done.bytes / 1_048_576,
            if done.full {
                " (full of protected evidence)"
            } else {
                ""
            }
        ),
        Err(error) => {
            eprintln!("lean-ctx inspect: maintenance failed: {error}");
            std::process::exit(1);
        }
    }
}

fn current_project_root() -> String {
    crate::core::config::Config::find_project_root()
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|dir| dir.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| ".".to_owned())
}

pub(crate) fn cmd_inspect(args: &[String]) {
    let options = match parse(args) {
        Ok(Some(options)) => options,
        Ok(None) => {
            usage();
            return;
        }
        Err(error) => {
            eprintln!("lean-ctx inspect: {error}");
            usage();
            std::process::exit(2);
        }
    };
    if options.maintain {
        maintain();
        return;
    }
    let scope_root = current_project_root();
    let run = super::common::CliToolRun::new("ctx_inspect", &scope_root, None)
        .human_facing()
        .receipt_index(INSPECT_RECEIPT_KEY);
    if let Some(task) = &options.task {
        // MCP tasks carry the session's project root as their project id.
        let Some(project) = options
            .project_id
            .clone()
            .or_else(|| lean_ctx_protocol::ProjectId::new(scope_root.clone()).ok())
        else {
            eprintln!("lean-ctx inspect: specify --project-id for this task");
            std::process::exit(2);
        };
        let scope = crate::core::context_store::task_scope(options.tenant_id.as_ref(), &project);
        let lineage = crate::core::context_store::lineage::load(&scope, task);
        if options.json {
            let document =
                serde_json::to_string_pretty(&lineage).unwrap_or_else(|_| "{}".to_owned());
            print_document(&run, document);
        } else {
            print_text(&run, &render_lineage(&lineage));
        }
        return;
    }
    let project = options
        .project
        .clone()
        .unwrap_or_else(|| scope_root.clone());
    let mut lookup = None;
    let receipts = if let Some(reference) = &options.receipt {
        match resolve_receipt(&project, reference) {
            Ok(entry) => vec![entry],
            Err(error) => fail(&error),
        }
    } else if let Some(source) = &options.output {
        let bytes = read_output(source).unwrap_or_else(|error| fail(&error));
        let (found_as, found) = find_by_output(&project, &bytes);
        lookup = Some(found_as);
        found
    } else {
        // The receipts of this root first, then those of the other roots of
        // the same repository — the listing the dashboard shows too.
        receipt_store::roots::latest_of_repository(&project, options.last)
    };
    let anchors = AuditAnchors::load();
    if options.json {
        print_document(&run, render_json(&receipts, &anchors, lookup.as_ref()));
        return;
    }
    let mut text = coverage_header();
    if let Some(lookup) = &lookup {
        text.push_str(&lookup.header(receipts.len()));
    }
    if lookup.is_none() || !receipts.is_empty() {
        text.push_str(&render_human(&project, &receipts, Some(&anchors)));
    }
    print_text(&run, &text);
}

fn fail(error: &str) -> ! {
    eprintln!("lean-ctx inspect: {error}");
    std::process::exit(2);
}

/// Print a human-facing text through the one CLI release path: released,
/// receipted where receipts are mandatory, then written unchanged by the
/// one observed writer.
fn print_text(run: &super::common::CliToolRun<'_>, text: &str) {
    let released = run.deliver_text(|| text.trim_end_matches('\n').to_owned());
    super::common::print_released(&released);
}

/// [`print_text`] for a machine-readable document, which is never broken: a
/// document that cannot be released is withheld as a whole.
fn print_document(run: &super::common::CliToolRun<'_>, document: String) {
    match run.deliver_json(|| document) {
        Ok(document) => super::common::print_released(&document),
        Err(reason) => super::common::exit_withheld(&reason),
    }
}

/// The receipt `reference` names: a full digest is loaded from the store, a
/// prefix is resolved among the recent receipts of `project` and of the
/// other roots of its repository.
fn resolve_receipt(
    project: &str,
    reference: &str,
) -> Result<(String, Result<StoredReceipt, LoadError>), String> {
    use receipt_store::roots;
    if reference.len() == 64 {
        return Ok((reference.to_owned(), receipt_store::load(reference)));
    }
    let every_root = MAX_LAST * (roots::MAX_REPOSITORY_ROOTS + 1);
    let mut matches: Vec<_> = roots::latest_of_repository(project, every_root)
        .into_iter()
        .filter(|(hex, _)| hex.starts_with(reference))
        .collect();
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(format!(
            "no recent receipt of this project starts with {reference}; pass the full digest"
        )),
        _ => Err(format!(
            "{reference} is ambiguous among the recent receipts; pass more digits"
        )),
    }
}

/// The delivered bytes to look up: a file, or stdin for `-`. They are hashed
/// and dropped; nothing of them is stored or printed.
fn read_output(source: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    let read = if source == "-" {
        std::io::stdin()
            .lock()
            .take(MAX_OUTPUT_BYTES + 1)
            .read_to_end(&mut bytes)
    } else {
        std::fs::File::open(source)
            .and_then(|file| file.take(MAX_OUTPUT_BYTES + 1).read_to_end(&mut bytes))
    };
    read.map_err(|error| format!("--output could not be read: {}", error.kind()))?;
    if bytes.len() as u64 > MAX_OUTPUT_BYTES {
        return Err("--output is larger than the inspector hashes".to_owned());
    }
    Ok(bytes)
}

/// How `--output` found its receipts.
struct OutputLookup {
    /// The digest of the bytes as they were given.
    given: Sha256Digest,
    /// Which form of the bytes a receipt binds; `None`: no receipt does.
    matched: Option<(&'static str, Sha256Digest)>,
}

impl OutputLookup {
    fn header(&self, receipts: usize) -> String {
        match &self.matched {
            Some((form, digest)) => format!(
                "Output {} — {receipts} receipt(s) bind these bytes ({form})\n\n",
                digest.as_str()
            ),
            None => format!(
                "Output {} — no stored receipt binds these bytes, as given or with or \
                 without a final newline.\n\
                 A receipt names its output only when the delivery was returned; a withheld \
                 or failed delivery names none.\n",
                self.given.as_str()
            ),
        }
    }
}

/// The receipts whose final context is `bytes`. Hosts differ in whether they
/// keep the final newline of a tool result, so the bytes are tried as given,
/// with one final newline added and with one removed — in that order.
fn find_by_output(
    project: &str,
    bytes: &[u8],
) -> (
    OutputLookup,
    Vec<(String, Result<StoredReceipt, LoadError>)>,
) {
    let given = receipt_store::output_digest(bytes);
    let mut forms = vec![("as given", given.clone())];
    let mut with_newline = bytes.to_vec();
    with_newline.push(b'\n');
    let with_newline = receipt_store::output_digest(&with_newline);
    forms.push(("with a final newline", with_newline));
    if let Some(trimmed) = bytes.strip_suffix(b"\n") {
        let trimmed = receipt_store::output_digest(trimmed);
        forms.push(("without the final newline", trimmed));
    }
    for (form, digest) in forms {
        let found = receipt_store::by_output(project, &digest);
        if !found.is_empty() {
            let lookup = OutputLookup {
                given,
                matched: Some((form, digest)),
            };
            return (lookup, found);
        }
    }
    let lookup = OutputLookup {
        given,
        matched: None,
    };
    (lookup, Vec::new())
}

/// One line on what the gateway can see on this machine, so a receipt is
/// never read as covering more than it does (G7).
fn coverage_header() -> String {
    let hosts = dirs::home_dir()
        .map(|home| crate::doctor::configured_host_coverage(&home))
        .filter(|coverage| !coverage.is_empty())
        .map(|coverage| {
            let hosts = coverage
                .iter()
                .map(|host| format!("{} {}", host.host, host.level.as_str()))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Gateway coverage on this machine: {hosts} (details: lean-ctx doctor)\n")
        })
        .unwrap_or_default();
    let organization = organization_record_note(&crate::core::policy::org::recorded_requirement());
    if hosts.is_empty() && organization.is_empty() {
        return String::new();
    }
    format!("{hosts}{organization}\n")
}

/// What this machine has on record about an organization policy today, and
/// how far that reaches into the receipts shown below — two different things.
///
/// The record is today's stored state of this machine. It is not read into
/// any receipt: a receipt says what its own trace recorded when it was
/// prepared. And a receipt whose trace has no `org policy` line documents no
/// organization verification — no more than that. It does not say the
/// delivery was unchecked or ungoverned: its policy snapshot, its pack, the
/// version-one floor it may have been decided under, its signature and its
/// audit anchor stand as they are.
fn organization_record_note(recorded: &crate::core::policy::org::RecordedRequirement) -> String {
    use crate::core::policy::org::RecordedRequirement;
    let record = match recorded {
        RecordedRequirement::Absent => return String::new(),
        RecordedRequirement::Unavailable(_) => {
            "UNAVAILABLE (the stored record cannot be read; governed operations refuse)".to_owned()
        }
        RecordedRequirement::Recorded {
            required, sequence, ..
        } => format!(
            "{}, signed sequence {sequence}",
            if *required {
                "required"
            } else {
                "unpinned (requirement lifted)"
            }
        ),
    };
    format!(
        "Organization policy record on this machine today: {record} — the stored record, not \
         verified by this view and not a statement about the receipts below (details: lean-ctx \
         policy org status).\n\
         A receipt names an organization decision only in an `org policy` line of its trace. \
         Without that line no organization verification is documented in that receipt; this \
         does not mean the delivery was unchecked, and its policy snapshot, pack, signature and \
         audit anchor are unaffected.\n"
    )
}

/// The receipt's anchor in the signed audit trail, as far as this machine
/// can check it: the entry is looked up by the digest the stored receipt was
/// just re-verified under, so a found entry names exactly this receipt.
fn anchor_json(stored: &StoredReceipt, anchors: &AuditAnchors) -> serde_json::Value {
    let anchor = anchors.of(stored.digest.hex());
    let signature = anchor.map(|anchor| signature_words(anchor.signature));
    serde_json::json!({
        "anchored": anchor.is_some(),
        "expected": anchor_expected(&stored.receipt),
        "audit_entry_hash": anchor.map(|anchor| anchor.entry_hash.as_str()),
        "audit_entry_position": anchor.map(|anchor| anchor.position),
        "signature": signature.map(|(state, _)| state),
        "signature_detail": signature.map(|(_, detail)| detail),
        "chain_covers_entry": anchor.map(|anchor| anchor.chain_verified),
        "trail_present": anchors.trail_present,
        "trail_unavailable": anchors.trail_unavailable,
        "trail_chain_verified": anchors.chain_valid,
        "trail_chain_verified_through": anchors.verified_through,
        "trail_entries": anchors.entries_total,
    })
}

/// The state of an audit entry's signature in one word and one sentence.
/// "verified" is a real Ed25519 check, never the mere presence of a field.
fn signature_words(state: SignatureState) -> (&'static str, &'static str) {
    match state {
        SignatureState::Verified => (
            "verified",
            "Ed25519 signature over the entry hash verified with this machine's audit key",
        ),
        SignatureState::Unverifiable(why) => ("present_not_verifiable", why),
        SignatureState::Absent => ("absent", "the entry carries no signature"),
        SignatureState::Invalid => (
            "INVALID",
            "the signature does not verify with this machine's audit key",
        ),
    }
}

/// A receipt recorded under protection is always anchored — MCP, CLI and
/// proxy egress alike; one recorded in developer mode when the delivery
/// recorded a security action.
fn anchor_expected(receipt: &ContextDecisionReceiptV1) -> bool {
    receipt.mode != GatewayModeV1::Developer || records_security_action(receipt)
}

fn trace_json(stored: &StoredReceipt, anchors: &AuditAnchors) -> serde_json::Value {
    let (state, digest, sidecar) = trace_finding(stored, Some(anchors));
    serde_json::json!({
        "state": state,
        "stored_bytes_sha256": digest,
        "sidecar": sidecar,
    })
}

fn counted(map: &std::collections::BTreeMap<String, u32>) -> String {
    map.iter()
        .map(|(name, count)| format!("{name} ×{count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The sidecar in the human view: what V1 cannot say. Everything in it was
/// PREPARED before a byte left; what was observed afterwards is its own
/// line, from its own record.
fn render_trace(text: &mut String, stored: &StoredReceipt, anchors: Option<&AuditAnchors>) {
    text.push_str(&format!(
        "  observed   {}\n\
         \x20 evidence   signed: only the audit anchor of receipt and sidecar, which says \
         PREPARED · observation: {}\n",
        observed_finding(stored).1,
        observation_evidence(stored, anchors).1,
    ));
    let (state, _, sidecar) = trace_finding(stored, anchors);
    let sentence = match state {
        "verified" => "bound to this receipt, stored bytes match the audit anchor",
        "ALTERED" => "ALTERED — the stored bytes do not match the digest in the audit anchor",
        "present_not_anchored" => "bound to this receipt, NOT anchored in the audit trail",
        "MISMATCH" => "MISMATCH — the record filed under this receipt names another receipt",
        "UNREADABLE" => "UNREADABLE — the record is not a trace sidecar of a known schema",
        "MISSING" => {
            "MISSING — this receipt owes a trace sidecar and none is stored (receipts of \
             versions before the sidecar have none)"
        }
        _ => "none — not owed (developer mode)",
    };
    text.push_str(&format!("  trace      {sentence}\n"));
    let Some(sidecar) = sidecar else {
        return;
    };
    let pack = match (&sidecar.pack_name, &sidecar.pack_version) {
        (Some(name), Some(version)) => format!("policy pack {name} {version}"),
        _ => "no policy pack".to_owned(),
    };
    text.push_str(&format!(
        "  transport  {} · {} · {pack}\n",
        serde_name(&sidecar.transport),
        sidecar.endpoint,
    ));
    if sidecar.unsourced {
        text.push_str("  sources    delivery without an admitted source decision\n");
    }
    match (&sidecar.policy_snapshot, sidecar.policy_diverged) {
        (Some(snapshot), false) => text.push_str(&format!(
            "  snapshot   one policy snapshot from the call's entry: {snapshot}\n"
        )),
        (Some(snapshot), true) => text.push_str(&format!(
            "  snapshot   {snapshot} — DIVERGED: an admission of this call was decided under \
             other rules than the snapshot of its entry\n"
        )),
        (None, _) => {}
    }
    // The organization decision as the sidecar recorded it when the delivery
    // was prepared. It is shown as a record: this view verifies the sidecar's
    // binding and anchor (the `trace` line above), not today's authority. The
    // signed policy version is the organization's; the pack version in the
    // transport line is the merged pack's own word.
    let standing = if state == "verified" {
        String::new()
    } else {
        format!(" — from a sidecar whose state is {state}, see the trace line")
    };
    match (
        &sidecar.org_policy,
        sidecar.org_policy_diverged,
        sidecar.org_policy_unavailable,
    ) {
        (Some(observed), _, _) => {
            let until = observed
                .valid_until
                .as_deref()
                .map_or_else(String::new, |until| format!(" · signed until {until}"));
            text.push_str(&format!(
                "  org policy {} · signed policy version {} · statement {}{until} — recorded at \
                 preparation, not verified again now{standing}\n",
                serde_name(&observed.state),
                observed.policy_version,
                observed.statement.as_str(),
            ));
        }
        (None, true, _) => text.push_str(&format!(
            "  org policy DIVERGED — the admissions of this delivery rest on different \
             organization decisions; none is named{standing}\n"
        )),
        (None, false, true) => text.push_str(&format!(
            "  org policy not available on this path — no verified view was held and no \
             verifier was asked; nothing about an organization decision is recorded or \
             enforced here{standing}\n"
        )),
        (None, false, false) => {}
    }
    for (number, object) in sidecar.objects.iter().enumerate() {
        let reference = match &object.source {
            Some(trace::SourceRef::Path(name) | trace::SourceRef::Id(name)) => name.as_str(),
            Some(trace::SourceRef::Withheld) => "withheld",
            None => continue,
        };
        text.push_str(&format!(
            "  source {}   {reference} · object {}\n",
            number + 1,
            short(object.object.hex()),
        ));
    }
    if sidecar.source_references_omitted > 0 {
        text.push_str(&format!(
            "  ({} source reference(s) not shown)\n",
            sidecar.source_references_omitted
        ));
    }
    if !sidecar.recovery.is_empty() || sidecar.recovery_omitted > 0 {
        text.push_str(&format!(
            "  recovery   {} reference(s) carried by the prepared bytes: {}{}\n",
            sidecar.recovery.len() + sidecar.recovery_omitted as usize,
            sidecar.recovery.join(", "),
            if sidecar.recovery_omitted > 0 {
                " (more not listed)"
            } else {
                ""
            },
        ));
    }
    let before = sidecar.before_release.as_ref().map_or_else(
        || "not recorded for this transport".to_owned(),
        |digest| digest.as_str().to_owned(),
    );
    let prepared = &sidecar.release_prepared;
    let kind = if prepared.is_error {
        "refusal or error"
    } else {
        "output"
    };
    text.push_str(&format!(
        "  before     {before} (the bytes before the release point)\n\
         \x20 prepared   {} for {} ({kind}) — prepared before any byte left; not a hand-over\n",
        prepared.bytes.as_str(),
        serde_name(&prepared.channel),
    ));
    let totals = &sidecar.totals;
    text.push_str(&format!(
        "  classes    {} secret(s) masked · {} personal value(s) masked · {} injection \
         signal(s) · {} withheld · {} not fully inspected · {} note(s)\n",
        totals.secrets_masked,
        totals.pii_masked,
        totals.injection_signals,
        totals.withheld,
        totals.not_fully_inspected,
        totals.notes,
    ));
    if !totals.reasons.is_empty() {
        text.push_str(&format!("  reasons    {}\n", counted(&totals.reasons)));
    }
    if !totals.transformations.is_empty() {
        text.push_str(&format!(
            "  transforms {}\n",
            counted(&totals.transformations)
        ));
    }
    if sidecar.objects_omitted > 0 {
        text.push_str(&format!(
            "  ({} object(s) counted in the trace without their own line)\n",
            sidecar.objects_omitted
        ));
    }
}

/// Receipts are anchored when the delivery recorded a security action.
fn records_security_action(receipt: &ContextDecisionReceiptV1) -> bool {
    let security = receipt.security;
    security.redactions > 0
        || security.injection_signals > 0
        || security.incomplete_coverage > 0
        || receipt.sources.blocked > 0
}

fn render_json(
    receipts: &[(String, Result<StoredReceipt, LoadError>)],
    anchors: &AuditAnchors,
    lookup: Option<&OutputLookup>,
) -> String {
    let items: Vec<serde_json::Value> = receipts
        .iter()
        .map(|(hex, loaded)| match loaded {
            Ok(stored) => {
                use crate::core::context_admission::receipt_store::view;
                let lineage = view::lineage(stored);
                serde_json::json!({
                    "digest": stored.digest.as_str(),
                    "verified": true,
                    "consistent": stored.receipt.validate().is_ok(),
                    "audit_anchor": anchor_json(stored, anchors),
                    "trace": trace_json(stored, anchors),
                    "observed": observed_json(stored, anchors),
                    "source_rights": view::source_rights(lineage.as_ref()),
                    "lineage": lineage,
                    "receipt": stored.receipt,
                })
            }
            Err(error) => serde_json::json!({
                "digest": format!("sha256:{hex}"),
                "verified": false,
                "error": load_error(error),
            }),
        })
        .collect();
    let mut document = serde_json::json!({ "receipts": items });
    if let Some(lookup) = lookup {
        document["output"] = serde_json::json!({
            "digest": lookup.given.as_str(),
            "matched": lookup.matched.as_ref().map(|(form, _)| form),
            "matched_digest": lookup.matched.as_ref().map(|(_, digest)| digest.as_str()),
        });
    }
    serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_owned())
}

fn load_error(error: &LoadError) -> &'static str {
    match error {
        LoadError::Missing => "missing",
        LoadError::Unreadable => "unreadable",
        LoadError::Tampered => "tampered",
    }
}

fn short(hex: &str) -> &str {
    &hex[..hex.len().min(12)]
}

fn render_human(
    project: &str,
    receipts: &[(String, Result<StoredReceipt, LoadError>)],
    anchors: Option<&AuditAnchors>,
) -> String {
    if receipts.is_empty() {
        return format!(
            "No Context Gateway receipts for {project} yet.\n\
             Receipts are written when an agent reads sources through lean-ctx.\n"
        );
    }
    let mut text = String::new();
    for (index, (hex, loaded)) in receipts.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        match loaded {
            Ok(stored) => render_receipt(&mut text, stored, anchors),
            Err(error) => text.push_str(&format!(
                "Receipt {} — NOT VERIFIED ({}): not shown\n",
                short(hex),
                load_error(error)
            )),
        }
    }
    text
}

pub(crate) fn render_lineage(
    lineage: &crate::core::context_store::lineage::TaskLineageV1,
) -> String {
    let mut text = format!("Task {} — outcome {}\n", lineage.task_id, lineage.outcome);
    if let Some(scope) = &lineage.scope {
        text.push_str(&format!("  scope      {scope}\n"));
    }
    if let Some(error) = &lineage.ledger_error {
        text.push_str(&format!(
            "  ledger     NOT VERIFIED ({error}): steps not shown\n"
        ));
    }
    for step in &lineage.steps {
        let fields: Vec<String> = step
            .fields
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        text.push_str(&format!(
            "  #{:<4} {:<26} {}\n",
            step.sequence,
            step.kind,
            fields.join(" ")
        ));
    }
    for delivery in &lineage.deliveries {
        let hex = delivery.digest.trim_start_matches("sha256:");
        match &delivery.summary {
            Some(s) => text.push_str(&format!(
                "  decision {} (verified) {} to {} · {} inspected · {} selected · {} withheld · \
                 {} redacted · {} → {} tokens\n",
                short(hex),
                s.outcome,
                s.destination,
                s.inspected,
                s.delivered,
                s.withheld,
                s.redactions,
                s.tokens_original,
                s.tokens_delivered,
            )),
            None => text.push_str(&format!(
                "  delivery {} — NOT VERIFIED ({}): not shown\n",
                short(hex),
                delivery.error.unwrap_or("unknown")
            )),
        }
        if let Some(egress) = &delivery.egress {
            if let Some(error) = egress.error {
                text.push_str(&format!("    transport evidence unavailable ({error})\n"));
            } else if egress.admission.is_none() {
                text.push_str("    egress admission unavailable; dispatch unknown\n");
            } else if egress.observations.is_empty() {
                text.push_str("    no transport observation recorded\n");
            }
            for observation in &egress.observations {
                render_transport_observation(&mut text, observation);
            }
        }
    }
    if lineage.gaps.is_empty() {
        text.push_str("  chain      complete: plan → delivery → outcome\n");
    } else {
        text.push_str(&format!("  gaps       {}\n", lineage.gaps.join(", ")));
    }
    text
}

fn serde_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Who the receipt was recorded for. A session principal is the holder of a
/// presenter capability — a reference, never an attested agent identity.
fn principal(receipt: &ContextDecisionReceiptV1) -> String {
    let id = receipt
        .principal
        .id
        .as_ref()
        .map_or("", lean_ctx_protocol::ProtocolReference::as_str);
    match receipt.principal.kind {
        PrincipalKindV1::Unknown => "unknown (no authenticated identity)".to_owned(),
        PrincipalKindV1::Session => format!("session-capability holder {id}"),
        PrincipalKindV1::Agent => id.to_owned(),
        other => format!("{} {id}", serde_name(&other)),
    }
}

/// The receipt's anchor in the signed audit trail and what verifying it
/// showed. The entry is looked up by the digest the stored receipt was just
/// recomputed to, so a found entry names exactly this receipt.
fn anchor_line(stored: &StoredReceipt, anchors: &AuditAnchors) -> String {
    if anchors.trail_unavailable {
        // Contention or an unreadable file: nothing was checked. Saying
        // "not found" or "not covered" here would report damage that the
        // trail does not have.
        return "  anchor     NOT CHECKED — the audit trail could not be read at this moment \
                (busy or unreadable); run the command again\n"
            .to_owned();
    }
    let chain = format!(
        "hash chain verified through entry {} of {}",
        anchors.verified_through, anchors.entries_total
    );
    match anchors.of(stored.digest.hex()) {
        Some(anchor) => {
            let (state, detail) = signature_words(anchor.signature);
            let covered = if anchor.chain_verified {
                "this entry is covered"
            } else {
                "this entry is NOT covered: it lies after the first broken link"
            };
            format!(
                "  anchor     audit entry {} (#{}) names this digest\n\
                 \x20 signature  {} — {detail}\n\
                 \x20 chain      {chain} — {covered}\n",
                short(&anchor.entry_hash),
                anchor.position,
                state.replace('_', " "),
            )
        }
        None if !anchors.trail_present => {
            "  anchor     none — no audit trail on this machine\n".to_owned()
        }
        None if anchor_expected(&stored.receipt) => format!(
            "  anchor     NOT FOUND — this receipt owes an audit anchor, but no entry names \
             its digest · {chain}\n"
        ),
        None => "  anchor     none — developer mode and no security action, so none was written\n"
            .to_owned(),
    }
}

fn render_receipt(text: &mut String, stored: &StoredReceipt, anchors: Option<&AuditAnchors>) {
    let receipt = &stored.receipt;
    let digest = short(stored.digest.hex());
    let verified = if receipt.validate().is_ok() {
        "verified"
    } else {
        "digest verified, receipt INCONSISTENT"
    };
    let s = receipt.sources;
    let sec = receipt.security;
    let policy = receipt
        .policy
        .as_ref()
        .map_or_else(|| "none".to_owned(), |p| short(p.digest.hex()).to_owned());
    let destination = &receipt.destination;
    let is_egress = destination.provider.as_str() != "mcp-host";
    let outcome = if is_egress
        && receipt.outcome == lean_ctx_protocol::context_gateway::DeliveryOutcomeV1::Delivered
    {
        "prepared".to_owned()
    } else {
        serde_name(&receipt.outcome)
    };
    // A receipt is stored before a byte leaves: what it counts and binds
    // was prepared. What was observed afterwards is the `observed` line.
    let boundary = if is_egress {
        "prepared"
    } else {
        "prepared for the host"
    };
    let target = destination.model.as_ref().map_or_else(
        || destination.provider.as_str().to_owned(),
        |model| format!("{} / {}", destination.provider.as_str(), model.as_str()),
    );
    let policy_identity = receipt.policy.as_ref().map_or_else(
        || "none recorded".to_owned(),
        |p| {
            let version = p
                .version
                .as_ref()
                .map(|version| format!(" {}", serde_name(version)))
                .unwrap_or_default();
            format!(
                "{}{version} · fingerprint {}",
                p.id.as_str(),
                p.digest.as_str()
            )
        },
    );
    let task = receipt
        .task
        .as_ref()
        .map(|task| format!("  task       {}\n", task.as_str()))
        .unwrap_or_default();
    text.push_str(&format!(
        "Context Gateway receipt {digest} ({verified})\n\
         \x20 digest     {}\n\
         \x20 receipt    {}\n\
         \x20 principal  {}\n\
         {task}\
         \x20 rule       {policy_identity}\n",
        stored.digest.as_str(),
        receipt.receipt_id.as_str(),
        principal(receipt),
    ));
    text.push_str(&format!(
        "\x20 outcome    {} (recorded at preparation, not an observed delivery) · mode {} · \
         policy {policy}\n\
         \x20 to         {target} ({})\n\
         \x20 objects    {} inspected · {} {boundary} · {} withheld\n\
         \x20 security   {} value(s) redacted · {} injection signal(s) · {} not fully inspected\n\
         \x20 tokens     {} original → {} {boundary}\n",
        outcome,
        serde_name(&receipt.mode),
        serde_name(&destination.locality),
        s.inspected,
        s.selected,
        s.blocked,
        sec.redactions,
        sec.injection_signals,
        sec.incomplete_coverage,
        receipt.tokens.original,
        receipt.tokens.delivered,
    ));
    match &receipt.final_context {
        Some(output) => text.push_str(&format!(
            "  output     {} — the bytes {boundary}\n",
            output.as_str()
        )),
        None => text.push_str(&format!(
            "  output     none — nothing was {boundary} ({outcome})\n"
        )),
    }
    // The right to read the sources is stated only as far as a check was
    // recorded; without one it is an open question, not a permission (E07).
    let lineage = crate::core::context_admission::receipt_store::view::lineage(stored);
    text.push_str(
        match crate::core::context_admission::receipt_store::view::source_rights(lineage.as_ref())
        {
            "checked" => "  rights     every source re-checked for this principal\n",
            "unsupported" => {
                "  rights     UNPROVEN — a source could not be re-checked for this principal\n"
            }
            _ => "  rights     not recorded — this receipt does not prove the right to read its sources\n",
        },
    );
    if let Some(quality) = &receipt.quality {
        let critical = quality.retention.critical;
        text.push_str(&format!(
            "  quality    {} · critical facts {} retained · {} recoverable · {} lost · \
             {} secret line(s) withheld\n",
            serde_name(&quality.overall),
            critical.retained,
            critical.recoverable,
            critical.lost,
            quality.retention.secret_lines_withheld,
        ));
        if let Some(recovery) = &quality.recovery {
            text.push_str(&format!(
                "  recovery   {} handle(s) emitted · {} verified · {} failure(s)\n",
                recovery.handles_emitted, recovery.handles_verified, recovery.failures,
            ));
        }
    }
    if let Some(anchors) = anchors {
        text.push_str(&anchor_line(stored, anchors));
    }
    render_trace(text, stored, anchors);
    if is_egress {
        // The egress lifecycle, in the words every view of a receipt shows.
        for line in egress_lines(stored) {
            text.push_str(&format!("  {line}\n"));
        }
    } else if let Some(metric) = crate::core::context_admission::hud::compact_metric(receipt) {
        text.push_str(&format!("  hud        {metric}\n"));
    }
    let itemized = receipt.decisions.len();
    if (s.inspected as usize) > itemized {
        text.push_str(&format!(
            "  ({itemized} decisions itemized, notable ones first; all {} are counted above)\n",
            s.inspected
        ));
    }
    for (index, decision) in receipt.decisions.iter().enumerate() {
        render_decision(text, index + 1, decision);
    }
}

fn render_transport_observation(
    text: &mut String,
    observation: &lean_ctx_protocol::context_gateway::EgressTransportObservationV1,
) {
    text.push_str(&format!("  {}\n", transport_observation_line(observation)));
}

fn render_decision(text: &mut String, number: usize, decision: &ContextDecisionV1) {
    // A note is a statement about a source the receipt already counts.
    let subject = if crate::core::context_admission::capture::is_note(decision) {
        "note on a source counted above".to_owned()
    } else {
        format!("object {}", short(decision.object.hex()))
    };
    text.push_str(&format!(
        "  {number}. {} · {subject}\n",
        serde_name(&decision.disposition),
    ));
    if decision.reason_codes.is_empty() {
        text.push_str("       inspected completely, nothing found\n");
    }
    if !decision.required_transformations.is_empty() {
        let kinds: Vec<String> = decision
            .required_transformations
            .iter()
            .map(serde_name)
            .collect();
        text.push_str(&format!("       transformed: {}\n", kinds.join(", ")));
    }
    for reason in &decision.reason_codes {
        text.push_str(&format!(
            "       {} — {}\n",
            reason.as_str(),
            explain(reason.as_str())
        ));
    }
    let detectors: Vec<String> = decision.signals.iter().filter_map(detector_note).collect();
    if !detectors.is_empty() {
        text.push_str(&format!("       detectors: {}\n", detectors.join(", ")));
    }
}

/// Only what is worth a reader's attention: findings and gaps.
fn detector_note(signal: &DetectorSignalV1) -> Option<String> {
    let name = signal
        .detector
        .id
        .as_str()
        .trim_start_matches("builtin.")
        .to_owned();
    // A stated reason is shown for every kind: a run that read every byte can
    // still have left a category unexamined, and a failed one says why.
    let because = signal
        .coverage
        .reason
        .as_ref()
        .map(|reason| format!(": {}", explain(reason.as_str())))
        .unwrap_or_default();
    let coverage = match signal.coverage.kind {
        kind @ (CoverageKindV1::Complete | CoverageKindV1::NotRequired) => {
            (!because.is_empty()).then(|| format!("{}{because}", serde_name(&kind)))
        }
        other => Some(format!(
            "{} ({} of {} bytes){because}",
            serde_name(&other),
            signal.coverage.bytes_inspected,
            signal.coverage.bytes_total
        )),
    };
    match (signal.evidence_count, coverage) {
        (0, None) => None,
        (0, Some(coverage)) => Some(format!("{name} {coverage}")),
        (n, None) => Some(format!("{name} {n} found")),
        (n, Some(coverage)) => Some(format!("{name} {n} found, {coverage}")),
    }
}

/// A plain sentence for each reason code the gateway emits.
pub(crate) fn explain(code: &str) -> String {
    let fixed = match code {
        "secret.redacted" => "credentials were masked before caching and compression",
        "secret.flagged" => "credentials were detected and delivered (secrets = \"warn\")",
        "secret.blocked" => "withheld: credentials found and secrets = \"block\"",
        "pii.redacted" => "checksum-valid personal data was masked",
        "pii.flagged" => "personal data was detected and delivered (pii = \"warn\")",
        "pii.blocked" => "withheld: personal data found and pii = \"block\"",
        "injection.flagged" => "prompt-injection pattern found; delivered and marked untrusted",
        "injection.redacted" => "the lines carrying a prompt-injection pattern were replaced",
        "injection.blocked" => "withheld: prompt-injection pattern and injection = \"block\"",
        "classification.flagged" => "a classification marking raised the sensitivity",
        "classification.blocked" => {
            "withheld: classification marking and classification = \"block\""
        }
        "coverage.partial" => "a detector inspected only part of the source (inspection budget)",
        "coverage.budget_exhausted" => "the inspection budget ended before the source did",
        "coverage.unsupported_media" => "no detector can read this medium (e.g. an image)",
        "detector.timed_out" => "a detector ran out of time; the source is not proven clean",
        "detector.invalid_pattern" => {
            "a configured custom pattern is invalid; that detector failed"
        }
        "redaction.incomplete" => "withheld: masking left a detectable value behind",
        "semantic.unavailable" => "the licensed semantic detector was unavailable (advisory)",
        "semantic.partial" => "the semantic detector inspected only part of the source (advisory)",
        "semantic.warming" => {
            "the semantic detector was still starting; this text was not examined by it"
        }
        "semantic.categories_not_inspected" => {
            "the installed models do not examine every category that is switched on"
        }
        "semantic.language_not_covered" => {
            "no installed model is declared for the language of this text; it was not examined"
        }
        "semantic.language_undetermined" => {
            "the language of this text could not be determined, so no model declared for \
             specific languages examined it"
        }
        "semantic.document_type_not_covered" => {
            "no installed model is declared for this kind of document (code, data or prose); \
             it was not examined"
        }
        "semantic.coverage_unknown" => {
            "the installed models do not declare which languages and document types they \
             cover; their findings are masked, but no category counts as examined"
        }
        "semantic.language_coverage_unknown" => {
            "the installed models do not declare which languages they cover"
        }
        "semantic.document_type_coverage_unknown" => {
            "the installed models do not declare which document types they cover"
        }
        "gateway.disabled" => "the Context Gateway is switched off",
        "destination.remote_restricted" => {
            "withheld: restricted content never goes to a remote model"
        }
        "destination.remote_allowed_once" => {
            "delivered: a person allowed this restricted block for this provider \
             (lean-ctx gateway allow-once)"
        }
        "egress.sealed_content" => {
            "the provider signed or encrypted this content, so it could not be masked"
        }
        "egress.opaque_payload" => "the proxy could not read this request body (opaque)",
        "delivery.no_source_decision" => {
            "the delivered bytes had no admitted source decision (unknown lineage)"
        }
        "receipt.not_persisted" => {
            "withheld: the mandatory receipt of this delivery could not be stored"
        }
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.to_owned();
    }
    if let Some(category) = code
        .strip_prefix("semantic.")
        .and_then(|rest| rest.strip_suffix(".not_inspected"))
    {
        // No finding for this category means nothing: it was not looked for.
        return format!(
            "the semantic detector did not examine {}: no installed model examines it, or none \
             is declared for this text's language or document type",
            category.replace('_', " ")
        );
    }
    if let Some(rest) = code.strip_prefix("semantic.") {
        return format!("semantic detector: {}", rest.replace(['.', '_'], " "));
    }
    "see docs/contracts/context-gateway-v1.md".to_owned()
}

#[cfg(test)]
#[path = "inspect_cmd_tests.rs"]
mod tests;
