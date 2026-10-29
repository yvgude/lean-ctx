// SPDX-License-Identifier: Apache-2.0
//! The one admitted, receipt-bound way out of the proxy.
//!
//! Every route that hands bytes to a destination goes through this module:
//!
//! 1. [`admit`] / [`admit_caller`] decide on what the caller supplied, under
//!    the global gateway configuration and — when the proxy runs under a
//!    project or organisation policy — under that policy as well. No
//!    optimisation, compression or derived store may see the body before.
//! 2. [`admit_final_with`] re-admits the body that actually leaves, at the
//!    destination it actually leaves for, after every transformation.
//! 3. [`AdmittedBody::bind`] persists the receipt for exactly those bytes and
//!    is the only constructor of an [`AdmittedBody`]; [`AdmittedBody::send`]
//!    is the only upstream HTTP send of the proxy. A route cannot put bytes
//!    on the wire that no admission produced, and it cannot choose another
//!    origin after the receipt named one.
//!
//! An admission is only as good as the policy it was decided under. Every
//! [`Admission`] carries that [`Authority`] — the global gateway policy
//! identity and the project authority — and it is checked to be the current
//! one before the final decision, before the receipt is bound and immediately
//! before every dispatch, the retry included. When it is no longer current
//! the request is refused: already masked or compressed text is never
//! re-judged under a new policy that would have needed to see the original.
//!
//! A refusal is a [`GatewayRefusal`]: a stable, content-free reason the
//! caller can read from the status, a header and the provider-shaped JSON
//! error body. It never carries protected values.
//!
//! Proxy-local adapter over `core::context_admission::egress`. Parsing reason
//! codes out of the egress refusal text, overriding `EgressOutcome::body`,
//! the policy identity in [`Authority`] and the two linked receipts of a
//! transformed request belong in that shared module once it exposes typed
//! refusals, a detachable policy epoch and a way to join two outcomes.

use std::borrow::Cow;
use std::sync::Arc;

use axum::http::request::Parts;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use lean_ctx_protocol::Sha256Digest;
use lean_ctx_protocol::context_gateway::{ClassificationV1, DestinationLocalityV1, DestinationV1};
use serde_json::Value;

use super::codec::{
    RequestBodyEncoding, decode_gzip_bounded, decode_zstd_bounded, encode_gzip, encode_zstd,
    request_body_encoding,
};
use super::request_project::{self, ProxyProject};
use super::sealed_origin::{self, Lineage, LineageSlot, SealedContext, SealedProducer};
use crate::core::config::Config;
use crate::core::context_admission::AdmissionPolicy;
use crate::core::context_admission::egress::{
    self, EgressBody, EgressOutcome, EgressTarget, TrustedDestinationBinding,
};
use crate::core::context_admission::receipt_store;
use crate::core::policy::runtime as project_policy;

/// Response header naming the refusing layer.
pub(crate) const REFUSAL_HEADER: &str = "x-leanctx-refusal";
/// Response header carrying the comma-separated reason codes.
///
/// Together with [`REFUSAL_HEADER`]: why the request was refused. Without
/// it, on a reply that was delivered (a forwarded request, `/v1/compress`):
/// the distinct `recovery.*` codes of why blocks of the request stayed
/// uncompressed under protection (`compress::KeptWhole`) — a statement, not a
/// refusal. Content-free in both uses.
pub(crate) const REASON_CODES_HEADER: &str = "x-leanctx-reason-codes";
/// Response header carrying the comma-separated ids of the withheld blocks
/// a refusal is about (`lean-ctx gateway why <id>`). Content-free.
pub(crate) const WITHHELD_BLOCKS_HEADER: &str = "x-leanctx-withheld-blocks";
const REFUSAL_KIND: &str = "context_gateway";
const FALLBACK_CODE: &str = "gateway.refused";
const RECEIPT_UNAVAILABLE: &str = "receipt.unavailable";
const POLICY_CHANGED: &str = "policy.changed";
const WITHHELD_MARKER: &str = "[lean-ctx gateway: content withheld";

fn refusal_text(code: &str) -> String {
    format!("request withheld by the context gateway ({code})")
}

/// A withheld request is answered `400`, never `403`: hosts read a 403 as an
/// authentication failure and send the user to log in again (Claude Code:
/// "authentication_failed … Please run /login"), which cannot help.
const WITHHELD_STATUS: StatusCode = StatusCode::BAD_REQUEST;

#[path = "admitted_refusal.rs"]
mod refusal;
pub(crate) use refusal::GatewayRefusal;

/// The admission policy of a configuration snapshot. An unusable global
/// configuration has no identity — nothing is admitted or sent under it.
fn usable_policy(snapshot: &Config) -> Result<AdmissionPolicy, GatewayRefusal> {
    let policy = AdmissionPolicy::from_config(snapshot);
    if policy.config_unloadable() {
        return Err(GatewayRefusal::unavailable("gateway.config_unloadable"));
    }
    Ok(policy)
}

/// The policy an admission was decided under. Content-free.
#[derive(Debug)]
pub(crate) struct Authority {
    /// The policy epoch the decision itself reported — the one identity the
    /// shared admission module defines: gateway settings, secret patterns,
    /// the effective switch, the bound semantic runtime and the load state,
    /// combined with the project fingerprint when `epoch_project` is set.
    /// The same value is re-checked at bind and before every dispatch; no
    /// narrower identity is kept beside it.
    epoch: egress::PolicyEpoch,
    /// The project fingerprint bound into `epoch` (project-bound document
    /// decisions only; `None` otherwise, as at admission).
    epoch_project: Option<[u8; 32]>,
    /// The project/organisation authority; `None` when no such policy was
    /// active, which is itself part of the identity.
    project: Option<project_policy::PublicationAuthority>,
    /// The authority the caller's content was admitted under, when this one
    /// belongs to the final admission of a transformed request. Both must
    /// stay current: transformed content is never carried by the final
    /// authority alone.
    original: Option<Box<Authority>>,
    /// The request project the decision was taken in (the proxy's configured
    /// or launch-pinned root), or `None`. Re-checked like the policy: a
    /// project that is no longer the proxy's refuses.
    scope: Option<std::path::PathBuf>,
}

