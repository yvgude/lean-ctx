// SPDX-License-Identifier: Apache-2.0
//! Required-organization marker: the endpoint's durable record that an
//! organization policy is, or was, required.
//!
//! The policy artifact and the trust store can both disappear; without a
//! record of its own the endpoint would then look unconfigured. The marker is
//! that record. It is read into the policy snapshot (and its fingerprint) on
//! every refresh, it only moves forward, and a lifted requirement stays on
//! record. The rules for accepting a policy or an unpin are not here: this
//! module stores the state a verified decision produced.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use lean_ctx_protocol::TeamId;
use lean_ctx_protocol::org_policy_verifier::{
    OrgRequiredMarkerAdvance, OrgRequiredMarkerEntryV1, OrgRequiredMarkerV1,
};

const MARKER_FILE: &str = "org-required-marker.json";
const LOCK_FILE: &str = "org-required-marker.lock";
/// Writers queue behind one another for at most this long; a writer that
/// cannot get the lock reports an error and changes nothing.
const LOCK_WAIT: Duration = Duration::from_secs(2);

/// Where the marker lives. Read-only resolution: taking a snapshot must not
/// create configuration directories.
pub(crate) fn marker_path_read_only() -> Result<PathBuf, String> {
    Ok(crate::core::paths::config_dir_read_only()?.join(MARKER_FILE))
}

/// Parses stored marker text. Anything but a strict, canonical, forward-moving
/// marker is an error: a damaged marker is never read as "no requirement".
pub(crate) fn parse(text: &str) -> Result<OrgRequiredMarkerV1, String> {
    OrgRequiredMarkerV1::parse(text.as_bytes())
        .map_err(|_| "org required marker is invalid".to_string())
}

/// The marker a decision was made for: what the caller captured before it
/// asked. A decision binds that state and no other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Predecessor<'a> {
    /// The exact stored text the decision saw.
    Stored(&'a str),
    /// No marker existed. Only a separately authorized first enrollment may
    /// say so; an ordinary verifier answer never creates the first marker.
    /// That enrollment path is specified and tested but not yet offered.
    #[allow(dead_code)]
    NeverStored,
}

/// Records the next accepted state, atomically and under an exclusive lock.
///
/// The stored marker is read again under the lock and must still be exactly
/// the `predecessor` the decision was made for. A marker that was lost,
/// damaged or changed in between refuses the write: nothing is re-created
/// from an answer about a state that is no longer there. Two writers
/// therefore cannot both append from the same predecessor; the lower or
/// conflicting entry is refused by [`OrgRequiredMarkerV1::advance`]. The file
/// is replaced by rename, so a reader sees the old or the new marker, never a
/// partial one. There is no operation that removes or resets the marker.
#[cfg(test)]
pub(crate) fn advance(
    organization: &TeamId,
    entry: OrgRequiredMarkerEntryV1,
    predecessor: Predecessor<'_>,
) -> Result<OrgRequiredMarkerAdvance, String> {
    advance_within(organization, entry, predecessor, Instant::now() + LOCK_WAIT)
        .map(|(outcome, _)| outcome)
}

