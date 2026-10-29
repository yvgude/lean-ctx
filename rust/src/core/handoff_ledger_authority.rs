// SPDX-License-Identifier: Apache-2.0
//! The sidecar of a handoff ledger: its binding and the record of the
//! elements that were bound when the ledger was written, with the check that
//! the record and the stored ledger agree.

use serde::{Deserialize, Serialize};

use super::{HandoffLedgerV1, ORIGIN_ALL_BOUND, ORIGIN_ORIGINLESS_PART};

/// The kind of a ledger element that is a copy of a stored item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PartKind {
    Finding,
    Fact,
}

/// One element of a stored ledger that was bound when the ledger was
/// written. Its identity is the tuple (kind, index, digest):
///
/// - `kind`: finding or fact;
/// - `index`: its position in the ledger's own list of that kind AS STORED —
///   `session.findings` or `knowledge.facts` of the serialized ledger, after
///   every omission;
/// - `digest`: [`LedgerPart::digest`] of the element at that position — for
///   a fact it covers category and key as well as the value.
///
/// Two elements with one text are told apart by their place. No content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct BoundEntry {
    pub(super) kind: PartKind,
    pub(super) index: usize,
    pub(super) digest: String,
}

/// Which elements of a ledger were bound at its creation, and how many: a
/// missing entry is detectable. Written for EVERY ledger created under
/// protection, with `count` 0 and no entries when none was bound; its
/// absence there is an inconsistency, never "no bound parts".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct BoundRecord {
    pub(super) count: usize,
    pub(super) entries: Vec<BoundEntry>,
}

pub(super) const INCONSISTENT_CODE: &str = "store.ledger_authority_inconsistent";
/// The tool identity the curated excerpts of a ledger are bound under.
pub(super) const CURATED_TOOL: &str = "ctx_handoff";

/// One finding or fact as it stands in a ledger.
#[derive(Debug, Clone, Copy)]
pub(crate) enum LedgerPart<'a> {
    /// A finding: its summary.
    Finding(&'a str),
    /// A knowledge fact.
    Fact {
        category: &'a str,
        key: &'a str,
        value: &'a str,
    },
}

impl LedgerPart<'_> {
    /// The text a store keeps for this part, and a binding binds.
    pub(super) fn text(&self) -> &str {
        match *self {
            Self::Finding(summary) => summary,
            Self::Fact { value, .. } => value,
        }
    }

    pub(super) fn kind(&self) -> PartKind {
        match self {
            Self::Finding(_) => PartKind::Finding,
            Self::Fact { .. } => PartKind::Fact,
        }
    }

    /// The digest of this part in the bound record of its ledger: BLAKE3
    /// (hex) over a domain tag and, each length-prefixed, `"finding"` and the
    /// summary — or `"fact"`, category, key and value, so that two facts with
    /// one value do not collide and a finding never equals a fact. It is of
    /// the exact text in the ledger and reveals none of it.
    pub(super) fn digest(&self) -> String {
        match *self {
            Self::Finding(summary) => part_digest(&["finding", summary]),
            Self::Fact {
                category,
                key,
                value,
            } => part_digest(&["fact", category, key, value]),
        }
    }
}

fn part_digest(fields: &[&str]) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"leanctx-ledger-bound-part-v1");
    for field in fields {
        hash.update(&(field.len() as u64).to_le_bytes());
        hash.update(field.as_bytes());
    }
    hash.finalize().to_hex().to_string()
}

/// The sidecar of a ledger: its binding and, beside it, the record of the
/// elements that were bound at creation. The binding's fields stay at the
/// top level (`flatten`), so a sidecar that is one serialized `StoredBinding`
/// — every sidecar written before the record existed — still parses, with
/// `bound_parts: None`, and a reader of the binding alone reads a new one.
///
/// The record says WHICH findings and facts entered the ledger through a
/// holding binding; the joined sources of all of them are the binding's
/// lineage. It is exactly as trusted as the binding next to it: both lie in
/// the one sidecar this store writes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct LedgerAuthority {
    #[serde(flatten)]
    pub(super) binding: crate::core::context_admission::stored::StoredBinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) bound_parts: Option<BoundRecord>,
}