impl Authority {
    /// `Ok` only while exactly this policy — and the one the caller's content
    /// was admitted under — is still the current one. A changed, added,
    /// removed or unverifiable policy and an unusable global configuration
    /// all refuse.
    pub(crate) fn verify_current(&self) -> Result<(), GatewayRefusal> {
        let project =
            request_project::resolve(&Config::load_arc()).map_err(GatewayRefusal::unavailable)?;
        if project.scope() != self.scope.as_deref() {
            return Err(GatewayRefusal::withheld(POLICY_CHANGED));
        }
        match &self.scope {
            Some(root) => request_project::in_project(root, || self.verify_in_scope()),
            None => self.verify_in_scope(),
        }
    }

    fn verify_in_scope(&self) -> Result<(), GatewayRefusal> {
        let current = usable_policy(&Config::load_arc())?;
        if !self.epoch.matches(
            &current,
            self.epoch_project.as_ref().map(<[u8; 32]>::as_slice),
        ) {
            return Err(GatewayRefusal::withheld(POLICY_CHANGED));
        }
        let project_current = match &self.project {
            Some(authority) => authority.verify().is_ok(),
            None => !project_policy::is_active(),
        };
        if !project_current {
            return Err(GatewayRefusal::withheld(POLICY_CHANGED));
        }
        self.original
            .as_deref()
            .map_or(Ok(()), Self::verify_in_scope)
    }

    /// The full identity: global policy, and project root, policy and role
    /// (or the absence of a project policy).
    fn same_identity(&self, other: &Self) -> bool {
        let project = |authority: &Self| {
            authority.project.as_ref().map(|project| {
                (
                    project.fingerprint_bytes(),
                    project.role_fingerprint_bytes(),
                )
            })
        };
        self.epoch == other.epoch
            && self.epoch_project == other.epoch_project
            && project(self) == project(other)
    }
}

/// Test-only barriers at the snapshot boundaries of an admission, so a test
/// can change the policy exactly there instead of racing for it.
#[cfg(test)]
pub(crate) mod failpoint {
    use std::cell::RefCell;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Point {
        /// The authority is captured; the shared admission has not run.
        AuthorityCaptured,
        /// The shared admission returned; its epoch is not yet compared.
        Decided,
        /// The caller authority was found current; the final admission has
        /// not started.
        CallerVerified,
    }

    type Hook = Box<dyn FnMut(Point)>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// Disarms the hook of this thread when dropped.
    pub(crate) struct Armed;

    pub(crate) fn arm(hook: impl FnMut(Point) + 'static) -> Armed {
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
        Armed
    }

    impl Drop for Armed {
        fn drop(&mut self) {
            HOOK.with(|slot| *slot.borrow_mut() = None);
        }
    }

    pub(super) fn reach(point: Point) {
        let armed = HOOK.with(|slot| slot.borrow_mut().take());
        if let Some(mut hook) = armed {
            hook(point);
            HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
        }
    }
}

/// A gateway decision together with the authority it was made under.
#[derive(Debug)]
pub(crate) struct Admission {
    pub(crate) outcome: EgressOutcome,
    authority: Authority,
    /// The outcome's own policy epoch includes the project fingerprint.
    project_epoch_bound: bool,
    /// The context this decision was taken in, as provider-sealed items of
    /// the response will be bound to it.
    sealed: SealedContext,
    /// The sources the request was derived from, as its caller-stage
    /// admission found them; `None` where no lineage is derived (no
    /// protection that demands source authority).
    lineage: Option<Lineage>,
}

impl Admission {
    pub(crate) fn is_refused(&self) -> bool {
        matches!(self.outcome.body, EgressBody::Refused(_))
    }

    /// The admitted document when the gateway changed the payload. A refusal
    /// stays in place.
    pub(crate) fn take_rewritten(&mut self) -> Option<Value> {
        match std::mem::replace(&mut self.outcome.body, EgressBody::Unchanged) {
            EgressBody::Rewritten(doc) => Some(doc),
            other => {
                self.outcome.body = other;
                None
            }
        }
    }

    /// This admission refused after the decision: the deny decision with
    /// `codes` is recorded in the outcome's capture, so the refusal receipt
    /// carries it.
    fn refused(mut self, codes: &[&str]) -> Self {
        self.outcome = self.outcome.refuse_with(codes);
        self
    }

    /// State in this admission's receipt which blocks of the request stayed
    /// whole under protection, and why: one note decision per block, each
    /// with its `recovery.*` code, so the receipt's counts per code are those
    /// of `kept`. Codes and counts only — a note names no block. Nothing is
    /// decided by it, the receipt's source counts do not move, and an
    /// admission without a capture records nothing.
    #[must_use]
    pub(crate) fn note_kept_whole(mut self, kept: &super::compress::KeptWhole) -> Self {
        let codes: Vec<&str> = kept
            .counts()
            .into_iter()
            .flat_map(|(code, blocks)| std::iter::repeat_n(code, blocks))
            .collect();
        if !codes.is_empty() {
            self.outcome = self.outcome.note_with(&codes);
        }
        self
    }