/// [`advance`] for a decision that is only good until `deadline`: the wait
/// for the lock is cut to the time left, and the time is checked under the
/// lock and once more after the new marker is staged, immediately before it
/// replaces the stored one. That last check is the commit admission: a
/// decision whose request has run out by then is not applied. What the
/// operating system needs for the replace itself after admission is outside
/// this bound. Returns the marker text as it is stored now.
pub(crate) fn advance_within(
    organization: &TeamId,
    entry: OrgRequiredMarkerEntryV1,
    predecessor: Predecessor<'_>,
    deadline: Instant,
) -> Result<(OrgRequiredMarkerAdvance, String), String> {
    const LATE: &str = "org required marker decision came too late";
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(LATE.into());
    }
    let dir = crate::core::paths::config_dir()?;
    std::fs::create_dir_all(&dir).map_err(|_| "org required marker directory is unavailable")?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))
        .map_err(|_| "org required marker lock is unavailable")?;
    crate::core::file_lock::acquire_exclusive_timeout(&lock, left.min(LOCK_WAIT))?;

    let path = dir.join(MARKER_FILE);
    let stored = crate::core::policy::files::read(&path, false)?;
    if Instant::now() >= deadline {
        return Err(LATE.into());
    }
    let (marker, outcome) = match (stored.as_deref(), predecessor) {
        (Some(text), Predecessor::Stored(captured)) if text == captured => {
            let mut marker = parse(text)?;
            let outcome = marker
                .advance(organization, entry)
                .map_err(|_| "org required marker refuses this state")?;
            (marker, outcome)
        }
        (None, Predecessor::NeverStored) => (
            OrgRequiredMarkerV1::first(organization.clone(), entry)
                .map_err(|_| "org required marker state is invalid")?,
            OrgRequiredMarkerAdvance::Appended,
        ),
        _ => return Err("org required marker is not the one this decision was made for".into()),
    };
    if outcome != OrgRequiredMarkerAdvance::Appended {
        // Nothing is written: what is stored is still the captured text.
        return Ok((outcome, stored.unwrap_or_default()));
    }
    let bytes = marker
        .canonical_bytes()
        .map_err(|_| "org required marker cannot be encoded")?;
    let text = String::from_utf8(bytes).map_err(|_| "org required marker cannot be encoded")?;
    // The new marker is staged first; whether it may replace the stored one is
    // decided on a fresh clock at the last point this process controls, right
    // before the replace. Past the deadline the staged file is discarded and
    // the stored marker stays as it was.
    crate::core::atomic_fs::try_atomic_write_if(&path, text.as_bytes(), None, || {
        Instant::now() < deadline
    })
    .map_err(|error| match error.kind() {
        std::io::ErrorKind::TimedOut => LATE,
        _ => "org required marker cannot be persisted",
    })?;
    Ok((outcome, text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::org_policy_verifier::OrgRequiredMarkerStateV1;
    use lean_ctx_protocol::{RecordDigest, TeamTimestampV1};

    fn digest(fill: char) -> RecordDigest {
        RecordDigest::new(format!("sha256:{}", fill.to_string().repeat(64))).unwrap()
    }

    fn entry(sequence: u64, state: OrgRequiredMarkerStateV1) -> OrgRequiredMarkerEntryV1 {
        OrgRequiredMarkerEntryV1 {
            sequence,
            recorded_at: TeamTimestampV1::new("2026-10-10T09:00:00Z").unwrap(),
            state,
        }
    }

    fn required(sequence: u64, fill: char) -> OrgRequiredMarkerEntryV1 {
        entry(
            sequence,
            OrgRequiredMarkerStateV1::Required {
                policy_digest: digest(fill),
            },
        )
    }

    fn stored_text() -> String {
        crate::core::policy::files::read(&marker_path_read_only().unwrap(), true)
            .unwrap()
            .unwrap()
    }

    fn stored() -> OrgRequiredMarkerV1 {
        parse(&stored_text()).unwrap()
    }

    /// The next state for a decision made on the marker as it is stored now.
    fn advance_stored(
        organization: &TeamId,
        entry: OrgRequiredMarkerEntryV1,
    ) -> Result<OrgRequiredMarkerAdvance, String> {
        advance(organization, entry, Predecessor::Stored(&stored_text()))
    }

    #[test]
    fn the_stored_marker_moves_forward_and_keeps_an_unpin_on_record() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let acme = TeamId::new("acme").unwrap();

        assert_eq!(
            advance(&acme, required(3, 'a'), Predecessor::NeverStored).unwrap(),
            OrgRequiredMarkerAdvance::Appended
        );
        assert_eq!(
            advance_stored(&acme, required(3, 'a')).unwrap(),
            OrgRequiredMarkerAdvance::Unchanged
        );
        assert!(advance_stored(&acme, required(2, 'b')).is_err());
        assert!(advance_stored(&acme, required(3, 'b')).is_err());
        assert!(advance_stored(&TeamId::new("other").unwrap(), required(4, 'b')).is_err());
        assert_eq!(stored().entries.len(), 1, "refusals persist nothing");

        let unpin = entry(
            3,
            OrgRequiredMarkerStateV1::Unpinned {
                unpin_digest: digest('d'),
            },
        );
        assert_eq!(
            advance_stored(&acme, unpin).unwrap(),
            OrgRequiredMarkerAdvance::Appended
        );
        let marker = stored();
        assert!(!marker.is_required());
        assert_eq!(marker.entries.len(), 2, "the requirement stays on record");
    }

    #[test]
    fn a_damaged_marker_is_an_error_not_an_absent_requirement() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let acme = TeamId::new("acme").unwrap();
        advance(&acme, required(1, 'a'), Predecessor::NeverStored).unwrap();
        let path = marker_path_read_only().unwrap();
        std::fs::write(&path, b"{}").unwrap();

        assert!(parse("{}").is_err());
        assert!(advance_stored(&acme, required(2, 'a')).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{}", "not overwritten");
    }

    #[test]
    fn a_decision_whose_deadline_passes_while_it_waits_is_not_recorded() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let acme = TeamId::new("acme").unwrap();
        advance(&acme, required(1, 'a'), Predecessor::NeverStored).unwrap();
        let captured = stored_text();
        let by = |deadline: Instant| {
            advance_within(
                &acme,
                required(2, 'b'),
                Predecessor::Stored(&captured),
                deadline,
            )
        };

        // Already run out: refused without touching the lock.
        assert!(by(Instant::now()).is_err());

        // Another writer keeps the lock. The wait ends with the decision's
        // own deadline — reached, and well before the general lock wait
        // would give up. This is the marker alone; no verifier is involved.
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .open(marker_path_read_only().unwrap().with_file_name(LOCK_FILE))
            .unwrap();
        crate::core::file_lock::acquire_exclusive_timeout(&lock, LOCK_WAIT).unwrap();
        let started = Instant::now();
        let deadline = started + Duration::from_millis(200);
        assert!(by(deadline).is_err());
        assert!(Instant::now() >= deadline, "refused before its deadline");
        assert!(started.elapsed() < LOCK_WAIT / 2);
        assert_eq!(stored_text(), captured, "nothing recorded late");
        drop(lock);

        // The same decision, in time, is recorded.
        assert_eq!(
            by(Instant::now() + LOCK_WAIT).unwrap().0,
            OrgRequiredMarkerAdvance::Appended
        );
    }

    #[test]
    fn a_decision_is_recorded_only_on_the_marker_it_was_made_for() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let acme = TeamId::new("acme").unwrap();
        advance(&acme, required(1, 'a'), Predecessor::NeverStored).unwrap();
        let captured = stored_text();
        advance_stored(&acme, required(2, 'b')).unwrap();
        let moved_on = stored_text();
        assert_ne!(moved_on, captured);

        // The marker moved on after the capture: the older decision is stale.
        assert!(advance(&acme, required(3, 'c'), Predecessor::Stored(&captured)).is_err());
        // A marker exists: nothing may claim that none was ever stored.
        assert!(advance(&acme, required(3, 'c'), Predecessor::NeverStored).is_err());
        assert_eq!(stored_text(), moved_on, "refusals persist nothing");

        // The marker is lost: an answer about it does not bring one back.
        let path = marker_path_read_only().unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(advance(&acme, required(3, 'c'), Predecessor::Stored(&moved_on)).is_err());
        assert!(
            !path.exists(),
            "no marker is re-created from a stale decision"
        );
    }
}
