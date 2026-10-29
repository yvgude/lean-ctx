// SPDX-License-Identifier: Apache-2.0

//! Vault knowledge: the `ctxpkg.research-context.v1` document, its
//! revisions, and draft proposals streamed from engine sessions.
//!
//! - `GET /v1/vaults/{vault_id}/knowledge` (`If-None-Match: "<revision>"`)
//!   → [`KnowledgeRevisionV1`] with `ETag: "<revision>"`, or 304
//! - `GET /v1/vaults/{vault_id}/knowledge/revisions?after=&limit=` → [`RevisionListV1`]
//! - `POST /v1/vaults/{vault_id}/proposals` [`DraftProposalV1`] → [`ProposalAckV1`];
//!   `Idempotency-Key` = the engine's outbox item id. A draft whose content is
//!   identical to an existing engine draft of the vault maps to that proposal
//!   (same `number`), whichever instance sent it first.
//! - `GET /v1/vaults/{vault_id}/proposals?state=&mine=1` → [`ProposalListV1`]
//! - 1.6: `GET /v1/vaults/{vault_id}/proposals/{number}` → [`ProposalV1`]:
//!   the draft exactly as its engine sent it (a sealed patch stays sealed),
//!   for engine-side review of sealed vaults
//! - `PUT /v1/vaults/{vault_id}/knowledge` [`KnowledgePutRequestV1`] →
//!   [`KnowledgeRevisionV1`], or 409 `revision_conflict`; needs `knowledge.merge`.
//!   The new revision is always `expected_revision + 1` (a sealed revision is
//!   sealed for that number). 1.6: `merged_proposals` marks those proposals
//!   merged at the new revision in the same transaction
//!
//! - 1.9: `GET /v1/vaults/{vault_id}/knowledge/diff?from=&to=` →
//!   [`RevisionDiffV1`]: the ops from one released revision to another, so
//!   an engine with a large document fetches deltas (in the clear only; a
//!   sealed vault answers `409 invalid_state` reason `sealed`)
//!
//! Revision 0 is the empty document. A draft becomes a revision only after
//! review in Hive.
//!
//! Quarantine (1.9, owner O3): a revision that an auto-merge threshold
//! produced (no person reviewed it) starts `quarantined`. `GET …/knowledge`
//! delivers the newest *released* revision (`revision` is that delivered
//! head) and counts the quarantined revisions after it in
//! `quarantined_after`; `?revision=N` reads a quarantined one for review
//! (right `knowledge.merge`). An engine never hands quarantined content to
//! any agent except for that review. A person releases it, or rolls it back,
//! within 24 hours; otherwise Hive proposes a task (origin
//! `quarantine_timeout`). A rollback appends a new revision with the document
//! of an earlier one (`rolls_back`). In a sealed vault an engine performs
//! it, because only an engine can re-seal for the new number.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use super::anchor::{CodeAnchorV1, validate_anchors};
use super::sealed::SealedBlobV1;
use super::{require_schema, validate_hex64, validate_id, validate_text};
use crate::{UtcTimestamp, ValidationError};

pub const KNOWLEDGE_REVISION_SCHEMA: &str = "leanctx.hive.knowledge-revision/v1";
pub const REVISION_LIST_SCHEMA: &str = "leanctx.hive.revision-list/v1";
pub const DRAFT_PROPOSAL_SCHEMA: &str = "leanctx.hive.proposal/v1";
pub const PROPOSAL_ACK_SCHEMA: &str = "leanctx.hive.proposal-ack/v1";
pub const PROPOSAL_LIST_SCHEMA: &str = "leanctx.hive.proposal-list/v1";
pub const KNOWLEDGE_PUT_REQUEST_SCHEMA: &str = "leanctx.hive.knowledge-put-request/v1";
/// 1.6: one proposal with its draft.
pub const PROPOSAL_VIEW_SCHEMA: &str = "leanctx.hive.proposal-view/v1";
/// Most proposals one revision may merge.
pub const MAX_MERGED_PROPOSALS: usize = 50;