    /// The typed refusal of a refused admission, from the deny decisions its
    /// receipt will carry.
    fn refusal(&self) -> GatewayRefusal {
        let codes = self.outcome.refusal_codes();
        if !codes.is_empty() {
            let mut listed: Vec<&str> = Vec::new();
            for code in &codes {
                if !listed.contains(&code.as_str()) {
                    listed.push(code.as_str());
                }
            }
            return GatewayRefusal::new(WITHHELD_STATUS, listed)
                .with_blocks(self.outcome.refused_blocks());
        }
        match &self.outcome.body {
            EgressBody::Refused(reason) => {
                GatewayRefusal::from_reason(reason).with_blocks(self.outcome.refused_blocks())
            }
            EgressBody::Unchanged | EgressBody::Rewritten(_) => {
                GatewayRefusal::withheld(FALLBACK_CODE)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn into_authority(self) -> Authority {
        self.authority
    }

    /// The check every bind and dispatch runs, for tests that hold an
    /// admission across a policy change.
    #[cfg(test)]
    pub(crate) fn still_current(&self) -> Result<(), GatewayRefusal> {
        self.verify_current()
    }

    /// The authority must be current, and the outcome's own policy epoch
    /// must agree where the shared module can tell: it binds a project
    /// fingerprint only into document admissions.
    fn verify_current(&self) -> Result<(), GatewayRefusal> {
        self.authority.verify_current()?;
        match &self.authority.project {
            Some(project)
                if self.project_epoch_bound
                    && !self
                        .outcome
                        .policy_epoch_current_for_project(&project.fingerprint_bytes()) =>
            {
                Err(GatewayRefusal::withheld(POLICY_CHANGED))
            }
            _ => Ok(()),
        }
    }
}

/// The kind of a transport failure, for logs. A `reqwest::Error` prints the
/// request URL with its query string, which can carry a caller's credential
/// or content; no log line of the proxy prints the error itself.
pub(crate) fn transport_failure(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connection failure"
    } else if error.is_body() {
        "body transfer failure"
    } else if error.is_decode() {
        "response decode failure"
    } else if error.is_request() {
        "request failure"
    } else {
        "transport failure"
    }
}

/// What a route is about to hand to a destination.
#[derive(Clone, Copy)]
pub(crate) enum Payload<'a> {
    /// A JSON document the gateway walks string by string.
    Document(&'a Value),
    /// Bytes no detector can read; only their count is known.
    Opaque(usize),
}

/// Admit one payload for one destination under the current policy.
///
/// Without an active project/organisation policy this is exactly the global
/// gateway admission. With one, the body is additionally held to that policy
/// (the rule every other carrier follows through `policy::runtime`): strings
/// pass its block, classification and redaction rules, and a body the policy
/// cannot inspect is withheld. The proxy has no per-request project binding,
/// so the policy is the one resolved for the proxy process itself — its
/// working directory's pack under the organisation floor. A policy that
/// cannot be verified and a global configuration that cannot be loaded
/// refuse; neither falls back to "no policy".
///
/// The returned admission may itself be a refusal (`EgressBody::Refused`).
pub(crate) fn admit(
    payload: Payload<'_>,
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
) -> Result<Admission, GatewayRefusal> {
    admit_as(payload, target, binding, None)
}

/// [`admit`] for a request whose caller the proxy can tell apart:
/// `principal` is the content-free identity the route established for it
/// (see `forward::admission::sealed_principal`). It is one dimension of the
/// context provider-sealed items are bound to; `None` means the route has no
/// such identity, which is then the same for every caller of that route.
pub(crate) fn admit_as(
    payload: Payload<'_>,
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
    principal: Option<&str>,
) -> Result<Admission, GatewayRefusal> {
    admit_at(payload, target, binding, principal, Stage::Caller)
}

/// Which body an admission decides on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// What the host supplied. Its parts are what the Engine delivered, so
    /// the source lineage of the request is derived here.
    Caller,
    /// What the proxy made of it. No lineage is derived from a transformed
    /// body; the caller stage's lineage stands.
    Final,
}

/// Resolve the project the proxy acts for (operator configuration only, see
/// [`request_project`]) and decide inside its request scope. A configured
/// root that cannot be used, or that is not the launch-pinned one, refuses:
/// nothing is admitted for a project the proxy cannot verify.
fn admit_at(
    payload: Payload<'_>,
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
    principal: Option<&str>,
    stage: Stage,
) -> Result<Admission, GatewayRefusal> {
    let project =
        request_project::resolve(&Config::load_arc()).map_err(GatewayRefusal::unavailable)?;
    match project.scope() {
        Some(root) => request_project::in_project(root, || {
            admit_in(payload, target, binding, principal, stage, &project)
        }),
        None => admit_in(payload, target, binding, principal, stage, &project),
    }
}

fn admit_in(
    payload: Payload<'_>,
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
    principal: Option<&str>,
    stage: Stage,
    proxy_project: &ProxyProject,
) -> Result<Admission, GatewayRefusal> {
    let derived: Option<LineageSlot> =
        (stage == Stage::Caller).then(|| std::rc::Rc::new(std::cell::RefCell::new(None)));
    let snapshot = Config::load_arc();
    // The admission policy of exactly this snapshot (settings, secret
    // patterns, effective switch, bound semantic runtime, load state): the
    // decision below is accepted only if it was taken under this policy.
    let captured = usable_policy(&snapshot)?;
    #[cfg(test)]
    failpoint::reach(failpoint::Point::AuthorityCaptured);
    let principal = principal.map(str::to_owned);
    // The decision, bound to the authority it was taken under and held to it
    // once more before it is returned. Run inside the source view for a
    // governed request, so that this closing check is made while the view the
    // decision was taken in is still held.
    let conclude = |outcome: EgressOutcome,
                    project: Option<project_policy::PublicationAuthority>,
                    sealed: SealedContext|
     -> Result<Admission, GatewayRefusal> {
        conclude_admission(
            outcome,
            project,
            sealed,
            derived.as_ref(),
            stage,
            proxy_project,
            &captured,
        )
    };
    // ONE verification decides whether this request is governed — by a
    // project policy, or by an observed organization decision that leaves no
    // policy (a signed unpin) — and is the view it then runs in. Nothing is
    // probed first and resolved again after: what is branched on here is what
    // the view holds, observation included.
    let governed = project_policy::with_governed_source_view(|| {
        let authority = project_policy::PublicationAuthority::capture().ok()?;
        let fingerprint = authority.fingerprint_bytes();
        // Read from the held view: whether this verification found a policy.
        // An observed organization decision alone is not one — it withholds
        // no opaque body and makes no pinned root a source project.
        let policy_verified = project_policy::is_active();
        let sealed = SealedContext::new(
            target,
            binding,
            Some((fingerprint, authority.role_fingerprint_bytes())),
            principal.clone(),
        );
        // The root the source view above verified the policy for and pinned
        // as the request project. Sources of sealed items are looked up and
        // re-admitted for it only when it is the proxy's own source project —
        // a policy found from the working directory is applied to the
        // content, but is no project to admit sources for.
        let pinned = project_policy::REQUEST_PROJECT
            .try_with(|slot| slot.borrow().clone())
            .ok()
            .flatten();
        // The configured root, or the launch-pinned root when exactly this
        // verification found a policy for it. Never the working directory.
        let root = proxy_project
            .source_project_under(policy_verified)
            .filter(|root| Some(root) == pinned.as_ref());
        let outcome = match payload {
            // The walk asks the register of this context about every
            // provider-sealed item the request returns.
            Payload::Document(doc) => egress::with_sealed_resolver(
                sealed_origin::resolver(sealed.clone(), derived.clone(), root),
                || {
                    egress::admit_request_for_project_with_binding(
                        doc,
                        target,
                        &fingerprint,
                        binding,
                    )
                },
            ),
            Payload::Opaque(bytes) => {
                // Bound to the project like a document; its content rules
                // cannot be evaluated on bytes nothing can read, so a
                // non-empty opaque body is withheld under a project policy.
                let outcome = egress::admit_opaque_for_project_with_binding(
                    bytes,
                    target,
                    &fingerprint,
                    binding,
                );
                if bytes > 0 && policy_verified && !matches!(outcome.body, EgressBody::Refused(_)) {
                    outcome.refuse_with(&["policy.content_withheld"])
                } else {
                    outcome
                }
            }
        };
        Some(conclude(outcome, Some(authority), sealed))
    })
    .map_err(|_| GatewayRefusal::unavailable("policy.unverifiable"))?;
    if let Some(decided) = governed {
        // Entered, and the view verified to its end.
        decided.ok_or_else(|| GatewayRefusal::unavailable("policy.unverifiable"))?
    } else {
        // That verification found no policy and observed no organization
        // decision: no view, no project binding.
        // Sources are admitted for the proxy's source project if it has
        // one without a policy (a configured root needs no pack); with
        // none, a lineage that names sources is unknown — it is not
        // checked against the working directory instead.
        let sources = proxy_project.source_project_under(false);
        let sealed = SealedContext::new(target, binding, None, principal);
        let outcome = match payload {
            Payload::Document(doc) => egress::with_sealed_resolver(
                sealed_origin::resolver(sealed.clone(), derived.clone(), sources),
                || egress::admit_request_with_binding(doc, target, binding),
            ),
            Payload::Opaque(bytes) => egress::admit_opaque_with_binding(bytes, target, binding),
        };
        conclude(outcome, None, sealed)
    }
}

/// Bind `outcome` to the authority it was decided under and hold it to that
/// authority once more. For a governed request this runs inside the source
/// view the decision was taken in.
fn conclude_admission(
    outcome: EgressOutcome,
    project: Option<project_policy::PublicationAuthority>,
    sealed: SealedContext,
    derived: Option<&LineageSlot>,
    stage: Stage,
    proxy_project: &ProxyProject,
    captured: &AdmissionPolicy,
) -> Result<Admission, GatewayRefusal> {
    // What the walk derived. Where protection demands source authority and
    // nothing was derived (a body nothing can read), the lineage is unknown,
    // not absent: what its response produces is recorded as such.
    let mut lineage = None;
    if let Some(slot) = derived {
        lineage = slot.take();
    }
    if lineage.is_none()
        && stage == Stage::Caller
        && crate::core::context_admission::stored::source_authority_required()
    {
        lineage = Some(Lineage::Unknown);
    }
    // Document and opaque decisions alike carry the project fingerprint.
    let project_epoch_bound = project.is_some();
    let epoch_project = project_epoch_bound
        .then(|| {
            project
                .as_ref()
                .map(project_policy::PublicationAuthority::fingerprint_bytes)
        })
        .flatten();
    // An owed receipt does not go without the caller it names: an unusable
    // receipt key refuses (fail closed); otherwise the reference is absent.
    let outcome = match sealed.principal_ref() {
        Ok(reference) => outcome.for_principal(reference),
        Err(code) if outcome.receipt_required() => return Err(GatewayRefusal::unavailable(code)),
        Err(_) => outcome,
    };
    let admission = Admission {
        project_epoch_bound,
        authority: Authority {
            epoch: outcome.policy_epoch(),
            epoch_project,
            project,
            original: None,
            scope: proxy_project.scope().map(std::path::Path::to_path_buf),
        },
        outcome,
        sealed,
        lineage,
    };
    #[cfg(test)]
    failpoint::reach(failpoint::Point::Decided);
    // The authority bound to this decision must be the one it was actually
    // taken under, for every payload shape. The shared admission resolved
    // its own configuration snapshot and reports the policy epoch it decided
    // under: unless that is the captured policy, the decision is discarded
    // (fail closed) — also when the policy changed and changed back in
    // between, because the decision then carries the other policy's epoch.
    // The comparison is by policy identity, not by which configuration object
    // was loaded: a reload of unchanged or policy-irrelevant settings is not
    // a policy change. The project side is pinned by the source view above,
    // which fails when its policy moves; a project-bound document decision
    // carries the project fingerprint in its epoch.
    if !admission.authority.epoch.matches(
        captured,
        admission
            .authority
            .epoch_project
            .as_ref()
            .map(<[u8; 32]>::as_slice),
    ) {
        return Err(GatewayRefusal::withheld(POLICY_CHANGED));
    }
    admission.authority.verify_current()?;
    Ok(admission)
}

/// The decision on what the caller supplied (stage 1).
#[derive(Debug)]
pub(crate) struct CallerAdmission {
    admission: Admission,
    destination: DestinationV1,
    /// The caller's identity as presented, before any gateway-held
    /// credential replaces its headers; the final admission uses the same.
    principal: Option<String>,
}

impl CallerAdmission {
    pub(crate) fn is_refused(&self) -> bool {
        self.admission.is_refused()
    }

