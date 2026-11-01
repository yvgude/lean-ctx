// SPDX-License-Identifier: Apache-2.0
//! The C0 2.0 sync of one bound vault (SPEC-ENGINE §3.4.4, V1.6): clone or
//! tail the replica, turn what the capture gate lets through into signed
//! events, push them, tail again. Runs only when the enrolled Hive speaks
//! C0 2.0 ([`crate::core::hive::bus::relay_v2::speaks_v2`]); against a 1.x
//! Hive the draft path stays as it is.

use std::path::Path;

use serde_json::Value;

use super::driver::{self, PlaceApi, Verifier};
use crate::core::hive::auth::Session;
use crate::core::hive::capture::{self, emit};
use crate::core::hive::events::build::{Place, Signer};
use crate::core::hive::events::hlc::Stamp;
use crate::core::hive::events::sign;
use crate::core::hive::keyset_v3::{self, Purpose};
use crate::core::hive::replica::{Replica, crypt};
use crate::core::hive::vault::VaultRecord;
use crate::core::hive::workers::keys as worker_keys;
use crate::core::hive::{HiveError, knowledge::screening::Screen};

/// The routes of a session, with its token refresh.
pub(crate) struct SessionApi<'a>(pub(crate) &'a Session);

impl PlaceApi for SessionApi<'_> {
    fn get(&self, path: &str) -> Result<Value, HiveError> {
        self.0.with_client(|c| c.get(path))
    }
    fn get_bytes(&self, path: &str, max: usize) -> Result<Vec<u8>, HiveError> {
        self.0.with_client(|c| c.get_bytes(path, max))
    }
    fn post(&self, path: &str, body: &Value) -> Result<Value, HiveError> {
        self.0.with_client(|c| c.post(path, body))
    }
}

/// Host app the CLI and the session hooks capture as.
pub(crate) const CAPTURE_HOST: &str = "lean-ctx";
pub(crate) const CAPTURE_ROLE: &str = "capture";
/// Replica meta key: `created_at` of the newest fact the gate considered.
const CAPTURE_SINCE: &str = "capture_since";

/// What one vault sync did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VaultSync {
    pub cloned: bool,
    pub stored: usize,
    pub captured: usize,
    /// Episodes queued for sessions that ended (V1.4).
    pub episodes: usize,
    /// Own local facts removed because a purge took their text out.
    pub purged_local: usize,
    /// Purge tombstones newly acknowledged.
    pub purge_acks: usize,
    /// Facts copied in by 1.x pulls, removed once the replica serves the
    /// vault (V4.10).
    pub legacy_removed: usize,
    pub pushed: usize,
    /// Anchor states the automatic re-check queued (`to_verify`, `gone`,
    /// or `fresh` again).
    pub rechecked: usize,
    /// Own nodes the capture lifecycle queued as superseded (`edge.add
    /// supersedes` from their successor), withdrawn (`retract`) and
    /// re-anchored after a local re-pin (`field.set anchors`).
    pub superseded: usize,
    pub retracted: usize,
    pub confirmed: usize,
    /// Events the place refused for good: (event id, refusal).
    pub failed: Vec<(String, String)>,
    pub counts: capture::CaptureCounts,
    /// A deletions-only pass (the month's transfer used up) found an
    /// ordinary tail page: the quota is back.
    pub quota_back: bool,
}