/// Format id of the knowledge document (the package format keeps its name).
pub const RESEARCH_CONTEXT_FORMAT: &str = "ctxpkg.research-context.v1";
/// 1.9 adds `gotcha`, `dead_end`, `experience` and `convention`.
pub const FACT_KINDS: [&str; 9] = [
    "claim",
    "definition",
    "decision",
    "observation",
    "procedure",
    "gotcha",
    "dead_end",
    "experience",
    "convention",
];
/// Edges between facts; 1.9 adds `explains`. Edges to code, work and actors
/// belong to the graph view (`super::graph`), not to the document.
pub const RELATION_TYPES: [&str; 6] = [
    "supports",
    "depends_on",
    "contradicts",
    "supersedes",
    "related_to",
    "explains",
];
/// Ops per proposal patch.
pub const MAX_PATCH_OPS: usize = 50;
/// 1.9: 5,000 facts (wiki scale); engines fetch deltas with
/// [`RevisionDiffV1`].
pub const MAX_FACTS: usize = 5000;
pub const MAX_RELATIONS: usize = 15_000;
pub const MAX_REFERENCES: usize = 50_000;
/// Characters of the serialized document.
pub const MAX_DOCUMENT_CHARS: usize = 8_000_000;
/// Largest knowledge request/response body (a full document fits).
pub const MAX_KNOWLEDGE_BODY_BYTES: usize = 16 * 1024 * 1024;
/// 1.9: wiki pages per document, items per page, characters of a prose item.
pub const MAX_PAGES: usize = 500;
pub const MAX_PAGE_ITEMS: usize = 200;
pub const MAX_PROSE_CHARS: usize = 4000;
/// Revisions per page.
pub const MAX_REVISIONS_PAGE: usize = 100;

/// One fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactV1 {
    pub id: String,
    pub statement: String,
    pub kind: String,
    #[serde(rename = "appliesTo", default, skip_serializing_if = "Vec::is_empty")]
    pub applies_to: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    /// Proposal credits assigned by Hive on merge; never sent in a patch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from_contributions: Vec<String>,
    /// 1.9: where in the code the fact applies ([`CodeAnchorV1`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<CodeAnchorV1>,
}

/// 1.9: one item of a wiki page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum PageItemV1 {
    /// A fact of the document, shown in place.
    Fact { id: String },
    /// Connecting text written for the page.
    Prose { text: String },
}

/// 1.9: a readable wiki page built from facts (the "tree" of the ctxpkg
/// mapping). Pages are part of the reviewed document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageV1 {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub items: Vec<PageItemV1>,
}

/// A typed edge between two facts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationV1 {
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub kind: String,
}

/// The knowledge document of one vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchContextDocumentV1 {
    pub format: String,
    #[serde(default)]
    pub facts: Vec<FactV1>,
    #[serde(default)]
    pub relations: Vec<RelationV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_gaps: Vec<String>,
    /// 1.9: wiki pages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<PageV1>,
}

impl Default for ResearchContextDocumentV1 {
    fn default() -> Self {
        Self {
            format: RESEARCH_CONTEXT_FORMAT.into(),
            facts: Vec::new(),
            relations: Vec::new(),
            open_gaps: Vec::new(),
            pages: Vec::new(),
        }
    }
}

/// One edit of a proposal patch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "op", rename_all = "snake_case")]
pub enum PatchOpV1 {
    AddFact {
        fact: FactV1,
    },
    UpdateFact {
        id: String,
        fact: FactV1,
    },
    /// Also drops the fact's relations and its items on pages (1.9); a page
    /// left without items goes too.
    RetractFact {
        id: String,
    },
    AddRelation {
        relation: RelationV1,
    },
    RemoveRelation {
        relation: RelationV1,
    },
    AddGap {
        text: String,
    },
    CloseGap {
        text: String,
    },
    /// 1.9.
    AddPage {
        page: PageV1,
    },
    /// 1.9: replaces the page with `id` (`page.id` must equal it).
    UpdatePage {
        id: String,
        page: PageV1,
    },
    /// 1.9.
    RemovePage {
        id: String,
    },
}