    /// The admitted document that replaces the caller's body from here on.
    pub(crate) fn take_rewritten(&mut self) -> Option<Value> {
        self.admission.take_rewritten()
    }

    pub(crate) fn into_admission(self) -> Admission {
        self.admission
    }
}

/// Stage 1 (G01): decide on the caller's content before anything else reads
/// it. `target`/`binding` name the destination the request is expected to
/// leave for; [`admit_final_with`] holds the request to the destination it
/// really leaves for.
pub(crate) fn admit_caller(
    payload: Payload<'_>,
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
    principal: Option<String>,
) -> Result<CallerAdmission, GatewayRefusal> {
    let admission = admit_as(payload, target, binding, principal.as_deref())?;
    Ok(CallerAdmission {
        destination: egress::destination_with_binding(target, binding),
        admission,
        principal,
    })
}

/// Stage 2 (G05): admit the body that leaves, after compression, pruning,
/// in-band recovery, prefix replay, routing and translation.
///
/// - The caller admission must still be current: when the policy changed
///   since, the request is refused (`policy.changed`). Transformed text is
///   never re-judged under a policy that did not see the original.
/// - Nothing a transformation produced or brought back may bypass the
///   decision: a body that is not byte-for-byte the admitted one is admitted
///   again, and so is any body headed for another destination than expected.
/// - The classification travels with the request (G02): the decision is
///   taken on the join of what the caller's content and the final body were
///   classified as, so restricted content never reaches a destination that
///   is not provably local, even when a transformation removed its marking.
/// - Both decisions are recorded: when the final pass carries the receipt,
///   the caller admission is kept as its provenance.
///
/// `untouched`: the body is byte-for-byte the admitted caller body. Returns
/// the admission to bind (possibly a refusal) and the replacement document
/// when the final pass rewrote one — or restored one, see below.
///
/// The passes of a request may have handed recovery stubs to the model. A
/// stub is text the proxy wrote, and this admission judges it like any other
/// text: an operator's secret pattern, a policy rule or a semantic detector
/// may mask or withhold the id in it — nothing exempts a stub. So the stubs
/// are compared after the decision: `restore` gets the document this
/// admission would send (`None`: it refused the request) and returns, if a
/// stub of the request is not in it byte for byte, the body with those
/// blocks back to their originals. That body is then admitted instead, once,
/// and it is the one that leaves — the original of a block is admitted as it
/// would have been without compression, and no block leaves shortened with a
/// handle its model cannot use. The first decision is dropped unrecorded:
/// nothing it decided on leaves.
pub(crate) fn admit_final_with(
    payload: Payload<'_>,
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
    caller: CallerAdmission,
    untouched: bool,
    restore: impl FnOnce(Option<&Value>) -> Option<Value>,
) -> Result<(Admission, Option<Value>), GatewayRefusal> {
    caller.admission.verify_current()?;
    #[cfg(test)]
    failpoint::reach(failpoint::Point::CallerVerified);
    let destination = egress::destination_with_binding(target, binding);
    if untouched && destination == caller.destination {
        return Ok((caller.admission, None));
    }
    let mut check = admit_at(
        payload,
        target,
        binding,
        caller.principal.as_deref(),
        Stage::Final,
    )?;
    let restored = match &check.outcome.body {
        EgressBody::Unchanged => None,
        EgressBody::Rewritten(admitted) => restore(Some(admitted)),
        EgressBody::Refused(_) => restore(None),
    };
    if let Some(originals) = &restored {
        check = admit_at(
            Payload::Document(originals),
            target,
            binding,
            caller.principal.as_deref(),
            Stage::Final,
        )?;
    }
    // The final decision may only stand in for the transformed content when
    // it was taken under the very authority that admitted the original:
    // same global policy, same project, policy and role. Anything else would
    // re-authorise old, already transformed content under a new policy.
    if !check.authority.same_identity(&caller.admission.authority) {
        return Err(GatewayRefusal::withheld(POLICY_CHANGED));
    }
    let Admission {
        outcome: caller_outcome,
        authority: caller_authority,
        lineage,
        ..
    } = caller.admission;
    let Admission {
        outcome: final_outcome,
        mut authority,
        project_epoch_bound,
        sealed,
        ..
    } = check;
    // Both authorities stay bound: the caller's next to the final one,
    // re-proved at seal and before every dispatch.
    authority.original = Some(Box::new(caller_authority));
    // ONE outcome, hence one receipt, for the bytes that leave: the caller
    // decision's provenance (what the original content was classified and
    // masked as), the final decision, every redaction of both and the higher
    // of the two classifications — whether the final pass found nothing
    // (once-transformed body) or masked again (twice-transformed body). A
    // refusal in either decision refuses the joined outcome.
    let mut outcome = caller_outcome.join(final_outcome);
    // The decision is taken on the joined classification (G02): restricted
    // content never reaches a destination that is not provably local, even
    // when a transformation removed its marking.
    if !matches!(outcome.body, EgressBody::Refused(_))
        && destination.locality != DestinationLocalityV1::Local
        && outcome
            .classification
            .is_some_and(|classification| classification >= ClassificationV1::Restricted)
    {
        outcome = outcome.refuse_with(&["destination.remote_restricted"]);
    }
    let mut admission = Admission {
        outcome,
        authority,
        project_epoch_bound,
        sealed,
        // The lineage of what the host supplied: the transformed body is
        // derived from the same sources.
        lineage,
    };
    let rewritten = admission.take_rewritten();
    if rewritten.is_some() {
        tracing::warn!(
            "lean-ctx proxy: a request transformation produced content the gateway masked \
             again before forwarding"
        );
    }
    // What leaves is what the standing decision made of the body it was
    // taken on: its own rewrite, or else the restored body itself.
    Ok((admission, rewritten.or(restored)))
}

/// [`admit_final_with`] for a request that issued no recovery stubs.
#[cfg(test)]
pub(crate) fn admit_final(
    payload: Payload<'_>,
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
    caller: CallerAdmission,
    untouched: bool,
) -> Result<(Admission, Option<Value>), GatewayRefusal> {
    admit_final_with(payload, target, binding, caller, untouched, |_| None)
}

/// Persist the withheld receipt of a refused admission and return its typed
/// reason. Failing to record a refusal never turns it into a delivery.
pub(crate) async fn record_refusal(
    admission: Admission,
    original_bytes: usize,
    agent_id: Option<String>,
) -> GatewayRefusal {
    let refusal = admission.refusal();
    tracing::warn!(
        "lean-ctx proxy: {} (see `lean-ctx inspect --proxy`)",
        refusal.message()
    );
    discard(admission, original_bytes, agent_id).await;
    refusal
}

/// Record that nothing of this admission was delivered.
async fn discard(admission: Admission, original_bytes: usize, agent_id: Option<String>) {
    let _ = egress::finish_admission_off_runtime(admission.outcome, None, original_bytes, agent_id)
        .await;
}

/// Bind `bytes` — a delivery that is not an upstream HTTP request (a
/// WebSocket frame, an API response) — to its admission receipt, under the
/// authority that is current at this moment.
pub(crate) async fn bind_delivery(
    admission: Admission,
    bytes: &[u8],
    original_bytes: usize,
) -> Result<BoundDelivery, GatewayRefusal> {
    let receipt_required = admission.outcome.receipt_required();
    let (admission_digest, producer) =
        seal(admission, bytes.to_vec(), original_bytes, None).await?;
    Ok(BoundDelivery {
        admission_digest,
        authority: producer.authority,
        receipt_required,
    })
}

/// Code of a frame that left while its owed transport observation could not
/// be saved: the channel ends, nothing further is sent on it.
const OBSERVATION_UNAVAILABLE: &str = "receipt.observation_unavailable";

/// A delivery bound by [`bind_delivery`]: its receipt and the authority it
/// was admitted under, for the transport that hands it over later.
#[derive(Debug)]
pub(crate) struct BoundDelivery {
    admission_digest: Option<Sha256Digest>,
    authority: Arc<Authority>,
    /// Frozen with admission: the receipt — and so the observation of its
    /// hand-over — is owed.
    receipt_required: bool,
}

impl BoundDelivery {
    /// The proof [`AdmittedBody::send`] runs on its finished request, for a
    /// transport without a request builder: immediately before the bytes are
    /// handed over, the authority they were admitted under must still be the
    /// current one (AP10-R1). Sealing and the send are separate awaits.
    pub(crate) fn verify_current(&self) -> Result<(), GatewayRefusal> {
        self.authority.verify_current()
    }