/// Whether an event's signature verifies under the signer rules of §3.3.5:
/// a worker event by a key of this place that signs for its worker id, any
/// other actor by a Hive key of purpose `event` valid when received.
pub(super) fn signature_ok(session: &Session, place_ref: &str, event: &Value) -> bool {
    let key_id = event
        .get("key_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let received_at = event
        .get("received_at")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    match event.pointer("/actor/kind").and_then(Value::as_str) {
        Some("worker") => {
            let Some(worker) = event.get("worker_id").and_then(Value::as_str) else {
                return false;
            };
            let signs = |found: &Option<([u8; 32], Vec<String>)>| {
                matches!(found, Some((key, workers))
                    if workers.iter().any(|x| x == worker) && sign::verify(event, key))
            };
            let found = worker_keys::worker_key_or_fetch(session, place_ref, key_id, received_at)
                .ok()
                .flatten();
            if signs(&found) {
                return true;
            }
            // A key cached before its worker registered lists no (or an
            // older set of) workers: fetch the place's keys once more.
            found.is_some()
                && worker_keys::fetch(session, place_ref).is_ok()
                && signs(&worker_keys::worker_key(
                    &session.store,
                    place_ref,
                    key_id,
                    received_at,
                ))
        }
        Some("person" | "server" | "legacy_import") => keyset_v3::hive_keys(
            &session.store,
            session.root_key(),
            Purpose::Event,
            received_at,
        )
        .iter()
        .any(|(id, key)| id == key_id && sign::verify(event, key)),
        _ => false,
    }
}

/// The C0 2.0 place id of a vault: the canonical hyphenated UUID. A 1.x
/// link resolve may hand out the same id as 32 plain hex digits, which a
/// C0 2.0 Hive does not accept as its place name.
pub fn place_id(vault_id: &str) -> String {
    uuid::Uuid::parse_str(vault_id)
        .map_or_else(|_| vault_id.to_string(), |u| u.hyphenated().to_string())
}

/// The instance id as C0 2.0 names it: the hyphenated UUID of the 1.x
/// `inst_<32 hex>` id.
pub fn canonical_instance_id(instance_id: &str) -> String {
    place_id(instance_id.strip_prefix("inst_").unwrap_or(instance_id))
}

/// The automatic anchor re-check of one sync (v5 2.2.1): the deterministic
/// `--stale` task of `hive maintain`, queued after this run's capture
/// events (continuing `clock`). Returns how many states it queued and the
/// clock after them; an error is logged and counts as none, so the sync goes
/// on.
fn recheck_anchors(
    replica: &Replica,
    place: &Place,
    signer: &Signer<'_>,
    seal: Option<&crate::core::hive::events::sealed_body::SealKey>,
    checkout: &mut capture::refs::Checkout,
    now: u64,
    clock: Stamp,
) -> (usize, Stamp) {
    use crate::core::hive::review::maintain;
    let req = maintain::Request {
        vault: String::new(),
        stale: true,
        contradictions: false,
        dedupe: false,
        limit: maintain::DEFAULT_LIMIT,
        confirm: None,
    };
    let target = maintain::Target {
        replica,
        place,
        signer,
        seal,
    };
    match maintain::run_stale_at(&target, checkout, &req, now, clock) {
        Ok((counts, clock)) => (counts.done, clock),
        Err(e) => {
            tracing::warn!("hive: anchor re-check skipped: {e}");
            (0, clock)
        }
    }
}

/// Replica meta keys of the lifecycle gate, per checkout (`<key>:<project
/// hash>`; checkouts of one vault share its replica but each has its own
/// knowledge store): the knowledge journal's LSN and the time (unix ms) of
/// the last lifecycle pass.
const LIFECYCLE_LSN: &str = "lifecycle_lsn";
const LIFECYCLE_AT: &str = "lifecycle_at";
/// The longest a lifecycle pass is skipped while nothing moved: it covers
/// what neither the journal nor the tail records (a store edited outside the
/// engine).
const LIFECYCLE_FULL_MS: u64 = 60 * 60 * 1000;

/// Whether a lifecycle pass has work: the knowledge journal moved since the
/// last pass, the place delivered new events (another engine may have marked
/// a claim this checkout already re-pinned), or an hour passed; and the
/// journal LSN to record.
fn lifecycle_due(
    replica: &Replica,
    root: &Path,
    project_hash: &str,
    remote_changed: bool,
    now: u64,
) -> Result<(bool, u64), HiveError> {
    let head = crate::core::knowledge::knowledge_dir_of(&root.to_string_lossy())
        .map_or(0, |dir| crate::core::knowledge::journal::head_lsn(&dir));
    let read = |key: &str| -> Result<Option<u64>, HiveError> {
        Ok(replica
            .meta(&format!("{key}:{project_hash}"))?
            .and_then(|v| String::from_utf8(v.to_vec()).ok())
            .and_then(|s| s.parse().ok()))
    };
    let due = match (read(LIFECYCLE_LSN)?, read(LIFECYCLE_AT)?) {
        (Some(lsn), Some(at)) => {
            remote_changed || lsn != head || now.saturating_sub(at) >= LIFECYCLE_FULL_MS
        }
        _ => true,
    };
    Ok((due, head))
}

/// The capture lifecycle of one sync ([`capture::lifecycle`]): what the
/// agent superseded, withdrew or re-pinned since its facts were captured,
/// queued after this run's capture and re-check events. A failure is logged
/// and plans nothing, so the sync goes on.
#[allow(clippy::too_many_arguments)]
fn lifecycle(
    target: &crate::core::hive::review::maintain::Target<'_>,
    graph: &crate::core::hive::replica::fold::Graph,
    checkout: &mut capture::refs::Checkout,
    root: &Path,
    project_hash: &str,
    remote_changed: bool,
    now: u64,
    clock: Stamp,
) -> Result<capture::lifecycle::Queued, HiveError> {
    use capture::lifecycle;
    let idle = lifecycle::Queued {
        clock,
        ..lifecycle::Queued::default()
    };
    let (due, head) = lifecycle_due(target.replica, root, project_hash, remote_changed, now)?;
    if !due {
        return Ok(idle);
    }
    let Some(store) = lifecycle::store_facts(target.replica, &root.to_string_lossy()) else {
        tracing::debug!("hive: lifecycle skipped, the knowledge store is not readable");
        return Ok(idle);
    };
    let own = lifecycle::own_nodes(target.replica, project_hash)?;
    let present = lifecycle::present(graph, target.replica)?;
    let removed = lifecycle::removed_identities(&root.to_string_lossy());
    let actions = lifecycle::plan(
        &own,
        &store,
        &removed,
        |id| present.view(graph, id),
        &present,
    );
    let queued = lifecycle::queue(target, graph, checkout, &actions, clock, now)?;
    target.replica.set_meta(
        &format!("{LIFECYCLE_LSN}:{project_hash}"),
        head.to_string().as_bytes(),
    )?;
    target.replica.set_meta(
        &format!("{LIFECYCLE_AT}:{project_hash}"),
        now.to_string().as_bytes(),
    )?;
    Ok(queued)
}

/// The claims of `knowledge` (after `since`) that pass the capture gate.
///
/// v5 2.6.1: a `node:<id>` reference is valid only for a live node of this
/// place's folded `graph`, so the gate's checkout learns those ids first;
/// without them every `node:` reference was dropped as invalid and no
/// `derived_from` edge was ever written (dogfood e2e, scenario 2). A claim
/// that names a live node then carries a `derived_from` edge to it, its own
/// provenance (not telemetry).
fn plan_capture(
    knowledge: &crate::core::knowledge::ProjectKnowledge,
    since: Option<chrono::DateTime<chrono::Utc>>,
    run: &mut capture::CaptureRun<'_>,
    graph: &crate::core::hive::replica::fold::Graph,
) -> crate::core::hive::knowledge::learn::LearnPlan {
    run.checkout
        .nodes
        .extend(graph.live_nodes().map(|(id, _)| id.clone()));
    let mut plan = crate::core::hive::knowledge::learn::plan_learn(
        knowledge,
        &lean_ctx_protocol::hive::knowledge::ResearchContextDocumentV1::default(),
        since,
        usize::MAX,
        run,
    );
    capture::derived::link(&mut plan.captured, graph);
    plan
}

/// Sync the vault `record` bound to the checkout at `root`. While a quota
/// or policy throttle holds sync, nothing is asked of Hive: the replica
/// keeps its pending events and the refusal says until when.
/// Whether sync may capture this machine's knowledge into the vault: the
/// same switches the `knowledge remember` note reads.
pub(crate) fn capture_allowed(config: &crate::core::hive::HiveConfig) -> bool {
    config.enabled != Some(false) && config.auto_capture
}

pub fn sync_vault(
    session: &Session,
    record: &VaultRecord,
    root: &Path,
    screen: &mut Screen,
) -> Result<VaultSync, HiveError> {
    use crate::core::hive::throttle::{self, Scope};
    let now = chrono::Utc::now();
    if let Err(held) = throttle::check(&session.store, Scope::Sync, now) {
        // A used-up transfer still delivers deletions (protected capacity):
        // apply and ack them, then keep waiting, unless the answer shows
        // the quota is back (E2E-FINAL: purges stayed `propagating`).
        if !throttle::quota_holds(&session.store, Scope::Sync, now) {
            return Err(held);
        }
        match sync_vault_once(session, record, root, screen, true) {
            Ok(done) if done.quota_back => throttle::quota_lifted(&session.store, Scope::Sync),
            Ok(done) => {
                if done.purge_acks > 0 {
                    tracing::info!(
                        "hive: {} purge tombstones of {} applied and acknowledged while transfer is used up",
                        done.purge_acks,
                        record.vault_id
                    );
                }
                return Err(held);
            }
            Err(e) => {
                tracing::warn!(
                    "hive: deletions of {} not applied yet: {e}",
                    record.vault_id
                );
                return Err(held);
            }
        }
    }
    // A used-up transfer quota holds sync; a policy refusal of this vault
    // is this vault's alone and holds nothing else.
    let done = sync_vault_once(session, record, root, screen, false)
        .map_err(|e| throttle::note_on(&session.store, None, e, chrono::Utc::now()))?;
    throttle::passed(&session.store, Scope::Sync);
    Ok(done)
}

fn sync_vault_once(
    session: &Session,
    record: &VaultRecord,
    root: &Path,
    screen: &mut Screen,
    deletions_only: bool,
) -> Result<VaultSync, HiveError> {
    let store = &session.store;
    let place_ref = format!("vault:{}", place_id(&record.vault_id));
    let (mk, _) = crypt::master_key(store.config_dir(), false)?;
    let (mut replica, _) = Replica::open(store.data_dir(), &record.vault_id, &mk)?;
    // A sealed place keys its node ids, dedupe keys and body commitments
    // with pk_id from its place secret, never with a server salt (§3.3.4).
    // Only the sealing engine (or a v1 conversion's lease holder) creates
    // the secret; every other member receives it with a grant. A vault
    // sealed after creation is converted first by the engine that gets the
    // conversion lease, which creates the secret (§3.16 `v2_plain`, V13.E3).
    if record.sealed {
        if let Some(outcome) =
            crate::core::hive::convert::sync_conversion(session, record, &replica)?
        {
            tracing::info!(
                "hive: sealed conversion of {}: {outcome:?}",
                record.vault_id
            );
        }
        // The first engine of an empty sealed vault creates epoch 1 and the
        // place secret; any other one fetches its granted key, which carries
        // the secret (R135). Then members waiting for a key are granted
        // (auto, or confirmed before).
        if crate::core::hive::sealed::place_secret(store, &record.vault_id)?.is_none() {
            crate::core::hive::sealed::sealing_key(session, &record.vault_id, 0).map_err(|e| {
                match e {
                    // The server's `forbidden_right sealed`: no recipient approval yet.
                    HiveError::Forbidden => HiveError::Refused(
                        "this engine is not approved for the sealed vault yet: a maintainer \
                         approves it in the vault's sealed settings on the web"
                            .into(),
                    ),
                    other => other,
                }
            })?;
        }
        crate::core::hive::sealed::grant_known_recipients(session, &record.vault_id)?;
        let secret =
            crate::core::hive::sealed::place_secret(store, &record.vault_id)?.ok_or_else(|| {
                HiveError::Refused(
                    "sealed_secret_missing: ask a member to grant this engine access to the \
                     sealed vault"
                        .into(),
                )
            })?;
        replica.set_id_salt(&crate::core::hive::events::ids::sealed_pk_id(&secret))?;
    }
    // Fresh Hive keys when we can; the last verified set otherwise.
    if keyset_v3::refresh(session).is_err() && keyset_v3::load(store, session.root_key()).is_none()
    {
        return Err(HiveError::Protocol("no verified Hive key set v3".into()));
    }
    let now = chrono::Utc::now().timestamp_millis().unsigned_abs();
    let snapshot_keys: Vec<[u8; 32]> =
        keyset_v3::hive_keys(store, session.root_key(), Purpose::Snapshot, now)
            .into_iter()
            .map(|(_, k)| k)
            .collect();
    let check = |e: &Value| signature_ok(session, &place_ref, e);
    let held = |epoch: u32| crate::core::hive::sealed::held_key(store, &record.vault_id, epoch);
    let verifier = Verifier {
        snapshot_keys: &snapshot_keys,
        signature_ok: &check,
        sealed: record.sealed,
        epoch_key: record.sealed.then_some(&held as _),
    };
    let api = SessionApi(session);
    let mut report = VaultSync::default();
    if deletions_only {
        let Some(graph) = driver::deletions(&api, &mut replica, &place_ref, &verifier)? else {
            report.quota_back = true;
            return Ok(report);
        };
        let applied = super::purge::apply(&mut replica, &graph, now)?;
        report.purged_local =
            super::purge::purge_local_store(&replica, &graph, &root.to_string_lossy())?;
        report.purge_acks = applied.new_acks;
        let writer = super::writer::VaultWriter::open(session, CAPTURE_ROLE)?;
        let acks = super::purge::signed_pending(&replica, &place_ref, writer.keys.signing_key())?;
        driver::push_acks(&api, &place_ref, &acks)?;
        super::purge::mark_sent(&replica, &acks)?;
        return Ok(report);
    }
    let first = if replica.cursor()?.is_none() {
        driver::clone_place(&api, &mut replica, &place_ref, &verifier)?
    } else {
        driver::tail(&api, &mut replica, &place_ref, &verifier)?
    };
    report.cloned = first.cloned;
    report.stored = first.stored;
    // Events stored unverified get one more check against current keys
    // (a signer's worker can register after its key was first cached).
    if let Some(pk) = replica.id_salt()? {
        for (id, event) in replica.unverified()? {
            if super::records::event_verifies(&event, &pk, record.sealed, &check) {
                replica.mark_verified(&id)?;
            }
        }
    }
    // A plaintext row whose sealed copy is in the log folds as a no-op and
    // loses its plaintext body (§3.16 `v2_plain`).
    if record.sealed
        && let Some(pk) = replica.id_salt()?
        && crate::core::hive::convert::settle_converted(&replica, &pk)? > 0
    {
        replica.refold()?;
    }
    // V4.10 / §3.16: the vault is now delivered from its replica; the facts
    // its 1.x revisions copied into the project knowledge go, and the local
    // relations that named them move to the replica nodes.
    if first.cloned {
        report.legacy_removed =
            crate::core::hive::migrate::retire_on_clone(store, &replica, record, root)?;
    }

    // Purges (§3.4.6): deny purged statements, remove own local text, queue
    // one ack per tombstone.
    let graph = replica.refold()?;
    let applied = super::purge::apply(&mut replica, &graph, now)?;
    report.purged_local =
        super::purge::purge_local_store(&replica, &graph, &root.to_string_lossy())?;
    report.purge_acks = applied.new_acks;

    // Capture: what the gate lets through becomes signed node.add events.
    let pk_id = replica
        .id_salt()?
        .ok_or_else(|| HiveError::Local("replica has no id_salt".into()))?;
    // Worker ids and hlc.node are derived from the instance UUID as C0 2.0
    // names it (the server recomputes the worker id from it).
    let writer = super::writer::VaultWriter::open(session, CAPTURE_ROLE)?;
    let keys = &writer.keys;
    // E2E 27.10. E5: `[hive] auto_capture = false` (or a "not shared"
    // answer, which writes it) means nothing new leaves this machine.
    let capture_on = capture_allowed(&crate::core::config::Config::load().hive);
    if !capture_on {
        tracing::info!("hive: capture is off on this machine (`hive.auto_capture`)");
    }
    if let Some(knowledge) = capture_on
        .then(|| crate::core::knowledge::ProjectKnowledge::load(&root.to_string_lossy()))
        .flatten()
    {
        let (mut run, place_name) = capture::open_run(
            store.data_dir(),
            &crate::core::hive::vault::project_identity(root)
                .map(|i| lean_ctx_protocol::hive::vault::stable_identity(&i))
                .unwrap_or_default(),
            root,
            screen,
            chrono::Utc::now().timestamp_millis(),
        );
        run.pk_id = pk_id;
        let since = replica
            .meta(CAPTURE_SINCE)?
            .and_then(|v| String::from_utf8(v.to_vec()).ok())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| t.with_timezone(&chrono::Utc));
        let plan = plan_capture(&knowledge, since, &mut run, &graph);
        let place = super::writer::vault_place(record, &replica)?;
        let signer = writer.signer();
        // A sealed place's events are sealed with the newest epoch key held.
        let seal = super::writer::vault_seal(&session.store, record)?;
        // The checkout's name in `own_sources` (never shared): what the
        // capture lifecycle finds this checkout's own nodes by.
        let project_hash =
            blake3::hash(root.to_string_lossy().as_bytes()).to_hex()[..16].to_owned();
        let target = emit::Target {
            replica: &replica,
            place: &place,
            signer: &signer,
            project_hash: &project_hash,
            seal: seal.as_ref(),
        };
        let emitted = emit::emit(
            &target,
            &mut run.checkout,
            &plan.captured,
            Stamp::default(),
            now,
        )?;
        report.captured = emitted.queued.len();
        // V1.4: the checkout's sessions that ended become episodes.
        let (episodes, clock) = capture::episode::emit_ended(
            &target,
            root,
            store.data_dir(),
            &mut run,
            emitted.clock,
            now,
        )?;
        report.episodes = episodes;
        // v5 2.2.1: every sync re-checks the anchored claims against this
        // checkout (`hive maintain --stale`, automatically): a changed
        // anchor makes the claim `outdated` until a person confirms it
        // (`--confirm`). A failed re-check never fails the sync.
        let (rechecked, clock) = recheck_anchors(
            &replica,
            &place,
            &signer,
            seal.as_ref(),
            &mut run.checkout,
            now,
            clock,
        );
        report.rechecked = rechecked;
        // What the agent superseded, withdrew or re-pinned since capture
        // (the knowledge journal gates it; a failure never fails the sync).
        let maintain_target = crate::core::hive::review::maintain::Target {
            replica: &replica,
            place: &place,
            signer: &signer,
            seal: seal.as_ref(),
        };
        match lifecycle(
            &maintain_target,
            &graph,
            &mut run.checkout,
            root,
            &project_hash,
            report.stored > 0,
            now,
            clock,
        ) {
            Ok(queued) => {
                report.superseded = queued.superseded;
                report.retracted = queued.retracted;
                report.confirmed = queued.confirmed;
            }
            Err(e) => tracing::warn!("hive: knowledge lifecycle skipped: {e}"),
        }
        capture::finish_run(store.data_dir(), &place_name, &run)?;
        report.counts = run.counts.clone();
        if let Some(mark) = plan.watermark {
            replica.set_meta(CAPTURE_SINCE, mark.to_rfc3339().as_bytes())?;
        }
    }

    let acks = super::purge::signed_pending(&replica, &place_ref, keys.signing_key())?;
    let pushed = driver::push_pending(&api, &replica, &place_ref, &acks)?;
    if pushed.acked {
        super::purge::mark_sent(&replica, &acks)?;
    }
    report.pushed = pushed.logged;
    report.failed = pushed.failed;
    let after = driver::tail(&api, &mut replica, &place_ref, &verifier)?;
    report.stored += after.stored;
    Ok(report)
}