pub(crate) fn fact_id_ok(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 100
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

fn unique_texts(
    field: &str,
    items: &[String],
    max_items: usize,
    max_chars: usize,
) -> Result<(), ValidationError> {
    if items.len() > max_items {
        return Err(ValidationError::new(format!(
            "{field} allows at most {max_items} entries"
        )));
    }
    let mut seen = HashSet::new();
    for item in items {
        validate_text(item, max_chars, field, false)?;
        if !seen.insert(item) {
            return Err(ValidationError::new(format!(
                "{field} must not contain duplicates"
            )));
        }
    }
    Ok(())
}

impl FactV1 {
    /// Checks of one fact on its own (kind, id, bounds).
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !fact_id_ok(&self.id) {
            return Err(ValidationError::new(format!(
                "fact id {:?} may contain only letters, numbers, dot, underscore, colon and hyphen (max 100)",
                self.id
            )));
        }
        if !FACT_KINDS.contains(&self.kind.as_str()) {
            return Err(ValidationError::new(format!(
                "fact {:?} has unsupported kind {:?}",
                self.id, self.kind
            )));
        }
        validate_text(&self.statement, 2000, "statement", true)?;
        unique_texts("appliesTo", &self.applies_to, 16, 500)?;
        unique_texts("sources", &self.sources, 16, 2000)?;
        if self.from_contributions.len() > 16 {
            return Err(ValidationError::new(
                "from_contributions allows at most 16 entries",
            ));
        }
        validate_anchors(&self.anchors)
    }
}

impl PageV1 {
    /// Checks of one page on its own; fact references are checked against
    /// the document by [`ResearchContextDocumentV1::validate`].
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !fact_id_ok(&self.id) {
            return Err(ValidationError::new("page id is malformed"));
        }
        validate_text(&self.title, 200, "page title", false)?;
        if self.items.is_empty() || self.items.len() > MAX_PAGE_ITEMS {
            return Err(ValidationError::new(format!(
                "a page has 1-{MAX_PAGE_ITEMS} items"
            )));
        }
        for item in &self.items {
            match item {
                PageItemV1::Fact { id } if !fact_id_ok(id) => {
                    return Err(ValidationError::new("page fact reference is malformed"));
                }
                PageItemV1::Prose { text } => {
                    validate_text(text, MAX_PROSE_CHARS, "prose", true)?;
                }
                PageItemV1::Fact { .. } => {}
            }
        }
        Ok(())
    }
}

impl RelationV1 {
    fn validate_kind(&self) -> Result<(), ValidationError> {
        if !RELATION_TYPES.contains(&self.kind.as_str()) {
            return Err(ValidationError::new(format!(
                "relation type {:?} is not supported",
                self.kind
            )));
        }
        if self.from == self.to {
            return Err(ValidationError::new("relation points a fact to itself"));
        }
        Ok(())
    }
}

impl ResearchContextDocumentV1 {
    /// Structural validation, identical in effect on engine and service.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.format != RESEARCH_CONTEXT_FORMAT {
            return Err(ValidationError::new(format!(
                "format must be {RESEARCH_CONTEXT_FORMAT}"
            )));
        }
        if self.facts.len() > MAX_FACTS || self.relations.len() > MAX_RELATIONS {
            return Err(ValidationError::new(format!(
                "at most {MAX_FACTS} facts and {MAX_RELATIONS} relations"
            )));
        }
        unique_texts("open_gaps", &self.open_gaps, 20, 500)?;
        let mut ids = HashSet::new();
        let mut references = 0usize;
        for fact in &self.facts {
            fact.validate()?;
            if !ids.insert(fact.id.as_str()) {
                return Err(ValidationError::new(format!(
                    "fact id {:?} is not unique",
                    fact.id
                )));
            }
            references += fact.applies_to.len() + fact.sources.len();
        }
        if references > MAX_REFERENCES {
            return Err(ValidationError::new(format!(
                "too many applicability and source references (max {MAX_REFERENCES})"
            )));
        }
        let mut edges = HashSet::new();
        for relation in &self.relations {
            relation.validate_kind()?;
            if !ids.contains(relation.from.as_str()) || !ids.contains(relation.to.as_str()) {
                return Err(ValidationError::new("relation points to a missing fact"));
            }
            if !edges.insert(relation) {
                return Err(ValidationError::new("relation repeats"));
            }
        }
        if self.pages.len() > MAX_PAGES {
            return Err(ValidationError::new(format!("at most {MAX_PAGES} pages")));
        }
        let mut page_ids = HashSet::new();
        for page in &self.pages {
            page.validate()?;
            if !page_ids.insert(page.id.as_str()) {
                return Err(ValidationError::new("page id is not unique"));
            }
            for item in &page.items {
                if let PageItemV1::Fact { id } = item
                    && !ids.contains(id.as_str())
                {
                    return Err(ValidationError::new("a page shows a missing fact"));
                }
            }
        }
        let chars = serde_json::to_string(self)
            .map_err(|error| ValidationError::new(format!("document: {error}")))?
            .chars()
            .count();
        if chars > MAX_DOCUMENT_CHARS {
            return Err(ValidationError::new(format!(
                "the document exceeds {MAX_DOCUMENT_CHARS} characters"
            )));
        }
        Ok(())
    }
}