    /// Record the hand-over of a WebSocket frame (`sent`: the socket took
    /// it). Where the receipt is owed, an observation that could not be saved
    /// refuses: the frame may have left, but the channel does not go on
    /// without evidence (AP10-C7). Without that duty the gap only stays
    /// visible as a missing observation.
    pub(crate) async fn observe_frame(&self, sent: bool) -> Result<(), GatewayRefusal> {
        let recorded =
            super::egress_observation::record_websocket_frame(self.admission_digest.as_ref(), sent)
                .await;
        if recorded || !self.receipt_required {
            Ok(())
        } else {
            Err(GatewayRefusal::unavailable(OBSERVATION_UNAVAILABLE))
        }
    }
}

/// Persist the receipts for `bytes` and hand back the authority they were
/// admitted under. Refused, outdated and unprovable admissions end here.
async fn seal(
    admission: Admission,
    bytes: Vec<u8>,
    original_bytes: usize,
    agent_id: Option<String>,
) -> Result<(Option<Sha256Digest>, SealedProducer), GatewayRefusal> {
    if admission.is_refused() {
        return Err(record_refusal(admission, original_bytes, agent_id).await);
    }
    if let Err(refusal) = admission.verify_current() {
        tracing::warn!("lean-ctx proxy: {}", refusal.message());
        // The withheld receipt names why nothing left (`policy.changed`, …).
        let code = refusal.codes.first().map_or(FALLBACK_CODE, String::as_str);
        discard(admission.refused(&[code]), original_bytes, agent_id).await;
        return Err(refusal);
    }
    let Admission {
        outcome,
        authority,
        sealed,
        lineage,
        ..
    } = admission;
    // One receipt: `outcome` already joins the caller decision and the final
    // one when the request was transformed.
    let digest =
        egress::finish_admission_off_runtime(outcome, Some(bytes), original_bytes, agent_id)
            .await
            .map_err(|_| GatewayRefusal::unavailable(RECEIPT_UNAVAILABLE))?
            .and_then(|evidence| evidence.admission_digest);
    Ok((
        digest,
        SealedProducer {
            context: sealed,
            authority: Arc::new(authority),
            lineage: lineage.map(Arc::new),
        },
    ))
}

/// Admit a destination for a channel that carries its content later, frame by
/// frame (a WebSocket tunnel): placement rules apply before any connection.
/// The channel's receipt binds the handshake: `url` and `head` (AP10-C2).
pub(crate) async fn admit_channel(
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
    principal: Option<&str>,
    url: &str,
    head: &RequestHead,
) -> Result<(), GatewayRefusal> {
    let mut admission = admit_as(Payload::Opaque(0), target, binding, principal)?;
    let url =
        reqwest::Url::parse(url).map_err(|_| GatewayRefusal::withheld("destination.unbound"))?;
    match bound_head(&admission, head, &url) {
        Ok(reference) => admission.outcome = admission.outcome.with_request_head(reference),
        Err(code) => {
            discard(admission, 0, None).await;
            return Err(GatewayRefusal::unavailable(code));
        }
    }
    bind_delivery(admission, &[], 0).await.map(|_| ())
}

/// The keyed reference of `head` sent to `url`, for the receipt of
/// `admission`. Where that receipt is owed and the receipt key is unusable,
/// the code to refuse with (fail closed); otherwise an absent reference.
fn bound_head(
    admission: &Admission,
    head: &RequestHead,
    url: &reqwest::Url,
) -> Result<Option<String>, &'static str> {
    match head.reference(url) {
        Ok(reference) => Ok(Some(reference)),
        Err(code) if admission.outcome.receipt_required() => Err(code),
        Err(_) => Ok(None),
    }
}