#[cfg(test)]
mod capture_tests {
    use ed25519_dalek::SigningKey;

    use super::*;
    use crate::core::hive::replica::fold::{FoldEvent, Graph};

    /// A live node of engine A, as B's replica folded it.
    const A_NODE: &str = "n_q7zk2m4xv3bt6yhw5rn2jc7dpa";
    /// A well-formed id the place does not hold.
    const UNKNOWN: &str = "n_aaaaaaaaaaaaaaaaaaaaaacab4";

    fn graph() -> Graph {
        let mut g = Graph::default();
        g.fold(&[FoldEvent {
            seq: 1,
            event_id: format!("E{:025}", 1),
            op: "node.add".into(),
            target: Some(A_NODE.into()),
            node_type: Some("fact".into()),
            body: Some(serde_json::json!({
                "statement": "The ledger close holds advisory lock 42.",
                "applies_to": []
            })),
            sig_invalid: false,
            provenance: Some("workspace".into()),
            review_state: Some("confirmed".into()),
        }])
        .unwrap();
        g
    }

    /// Dogfood e2e scenario 2: B remembers a claim with `--ref file:… --ref
    /// node:<A's live node>` and syncs. The sync's capture step must keep
    /// the node reference and queue a `derived_from` edge after the claim;
    /// before the fix the gate knew no node ids and dropped it.
    #[test]
    fn a_remembered_node_reference_becomes_a_derived_from_edge_on_sync() {
        let repo = capture::tests::repo();
        let mut f = capture::tests::fact(
            "arch",
            "The nightly export waits for advisory lock 42 before the ledger close.",
        );
        f.capture.refs = vec![
            "file:src/lib.rs".into(),
            format!("node:{A_NODE}"),
            format!("node:{UNKNOWN}"),
        ];
        let mut knowledge =
            crate::core::knowledge::ProjectKnowledge::new(&repo.path().to_string_lossy());
        knowledge.facts.push(f);

        let mut screen =
            Screen::with_rules(Vec::new(), std::sync::Arc::new(Vec::new()), Vec::new());
        let mut run =
            capture::CaptureRun::new(&mut screen, repo.path(), [9; 32], false, Vec::new(), 0);
        let graph = graph();
        let plan = plan_capture(&knowledge, None, &mut run, &graph);
        assert_eq!(plan.captured.len(), 1, "{:?}", plan.dropped);
        assert_eq!(plan.captured[0].derived_from, [A_NODE]);
        assert!(
            !plan.captured[0].refs.iter().any(|r| r.contains(UNKNOWN)),
            "a node the place does not hold is no reference"
        );

        let data = tempfile::tempdir().unwrap();
        let (replica, _) = Replica::open(data.path(), "pl_sync_derive", &[3; 32]).unwrap();
        let key = SigningKey::from_bytes(&[0x49; 32]);
        let place = Place {
            kind: "vault".into(),
            id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
            pk_id: [9; 32],
            causal_base: 1,
        };
        let signer = Signer {
            key: &key,
            hlc_node: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b".into(),
            worker_id: "wkr_vk4oelpv5lzjmev44hun".into(),
        };
        let target = emit::Target {
            replica: &replica,
            place: &place,
            signer: &signer,
            project_hash: "ph",
            seal: None,
        };
        let out = emit::emit(
            &target,
            &mut run.checkout,
            &plan.captured,
            Stamp::default(),
            1_792_800_000_000,
        )
        .unwrap();
        assert_eq!(out.queued.len(), 1);
        let pending: Vec<Value> = replica
            .pending()
            .unwrap()
            .iter()
            .map(|(_, b)| serde_json::from_slice(b).unwrap())
            .collect();
        let ops: Vec<&str> = pending.iter().map(|e| e["op"].as_str().unwrap()).collect();
        assert_eq!(ops, ["node.add", "edge.add"]);
        let applies_to = pending[0]["body"]["applies_to"].as_array().unwrap();
        assert!(
            applies_to.iter().any(|a| *a == format!("node:{A_NODE}")),
            "the claim body names the node the server counts: {applies_to:?}"
        );
        assert_eq!(pending[1]["body"]["kind"], "derived_from");
        assert_eq!(pending[1]["body"]["from"], out.queued[0].1.as_str());
        assert_eq!(pending[1]["body"]["to"], A_NODE);
    }
}