impl PatchOpV1 {
    /// Checks of one op on its own; applying it is the engine's/service's job.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.check(false)
    }

    /// 1.9: an op Hive wrote ([`RevisionDiffV1`]); its facts carry their
    /// full `from_contributions` in the target revision.
    pub fn validate_authored(&self) -> Result<(), ValidationError> {
        self.check(true)
    }

    fn check(&self, authored: bool) -> Result<(), ValidationError> {
        let fact_ok = |fact: &FactV1| {
            if authored {
                fact.validate()
            } else {
                validate_patch_fact(fact)
            }
        };
        match self {
            Self::AddFact { fact } => fact_ok(fact),
            Self::UpdateFact { id, fact } => {
                if id != &fact.id {
                    return Err(ValidationError::new("update_fact id must match fact.id"));
                }
                fact_ok(fact)
            }
            Self::RetractFact { id } => {
                if fact_id_ok(id) {
                    Ok(())
                } else {
                    Err(ValidationError::new("retract_fact id is malformed"))
                }
            }
            Self::AddRelation { relation } | Self::RemoveRelation { relation } => {
                relation.validate_kind()
            }
            Self::AddGap { text } | Self::CloseGap { text } => {
                validate_text(text, 500, "gap", false)
            }
            Self::AddPage { page } => page.validate(),
            Self::UpdatePage { id, page } => {
                if id != &page.id {
                    return Err(ValidationError::new("update_page id must match page.id"));
                }
                page.validate()
            }
            Self::RemovePage { id } => {
                if fact_id_ok(id) {
                    Ok(())
                } else {
                    Err(ValidationError::new("remove_page id is malformed"))
                }
            }
        }
    }
}

fn validate_patch_fact(fact: &FactV1) -> Result<(), ValidationError> {
    if !fact.from_contributions.is_empty() {
        return Err(ValidationError::new(
            "from_contributions is assigned by Hive and never sent in a patch",
        ));
    }
    fact.validate()
}

/// 1.9: whether a revision is delivered to agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionState {
    #[default]
    Released,
    /// Auto-merged and not reviewed yet: never delivered to agents.
    Quarantined,
    /// Undone by a later revision that `rolls_back` to an earlier one.
    RolledBack,
    #[serde(other)]
    Unknown,
}

impl RevisionState {
    fn is_released(&self) -> bool {
        *self == Self::Released
    }
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// One revision of a vault's knowledge; exactly one of `document`/`sealed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeRevisionV1 {
    pub schema: String,
    pub vault_id: String,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<ResearchContextDocumentV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<SealedBlobV1>,
    pub updated_at: UtcTimestamp,
    /// 1.9: `released` unless a reviewer reads a quarantined revision.
    #[serde(default, skip_serializing_if = "RevisionState::is_released")]
    pub state: RevisionState,
    /// 1.9: quarantined revisions after this delivered head.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub quarantined_after: u64,
    /// 1.9: facts whose state is not `current` ([`super::facts`]); absent
    /// for sealed vaults, whose states stay on the engines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fact_states: Vec<super::facts::FactStateV1>,
    /// 1.9 (ctxpkg ids): the document tree and the revision commit
    /// (`hive::ctxpkg`), computed by Hive over the stored document; clear
    /// vaults only. An engine whose own ids differ treats the revision as
    /// altered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctxpkg_tree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctxpkg_commit: Option<String>,
}