/// Result of admitting a text payload as one object.
pub(crate) struct TextAdmission {
    pub(crate) admission: Admission,
    /// The masked text to deliver instead; `None` delivers the original bytes.
    pub(crate) rewritten: Option<String>,
}

/// Admit UTF-8 content whose shape is not a model request (a passthrough
/// body, a WebSocket text frame) as one object: every byte is inspected,
/// no key is skipped as structure. Content the gateway withholds refuses the
/// whole payload — a marker is never sent in its place.
pub(crate) fn admit_text(
    text: &str,
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
    principal: Option<&str>,
) -> Result<TextAdmission, GatewayRefusal> {
    let document = Value::String(text.to_owned());
    let mut admission = admit_as(Payload::Document(&document), target, binding, principal)?;
    let rewritten = match admission.take_rewritten() {
        Some(Value::String(masked)) if masked.starts_with(WITHHELD_MARKER) => {
            let listed = masked
                .split_once('—')
                .map_or("", |(_, codes)| codes)
                .trim_end_matches(']');
            // The notice may name its block: `<codes> · lean-ctx gateway why <id>`.
            let listed = listed.split(" · ").next().unwrap_or_default();
            let codes: Vec<&str> = listed
                .split(',')
                .map(str::trim)
                .filter(|code| !code.is_empty())
                .collect();
            admission = if codes.is_empty() {
                admission.refused(&[FALLBACK_CODE])
            } else {
                admission.refused(&codes)
            };
            None
        }
        Some(Value::String(masked)) => Some(masked),
        Some(_) => {
            admission = admission.refused(&["egress.rewrite_unrepresentable"]);
            None
        }
        None => None,
    };
    Ok(TextAdmission {
        admission,
        rewritten,
    })
}