impl HandoffLedgerV1 {
    /// The copies of stored items in this ledger, in the order and at the
    /// places they are stored: (kind, index in its own list, part).
    fn parts(&self) -> impl Iterator<Item = (PartKind, usize, LedgerPart<'_>)> {
        let findings = self
            .session
            .findings
            .iter()
            .enumerate()
            .map(|(index, summary)| (PartKind::Finding, index, LedgerPart::Finding(summary)));
        let facts = self
            .knowledge
            .facts
            .iter()
            .enumerate()
            .map(|(index, fact)| {
                let part = LedgerPart::Fact {
                    category: &fact.category,
                    key: &fact.key,
                    value: &fact.value,
                };
                (PartKind::Fact, index, part)
            });
        findings.chain(facts)
    }

    /// The bound record of this ledger as it is about to be stored:
    /// `bound(kind, index)` says which of its elements were bound. Derived
    /// from the final lists, so creation and load see the same places.
    pub(super) fn bound_record(&self, bound: impl Fn(PartKind, usize) -> bool) -> BoundRecord {
        let entries: Vec<BoundEntry> = self
            .parts()
            .filter(|(kind, index, _)| bound(*kind, *index))
            .map(|(kind, index, part)| BoundEntry {
                kind,
                index,
                digest: part.digest(),
            })
            .collect();
        BoundRecord {
            count: entries.len(),
            entries,
        }
    }

    /// Whether `authority`, the sidecar of this ledger written under
    /// protection, agrees with the ledger as it is stored. All of:
    ///
    /// - the bound record is present, its count is its number of entries,
    ///   and the entries are strictly ascending by (kind, index);
    /// - every entry names an element that exists at that place and has the
    ///   recorded digest (text edited, entries reordered or removed: no);
    /// - the marker is the one creation decides for this stored ledger and
    ///   this record ([`Self::checked_origin`] — the same function);
    /// - the lineage agrees: it names sources other than those of curated
    ///   excerpts only when at least one element is recorded (a record that
    ///   lost its entries does not pass as "nothing was bound").
    ///
    /// Anything else withholds the whole ledger. A known-bound element is
    /// never turned into unbound text by a damaged or missing record.
    pub(super) fn agrees_with(&self, authority: &LedgerAuthority) -> bool {
        let Some(record) = authority.bound_parts.as_ref() else {
            return false;
        };
        let parts: Vec<(PartKind, usize, LedgerPart<'_>)> = self.parts().collect();
        let ascending = record
            .entries
            .windows(2)
            .all(|pair| (pair[0].kind, pair[0].index) < (pair[1].kind, pair[1].index));
        let found = record.entries.iter().all(|entry| {
            parts.iter().any(|(kind, index, part)| {
                *kind == entry.kind && *index == entry.index && part.digest() == entry.digest
            })
        });
        let marker = authority.binding.origin() == self.checked_origin(record);
        let part_sources = authority
            .binding
            .file_sources()
            .is_some_and(|(_, files)| files.iter().any(|(_, tool, _)| *tool != CURATED_TOOL));
        record.count == record.entries.len()
            && ascending
            && found
            && marker
            && (record.count > 0 || !part_sources)
    }

    /// The marker of this ledger, created under protection, from the ledger
    /// as it is STORED and its bound record — the one predicate of creation
    /// and of [`Self::agrees_with`]: all-bound exactly when every finding and
    /// fact in the stored lists is recorded as bound and the ledger carries
    /// no other text without source authority (`has_unbound_text`: session
    /// snapshot, task, decisions, next steps, evidence keys, workflow,
    /// overlays); otherwise it has an originless part.
    pub(super) fn checked_origin(&self, record: &BoundRecord) -> &'static str {
        if record.count == self.parts().count() && !self.has_unbound_text() {
            ORIGIN_ALL_BOUND
        } else {
            ORIGIN_ORIGINLESS_PART
        }
    }
}