impl KnowledgeRevisionV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, KNOWLEDGE_REVISION_SCHEMA)?;
        super::validate_vault_id(&self.vault_id)?;
        if self.state != RevisionState::Released && self.quarantined_after != 0 {
            return Err(ValidationError::new(
                "quarantined_after counts after a delivered head only",
            ));
        }
        match (&self.document, &self.sealed) {
            (Some(document), None) => {
                document.validate()?;
                let ids: HashSet<&str> = document.facts.iter().map(|f| f.id.as_str()).collect();
                for (i, state) in self.fact_states.iter().enumerate() {
                    state.validate()?;
                    if !ids.contains(state.fact.as_str()) {
                        return Err(ValidationError::new("a fact state names a missing fact"));
                    }
                    if i > 0 && self.fact_states[i - 1].fact >= state.fact {
                        return Err(ValidationError::new(
                            "fact_states must be sorted by fact without repeats",
                        ));
                    }
                }
                Ok(())
            }
            (None, Some(sealed)) => {
                if !self.fact_states.is_empty()
                    || self.ctxpkg_tree.is_some()
                    || self.ctxpkg_commit.is_some()
                {
                    return Err(ValidationError::new(
                        "a sealed revision carries no fact states and no ctxpkg ids",
                    ));
                }
                sealed.validate()
            }
            _ => Err(ValidationError::new(
                "a revision carries exactly one of document or sealed",
            )),
        }
    }
}

/// One entry of the revision history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionSummaryV1 {
    pub revision: u64,
    pub created_at: UtcTimestamp,
    /// Proposal number the revision merged, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<u64>,
    /// `sha256` hex of the canonical document (or ciphertext, when sealed).
    pub digest: String,
    /// 1.9.
    #[serde(default, skip_serializing_if = "RevisionState::is_released")]
    pub state: RevisionState,
    /// 1.9: the earlier revision whose document this one restores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rolls_back: Option<u64>,
    /// 1.9 (ctxpkg ids): the document tree and the revision commit
    /// (`hive::ctxpkg`), computed by Hive over the stored document; clear
    /// vaults only. An engine whose own ids differ treats the revision as
    /// altered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctxpkg_tree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctxpkg_commit: Option<String>,
}

/// Response of `GET /v1/vaults/{vault_id}/knowledge/revisions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionListV1 {
    pub schema: String,
    pub vault_id: String,
    pub revisions: Vec<RevisionSummaryV1>,
    /// Pass as `after` for the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after: Option<u64>,
}

impl RevisionListV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, REVISION_LIST_SCHEMA)?;
        super::validate_vault_id(&self.vault_id)?;
        if self.revisions.len() > MAX_REVISIONS_PAGE {
            return Err(ValidationError::new("revision page is too long"));
        }
        let mut previous = None;
        for summary in &self.revisions {
            validate_hex64(&summary.digest, "digest")?;
            if summary
                .rolls_back
                .is_some_and(|target| target >= summary.revision)
            {
                return Err(ValidationError::new(
                    "a rollback restores an earlier revision",
                ));
            }
            if previous.is_some_and(|previous| summary.revision <= previous) {
                return Err(ValidationError::new("revisions must ascend"));
            }
            previous = Some(summary.revision);
        }
        Ok(())
    }
}

/// Review state of a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    Draft,
    Open,
    Merged,
    Rejected,
    /// 1.6: its author withdrew it.
    Withdrawn,
    /// 1.9 (section 7): a sealed proposal that reached its auto-merge
    /// threshold; the next engine with `knowledge.merge` merges it.
    Ready,
    #[serde(other)]
    Unknown,
}

/// Patch of a proposal against `base_revision`: `ops` in the clear, or
/// `sealed` (the encrypted `ops` array) for sealed vaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPatchV1 {
    pub base_revision: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ops: Vec<PatchOpV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<SealedBlobV1>,
}

/// What the engine's screening removed before the draft was queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineScreeningV1 {
    pub scanner_version: u32,
    pub secrets_removed: u32,
    pub pii_removed: u32,
    pub items_dropped: u32,
}

/// `POST /v1/vaults/{vault_id}/proposals`: a draft from one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftProposalV1 {
    pub schema: String,
    /// Always `draft`; review in Hive moves it on.
    pub state: ProposalState,
    /// One line for reviewers; required in the clear. 1.5: absent for
    /// sealed vaults, where nothing readable leaves the engine (the real
    /// summary is inside the sealed patch; Hive shows a placeholder).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Reviewer notes; absent for sealed vaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Source references; empty for sealed vaults.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    pub context_patch: ContextPatchV1,
    /// Opaque, per-session id (no host or user data).
    pub session_ref: String,
    pub engine_screening: EngineScreeningV1,
    /// 1.9: where the draft comes from; absent means `learned`. Always in
    /// the clear, sealed vaults included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<DraftOrigin>,
}