/// Code of a request whose path or query the gateway did not admit.
const TARGET_WITHHELD: &str = "egress.target_withheld";

/// Admit what a request carries besides its body — the caller-chosen path
/// and query and the values of the relayed caller headers — before anything
/// is sent (AP10-C2; the text is [`super::egress_target::target_text`]). It
/// leaves like the body does, but cannot be masked without changing the
/// request: any finding refuses, and the refusal is recorded like a refused
/// body. `headers` is the set that will be relayed; credential headers meant
/// for the destination are left out of the text there.
pub(crate) async fn admit_request_target<'h>(
    path_and_query: &str,
    headers: impl IntoIterator<Item = (&'h HeaderName, &'h HeaderValue)>,
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
    principal: Option<&str>,
) -> Result<(), GatewayRefusal> {
    let text = super::egress_target::target_text(
        target.provider,
        target.upstream_base,
        path_and_query,
        headers,
    );
    if text.is_empty() {
        return Ok(());
    }
    let TextAdmission {
        admission,
        rewritten,
    } = admit_text(&text, target, binding, principal)?;
    if admission.is_refused() {
        return Err(record_refusal(admission, text.len(), None).await);
    }
    if rewritten.is_some() {
        let admission = admission.refused(&[TARGET_WITHHELD]);
        return Err(record_refusal(admission, text.len(), None).await);
    }
    // Admitted unchanged: the body's receipt covers the request; this
    // decision is not stored on its own.
    Ok(())
}

fn decode_body<'a>(parts: &Parts, body: &'a [u8], limit: usize) -> Option<Cow<'a, [u8]>> {
    match request_body_encoding(parts) {
        RequestBodyEncoding::Identity => Some(Cow::Borrowed(body)),
        RequestBodyEncoding::Gzip => decode_gzip_bounded(body, limit).ok().map(Cow::Owned),
        RequestBodyEncoding::Zstd => decode_zstd_bounded(body, limit).ok().map(Cow::Owned),
        RequestBodyEncoding::Passthrough => None,
    }
}

fn encode_body(parts: &Parts, logical: Vec<u8>) -> Option<Vec<u8>> {
    match request_body_encoding(parts) {
        RequestBodyEncoding::Identity => Some(logical),
        RequestBodyEncoding::Gzip => encode_gzip(&logical).ok(),
        RequestBodyEncoding::Zstd => encode_zstd(&logical).ok(),
        RequestBodyEncoding::Passthrough => None,
    }
}

fn is_json(text: &str) -> bool {
    serde_json::from_str::<serde::de::IgnoredAny>(text).is_ok()
}

/// Admit the body of a credential-preserving passthrough route. Readable
/// text (after its declared content encoding) is admitted as one object; an
/// unknown encoding, undecodable or binary body is an opaque payload —
/// delivered marked as such in developer mode, refused in governed and
/// sovereign mode. A masked JSON body that is no longer JSON is refused.
///
/// Returns the admission to bind (possibly a refusal) and the bytes to send.
pub(crate) fn admit_passthrough(
    parts: &Parts,
    body: &[u8],
    limit: usize,
    target: &EgressTarget<'_>,
    binding: TrustedDestinationBinding<'_>,
) -> Result<(Admission, Vec<u8>), GatewayRefusal> {
    let principal = super::forward::sealed_principal(parts);
    let principal = principal.as_deref();
    let decoded = decode_body(parts, body, limit);
    let text = decoded
        .as_deref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .filter(|text| !text.is_empty() && !text.contains('\0'));
    let Some(text) = text else {
        let admission = admit_as(Payload::Opaque(body.len()), target, binding, principal)?;
        return Ok((admission, body.to_vec()));
    };
    let TextAdmission {
        admission,
        rewritten,
    } = admit_text(text, target, binding, principal)?;
    let Some(masked) = rewritten else {
        return Ok((admission, body.to_vec()));
    };
    let encoded = if is_json(text) && !is_json(&masked) {
        None
    } else {
        encode_body(parts, masked.into_bytes())
    };
    if let Some(bytes) = encoded {
        return Ok((admission, bytes));
    }
    Ok((
        admission.refused(&["egress.rewrite_unrepresentable"]),
        Vec::new(),
    ))
}

/// Bytes an admission produced, bound to their receipt, to one URL under the
/// admitted upstream and to the authority they were admitted under. Only
/// [`AdmittedBody::bind`] creates one.
#[derive(Debug)]
pub(crate) struct AdmittedBody {
    bytes: Vec<u8>,
    url: reqwest::Url,
    /// The method and header set the bytes are sent with, bound with them.
    head: RequestHead,
    admission_digest: Option<Sha256Digest>,
    /// The context and authority the bytes were admitted under; shared with
    /// the register of sealed items their response may produce.
    producer: SealedProducer,
}

/// What a request carries besides its URL and body: the method and the
/// header set relayed with it. Bound by [`AdmittedBody::bind`] — its keyed
/// reference is in the receipt's sidecar — and the only head
/// [`AdmittedBody::send`] lets leave (AP10-C2).
#[derive(Debug, Clone)]
pub(crate) struct RequestHead {
    method: reqwest::Method,
    headers: reqwest::header::HeaderMap,
}

