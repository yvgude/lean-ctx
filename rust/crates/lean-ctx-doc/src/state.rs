// SPDX-License-Identifier: Apache-2.0

//! The derived doc state of a page (C0 2.2 `doc-states`), never stored:
//! fresh, noted (every derived anchor that moved is covered by an open note
//! that accounted for exactly what is observed now) or stale.

use serde::{Deserialize, Serialize};

/// A page's state as a reader shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocState {
    Fresh,
    Noted,
    Stale,
}

/// Why a page is not fresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateReason {
    AnchorChanged,
    AnchorGone,
}

/// One moved anchor: its ref and the digest observed now (`None`: gone).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drift {
    #[serde(rename = "ref")]
    pub reference: String,
    /// `observed` in a drift entry, `digest` in a note's cover.
    #[serde(alias = "digest")]
    pub observed: Option<String>,
}

/// What a note accounted for: a ref at a digest (`None`: as gone).
pub type Cover = Drift;

/// One note about the page or one of its sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteCovers {
    pub open: bool,
    pub covers: Vec<Cover>,
}

/// The verdict with its reason and the derived refs no open note covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub state: DocState,
    pub reason: Option<StateReason>,
    pub uncovered: Vec<String>,
}

/// The doc state of a page from its `anchor_state` (`fresh`, `to_verify`,
/// `gone` or absent), the refs of its derived anchors, its `drift` and the
/// notes about it.
pub fn of(
    anchor_state: Option<&str>,
    derived: &[&str],
    drift: &[Drift],
    notes: &[NoteCovers],
) -> Verdict {
    let fresh = Verdict {
        state: DocState::Fresh,
        reason: None,
        uncovered: Vec::new(),
    };
    if matches!(anchor_state, None | Some("fresh")) {
        return fresh;
    }
    let moved: Vec<&Drift> = drift
        .iter()
        .filter(|d| derived.contains(&d.reference.as_str()))
        .collect();
    if moved.is_empty() && !drift.is_empty() {
        // Only cited anchors moved: they never drive the state.
        return fresh;
    }
    let gone = anchor_state == Some("gone") || moved.iter().any(|d| d.observed.is_none());
    let reason = Some(if gone {
        StateReason::AnchorGone
    } else {
        StateReason::AnchorChanged
    });
    let mut uncovered: Vec<String> = moved
        .iter()
        .filter(|d| {
            !notes.iter().filter(|n| n.open).any(|n| {
                n.covers
                    .iter()
                    .any(|c| c.reference == d.reference && c.observed == d.observed)
            })
        })
        .map(|d| d.reference.clone())
        .collect();
    uncovered.sort();
    // A writer that sent no drift detail leaves nothing to cover: stale.
    let state = if moved.is_empty() || !uncovered.is_empty() {
        DocState::Stale
    } else {
        DocState::Noted
    };
    Verdict {
        state,
        reason,
        uncovered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D7: &str = "7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c7c";
    const D5: &str = "5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";
    const DERIVED: &[&str] = &["file:a.rs"];

    fn drift(r: &str, d: Option<&str>) -> Drift {
        Drift {
            reference: r.into(),
            observed: d.map(Into::into),
        }
    }
    fn note(open: bool, d: Option<&str>) -> NoteCovers {
        NoteCovers {
            open,
            covers: vec![drift("file:a.rs", d)],
        }
    }
    fn check(
        name: &str,
        st: Option<&str>,
        dr: &[Drift],
        notes: &[NoteCovers],
        want: (DocState, Option<StateReason>, &[&str]),
    ) {
        let v = of(st, DERIVED, dr, notes);
        assert_eq!((v.state, v.reason), (want.0, want.1), "{name}");
        assert_eq!(v.uncovered, want.2, "{name}");
    }

    // C0 2.2 vectors (hive-v2 vectors/doc_state).
    #[test]
    fn doc_state_vectors() {
        use DocState::{Fresh, Noted, Stale};
        use StateReason::{AnchorChanged as Changed, AnchorGone as Gone};
        let moved = [drift("file:a.rs", Some(D7))];
        check("no_state", None, &[], &[], (Fresh, None, &[]));
        check("fresh", Some("fresh"), &[], &[], (Fresh, None, &[]));
        check(
            "changed_uncovered",
            Some("to_verify"),
            &moved,
            &[],
            (Stale, Some(Changed), &["file:a.rs"]),
        );
        check(
            "changed_covered",
            Some("to_verify"),
            &moved,
            &[note(true, Some(D7))],
            (Noted, Some(Changed), &[]),
        );
        check(
            "covered_an_older_move",
            Some("to_verify"),
            &moved,
            &[note(true, Some(D5))],
            (Stale, Some(Changed), &["file:a.rs"]),
        );
        check(
            "resolved_note_does_not_cover",
            Some("to_verify"),
            &moved,
            &[note(false, Some(D7))],
            (Stale, Some(Changed), &["file:a.rs"]),
        );
        check(
            "migrated_null_cover_does_not_cover",
            Some("to_verify"),
            &moved,
            &[note(true, None)],
            (Stale, Some(Changed), &["file:a.rs"]),
        );
        check(
            "gone_covered",
            Some("gone"),
            &[drift("file:a.rs", None)],
            &[note(true, None)],
            (Noted, Some(Gone), &[]),
        );
        check(
            "cites_drift_only",
            Some("to_verify"),
            &[drift("file:docs/b.md", Some(D7))],
            &[],
            (Fresh, None, &[]),
        );
        check(
            "state_without_drift",
            Some("to_verify"),
            &[],
            &[],
            (Stale, Some(Changed), &[]),
        );
    }
}