/// 1.9: the origin of a draft. Every origin passes the same engine
/// admission filter before upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftOrigin {
    /// Learned from sessions (the default).
    Learned,
    /// Imported from the repo's docs or git history. Its sources are
    /// `import:docs:<path>#<slug>` or `import:git:<sha>`; in a sealed
    /// vault they travel only inside the sealed patch, because paths and
    /// commit hashes are metadata too.
    Import,
    #[serde(other)]
    Unknown,
}

/// 1.9: an import source, `import:docs:<path>#<slug>` (repo-relative path,
/// slug of `[a-z0-9-]`) or `import:git:<sha>` (40 or 64 lowercase hex).
pub fn import_source_ok(source: &str) -> bool {
    if let Some(sha) = source.strip_prefix("import:git:") {
        return matches!(sha.len(), 40 | 64)
            && sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    }
    let Some(rest) = source.strip_prefix("import:docs:") else {
        return false;
    };
    let Some((path, slug)) = rest.rsplit_once('#') else {
        return false;
    };
    !slug.is_empty()
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

impl DraftProposalV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, DRAFT_PROPOSAL_SCHEMA)?;
        if self.state != ProposalState::Draft {
            return Err(ValidationError::new("engine proposals are drafts"));
        }
        if let Some(summary) = &self.summary {
            validate_text(summary, 200, "summary", false)?;
        }
        if let Some(body) = &self.body {
            validate_text(body, 20_000, "body", true)?;
        }
        unique_texts("sources", &self.sources, 16, 2000)?;
        match self.origin {
            Some(DraftOrigin::Import) => {
                if self.context_patch.sealed.is_none() && self.sources.is_empty() {
                    return Err(ValidationError::new(
                        "an import in the clear names its sources",
                    ));
                }
                if !self.sources.iter().all(|s| import_source_ok(s)) {
                    return Err(ValidationError::new(
                        "import sources are import:docs:<path>#<slug> or import:git:<sha>",
                    ));
                }
            }
            Some(DraftOrigin::Unknown) => {
                return Err(ValidationError::new("unknown draft origin"));
            }
            Some(DraftOrigin::Learned) | None => {}
        }
        validate_id(&self.session_ref, "session_ref")?;
        let patch = &self.context_patch;
        match (&patch.sealed, patch.ops.is_empty()) {
            (None, false) => {
                if self.summary.is_none() {
                    return Err(ValidationError::new("a draft in the clear needs a summary"));
                }
                if patch.ops.len() > MAX_PATCH_OPS {
                    return Err(ValidationError::new(format!(
                        "a patch carries 1-{MAX_PATCH_OPS} ops"
                    )));
                }
                patch.ops.iter().try_for_each(PatchOpV1::validate)
            }
            (Some(sealed), true) => {
                if self.summary.is_some() || self.body.is_some() || !self.sources.is_empty() {
                    return Err(ValidationError::new("sealed drafts carry no plaintext"));
                }
                sealed.validate()
            }
            _ => Err(ValidationError::new("a patch carries either ops or sealed")),
        }
    }
}

/// Response of `POST /v1/vaults/{vault_id}/proposals`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalAckV1 {
    pub schema: String,
    pub number: u64,
    pub state: ProposalState,
}

/// One proposal in a listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalSummaryV1 {
    pub number: u64,
    pub state: ProposalState,
    pub summary: String,
    pub base_revision: u64,
    pub created_at: UtcTimestamp,
    /// Revision the merge produced, when merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_revision: Option<u64>,
    /// Whether this instance sent it.
    pub mine: bool,
}

/// Response of `GET /v1/vaults/{vault_id}/proposals`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalListV1 {
    pub schema: String,
    pub vault_id: String,
    pub proposals: Vec<ProposalSummaryV1>,
}

/// `PUT /v1/vaults/{vault_id}/knowledge`: compare-and-set a whole revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgePutRequestV1 {
    pub schema: String,
    pub expected_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<ResearchContextDocumentV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<SealedBlobV1>,
    /// 1.6: proposals this revision merges (draft or open ones, ≤ 50,
    /// unique); Hive marks them merged at the new revision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_proposals: Vec<u64>,
    /// 1.9: this revision restores the document of that earlier revision
    /// (a rollback); it merges no proposals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rolls_back: Option<u64>,
}