#[cfg(test)]
mod tests {
    /// E2E 27.10. E5: `knowledge remember` said "switched off" while the
    /// next sync captured anyway; both read this one switch now.
    #[test]
    fn capture_follows_auto_capture_and_the_off_switch() {
        use crate::core::hive::HiveConfig;
        assert!(super::capture_allowed(&HiveConfig::default()));
        let mut off = HiveConfig::default();
        crate::core::hive::connect::without_sharing(&mut off);
        assert!(!super::capture_allowed(&off), "a \"not shared\" answer");
        let disabled = HiveConfig {
            enabled: Some(false),
            ..HiveConfig::default()
        };
        assert!(!super::capture_allowed(&disabled));
    }

    /// Found on the dogfood Hive: a 1.x link stores the vault id as plain
    /// hex, the C0 2.0 place name is the hyphenated UUID.
    #[test]
    fn place_ids_are_hyphenated_uuids() {
        assert_eq!(
            super::place_id("8b1ba534f3284cc1b939dafd92863522"),
            "8b1ba534-f328-4cc1-b939-dafd92863522"
        );
        assert_eq!(
            super::place_id("8b1ba534-f328-4cc1-b939-dafd92863522"),
            "8b1ba534-f328-4cc1-b939-dafd92863522"
        );
        assert_eq!(super::place_id("not-a-uuid"), "not-a-uuid");
        assert_eq!(
            super::canonical_instance_id("inst_a164e846e1ad4d7fb66e4fed4e28b2aa"),
            "a164e846-e1ad-4d7f-b66e-4fed4e28b2aa"
        );
    }
}