impl RequestHead {
    pub(crate) fn new(
        method: reqwest::Method,
        headers: impl IntoIterator<Item = (HeaderName, HeaderValue)>,
    ) -> Self {
        let mut map = reqwest::header::HeaderMap::new();
        for (name, value) in headers {
            map.append(name, value);
        }
        Self {
            method,
            headers: map,
        }
    }

    fn reference(&self, url: &reqwest::Url) -> Result<String, &'static str> {
        receipt_store::keyed::request_head_ref(
            self.method.as_str(),
            url.as_str(),
            self.headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_bytes())),
        )
    }
}

/// Code of a request whose finished head is not the bound one.
const HEAD_UNBOUND: &str = "egress.head_unbound";

/// One dispatch attempt and whether its observation was saved. An attempt
/// whose evidence could not be saved must not be retried.
pub(crate) struct SendAttempt {
    pub(crate) response: Result<reqwest::Response, reqwest::Error>,
    pub(crate) recorded: bool,
}

/// `url` when it stays on the origin of the admitted upstream. Caller-chosen
/// path or query text can therefore never move the body to another host.
fn bound_url(upstream_base: &str, url: &str) -> Option<reqwest::Url> {
    let base = reqwest::Url::parse(upstream_base).ok()?;
    let url = reqwest::Url::parse(url).ok()?;
    (base.scheme() == url.scheme()
        && base.host_str().is_some()
        && base.host_str() == url.host_str()
        && base.port_or_known_default() == url.port_or_known_default()
        && base.username() == url.username()
        && base.password() == url.password())
    .then_some(url)
}

impl AdmittedBody {
    /// Bind the exact `bytes` that will be sent to `url` with `head` to the
    /// receipt of `admission`. A refused admission is recorded and returned
    /// as its refusal; an authority that is no longer current, a URL outside
    /// `upstream_base`'s origin or a missing required receipt refuses as
    /// well. `bytes` must be what `admission` admitted, in its final wire
    /// encoding.
    pub(crate) async fn bind(
        mut admission: Admission,
        bytes: Vec<u8>,
        upstream_base: &str,
        url: &str,
        head: RequestHead,
        original_bytes: usize,
        agent_id: Option<String>,
    ) -> Result<Self, GatewayRefusal> {
        if admission.is_refused() {
            return Err(record_refusal(admission, original_bytes, agent_id).await);
        }
        let Some(url) = bound_url(upstream_base, url) else {
            discard(admission, original_bytes, agent_id).await;
            return Err(GatewayRefusal::withheld("destination.unbound"));
        };
        match bound_head(&admission, &head, &url) {
            Ok(reference) => {
                admission.outcome = admission.outcome.with_request_head(reference);
            }
            Err(code) => {
                // Like a receipt that cannot be stored: nothing is sent.
                discard(admission, original_bytes, agent_id).await;
                return Err(GatewayRefusal::unavailable(code));
            }
        }
        let (admission_digest, producer) =
            seal(admission, bytes.clone(), original_bytes, agent_id).await?;
        Ok(Self {
            bytes,
            url,
            head,
            admission_digest,
            producer,
        })
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// A recorder for the response to these bytes: sealed items the
    /// destination produces are bound to this request's context and authority.
    pub(crate) fn sealed_recorder(&self) -> sealed_origin::Recorder {
        sealed_origin::Recorder::new(self.producer.clone())
    }

    #[cfg(test)]
    pub(crate) fn admission_digest(&self) -> Option<&Sha256Digest> {
        self.admission_digest.as_ref()
    }

    /// Send the admitted bytes to the bound URL with the bound head and
    /// record what the attempt observed. `decorate` sets transport options
    /// such as timeouts; the body, the URL, the method and the header set are
    /// always the bound ones — a finished request whose head differs from
    /// the bound one (a header `decorate` added, changed or removed) is
    /// refused, not sent (AP10-C2). A retry is another call with the next
    /// `attempt` — same bytes, same destination, same head.
    ///
    /// Every call proves that the authority the bytes were admitted under is
    /// still the current one — before the request is built, so that nothing
    /// is decorated for a request already refused, and again on the finished
    /// request, immediately before it is handed to the client: `decorate` is
    /// the caller's code and takes whatever time it takes. When the proof
    /// fails, nothing is sent and the typed refusal is returned: bytes
    /// admitted under an old policy do not leave under a new one, not even as
    /// the retry of a request whose first attempt already went out. This
    /// admits the dispatch; it promises nothing about the time the connection
    /// itself then takes.
    pub(crate) async fn send(
        &self,
        client: &reqwest::Client,
        attempt: u32,
        decorate: impl FnOnce(reqwest::RequestBuilder) -> reqwest::RequestBuilder,
    ) -> Result<SendAttempt, GatewayRefusal> {
        let prove = || {
            self.producer
                .authority
                .verify_current()
                .inspect_err(|refusal| {
                    tracing::warn!(
                        "lean-ctx proxy: attempt {attempt} not sent: {}",
                        refusal.message()
                    );
                })
        };
        prove()?;
        let request = decorate(
            client
                .request(self.head.method.clone(), self.url.clone())
                .headers(self.head.headers.clone()),
        )
        .body(self.bytes.clone())
        .build()
        .map_err(|_| GatewayRefusal::unavailable("egress.request_unbuildable"))?;
        if request.method() != self.head.method
            || request.url() != &self.url
            || request.headers() != &self.head.headers
        {
            tracing::warn!("lean-ctx proxy: attempt {attempt} not sent: {HEAD_UNBOUND}");
            return Err(GatewayRefusal::withheld(HEAD_UNBOUND));
        }
        prove()?;
        let response = client.execute(request).await;
        let recorded = super::egress_observation::record_http_attempt(
            self.admission_digest.as_ref(),
            attempt,
            &response,
        )
        .await;
        Ok(SendAttempt { response, recorded })
    }
}

#[cfg(test)]
#[path = "admitted_tests.rs"]
mod tests;