/// Response of `GET /v1/vaults/{vault_id}/proposals/{number}` (1.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalV1 {
    pub schema: String,
    pub number: u64,
    pub state: ProposalState,
    pub created_at: UtcTimestamp,
    /// Whether this instance sent it.
    pub mine: bool,
    /// Revision the merge produced, when merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_revision: Option<u64>,
    /// The draft exactly as its engine sent it.
    pub draft: DraftProposalV1,
    /// 1.9 (section 7): who merged or rejected it, a person or the
    /// auto-merge threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<super::tasks::DecidedByV1>,
}

impl ProposalV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, PROPOSAL_VIEW_SCHEMA)?;
        self.draft.validate()
    }
}

impl KnowledgePutRequestV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, KNOWLEDGE_PUT_REQUEST_SCHEMA)?;
        if self.merged_proposals.len() > MAX_MERGED_PROPOSALS
            || self.merged_proposals.iter().collect::<HashSet<_>>().len()
                != self.merged_proposals.len()
        {
            return Err(ValidationError::new(format!(
                "merged_proposals lists at most {MAX_MERGED_PROPOSALS} distinct proposals"
            )));
        }
        if let Some(target) = self.rolls_back {
            if target >= self.expected_revision {
                return Err(ValidationError::new(
                    "a rollback restores an earlier revision",
                ));
            }
            if !self.merged_proposals.is_empty() {
                return Err(ValidationError::new("a rollback merges no proposals"));
            }
        }
        match (&self.document, &self.sealed) {
            (Some(document), None) => document.validate(),
            (None, Some(sealed)) => sealed.validate(),
            _ => Err(ValidationError::new(
                "a put carries exactly one of document or sealed",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeEventKind {
    /// A person merged, or released a quarantined revision.
    Released,
    /// A threshold merged a revision into quarantine (reviewers only act).
    Quarantined,
    /// A rollback revision undid `revision`.
    RolledBack,
    #[serde(other)]
    Unknown,
}

/// 1.9 `knowledge.event` (service → client), on the vault channel: the
/// engine fetches the delivered head (or the quarantined revision, to
/// review it) over HTTP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeEventV1 {
    pub channel: String,
    pub seq: u64,
    pub revision: u64,
    pub event: KnowledgeEventKind,
    /// With `quarantined`: when the quarantine ends in a task (24 hours).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_by: Option<UtcTimestamp>,
    pub server_time: UtcTimestamp,
}

pub const REVISION_DIFF_SCHEMA: &str = "leanctx.hive.revision-diff/v1";

/// 1.9: `GET /v1/vaults/{vault_id}/knowledge/diff?from=&to=`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionDiffV1 {
    pub schema: String,
    pub vault_id: String,
    pub from: u64,
    pub to: u64,
    /// Applied in order to revision `from`, they give revision `to` exactly
    /// (its digest), credits included: unlike patches, these ops carry
    /// `from_contributions`, the fact's full list in `to`; applying
    /// `update_fact` keeps earlier credits and appends the missing ones.
    pub ops: Vec<PatchOpV1>,
}

impl RevisionDiffV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, REVISION_DIFF_SCHEMA)?;
        super::validate_vault_id(&self.vault_id)?;
        if self.to <= self.from {
            return Err(ValidationError::new("a diff goes forward"));
        }
        self.ops.iter().try_for_each(PatchOpV1::validate_authored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(id: &str) -> FactV1 {
        FactV1 {
            id: id.into(),
            statement: "Hive never blocks a tool call.".into(),
            kind: "decision".into(),
            applies_to: vec![],
            sources: vec![],
            from_contributions: vec![],
            anchors: vec![],
        }
    }

    #[test]
    fn documents_reject_dangling_and_repeated_relations() {
        let mut document = ResearchContextDocumentV1 {
            facts: vec![fact("a"), fact("b")],
            ..Default::default()
        };
        let edge = RelationV1 {
            from: "a".into(),
            to: "b".into(),
            kind: "supports".into(),
        };
        document.relations = vec![edge.clone()];
        assert!(document.validate().is_ok());
        document.relations.push(edge);
        assert!(document.validate().is_err());
        document.relations = vec![RelationV1 {
            from: "a".into(),
            to: "missing".into(),
            kind: "supports".into(),
        }];
        assert!(document.validate().is_err());
    }

    #[test]
    fn patches_never_carry_credits() {
        let mut credited = fact("a");
        credited.from_contributions = vec!["p1".into()];
        assert!(PatchOpV1::AddFact { fact: credited }.validate().is_err());
        assert!(PatchOpV1::AddFact { fact: fact("a") }.validate().is_ok());
    }
}
