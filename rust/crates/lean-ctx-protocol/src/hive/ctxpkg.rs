// SPDX-License-Identifier: Apache-2.0

//! The ctxpkg object mapping (C0 1.9, section 7; plan O1).
//!
//! Knowledge maps onto content-addressed objects like a git repository:
//! a fact is a **blob**, a page and a revision's whole document are
//! **trees**, a revision is a **commit**, a proposal is a pull request,
//! Hive is a remote and the engine a client. The open ctxpkg spec (O1)
//! builds on these ids; every implementation reproduces
//! `ctxpkg.vectors.json`.
//!
//! Every id is `sha256:` + hex of `canonical_json` of a typed object:
//! - blob: `{"type":"fact","fact":<FactV1>}` (credits included);
//! - page tree: `{"type":"page","id","title","items":[{"kind":"fact","blob":<id>}|{"kind":"prose","text"}]}`;
//! - document tree: `{"type":"tree","format","facts":[blob ids sorted],
//!   "relations":[…sorted by (from,to,type)],"open_gaps":[…sorted],"pages":[page tree ids sorted]}`;
//! - commit: `{"type":"commit","revision","tree","parent":<commit id>|null,
//!   "merged_proposals":[…ascending],"rolls_back":<n>|null}`.
//!
//! Order inside the document does not change the tree: two documents with
//! the same facts, relations, gaps and pages share it.
//!
//! Export: an engine renders `AGENTS.md` from the released document
//! locally; nothing about it is on the wire. Import (`origin: import`,
//! section 5 addendum) names its sources with [`import_slug`] headings.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::canonical_json;
use super::knowledge::{FactV1, PageItemV1, PageV1, ResearchContextDocumentV1};
use crate::ValidationError;

fn object_id(value: &Value) -> Result<String, ValidationError> {
    let digest = Sha256::digest(canonical_json(value)?);
    let mut out = String::with_capacity(71);
    out.push_str("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    Ok(out)
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, ValidationError> {
    serde_json::to_value(value).map_err(|error| ValidationError::new(format!("ctxpkg: {error}")))
}

pub fn fact_blob_id(fact: &FactV1) -> Result<String, ValidationError> {
    object_id(&json!({"type": "fact", "fact": to_value(fact)?}))
}

pub fn page_tree_id(
    page: &PageV1,
    document: &ResearchContextDocumentV1,
) -> Result<String, ValidationError> {
    let mut items = Vec::with_capacity(page.items.len());
    for item in &page.items {
        items.push(match item {
            PageItemV1::Fact { id } => {
                let fact = document
                    .facts
                    .iter()
                    .find(|fact| &fact.id == id)
                    .ok_or_else(|| ValidationError::new("a page shows a missing fact"))?;
                json!({"kind": "fact", "blob": fact_blob_id(fact)?})
            }
            PageItemV1::Prose { text } => json!({"kind": "prose", "text": text}),
        });
    }
    object_id(&json!({"type": "page", "id": page.id, "title": page.title, "items": items}))
}

pub fn document_tree_id(document: &ResearchContextDocumentV1) -> Result<String, ValidationError> {
    let mut facts = document
        .facts
        .iter()
        .map(fact_blob_id)
        .collect::<Result<Vec<_>, _>>()?;
    facts.sort();
    let mut relations: Vec<_> = document
        .relations
        .iter()
        .map(|r| (r.from.as_str(), r.to.as_str(), r.kind.as_str()))
        .collect();
    relations.sort_unstable();
    let relations: Vec<Value> = relations
        .into_iter()
        .map(|(from, to, kind)| json!({"from": from, "to": to, "type": kind}))
        .collect();
    let mut gaps = document.open_gaps.clone();
    gaps.sort();
    let mut pages = document
        .pages
        .iter()
        .map(|page| page_tree_id(page, document))
        .collect::<Result<Vec<_>, _>>()?;
    pages.sort();
    object_id(&json!({
        "type": "tree", "format": document.format, "facts": facts,
        "relations": relations, "open_gaps": gaps, "pages": pages,
    }))
}

/// The commit of `revision`; `parent` is the previous revision's commit
/// (none for revision 1).
pub fn revision_commit_id(
    revision: u64,
    tree: &str,
    parent: Option<&str>,
    merged_proposals: &[u64],
    rolls_back: Option<u64>,
) -> Result<String, ValidationError> {
    let mut merged = merged_proposals.to_vec();
    merged.sort_unstable();
    object_id(&json!({
        "type": "commit", "revision": revision, "tree": tree, "parent": parent,
        "merged_proposals": merged, "rolls_back": rolls_back,
    }))
}

/// Slug of a Markdown heading for import sources and page ids: lowercase
/// ASCII letters and digits kept, every other run of characters one `-`,
/// no leading or trailing `-`, at most 64 characters (cut at a `-` when
/// possible); `section` when nothing is left.
pub fn import_slug(heading: &str) -> String {
    let mut slug = String::new();
    for ch in heading.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    if slug.len() > 64 {
        slug.truncate(64);
        if let Some(cut) = slug.rfind('-').filter(|cut| *cut > 0) {
            slug.truncate(cut);
        }
    }
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() {
        "section".into()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_stable_and_bounded() {
        assert_eq!(
            import_slug("## Retry budget (payments)"),
            "retry-budget-payments"
        );
        assert_eq!(import_slug("Ünïcode — only"), "n-code-only");
        assert_eq!(import_slug("!!!"), "section");
        assert!(import_slug(&"word ".repeat(30)).len() <= 64);
        assert!(!import_slug(&"word ".repeat(30)).ends_with('-'));
    }

    #[test]
    fn document_order_does_not_change_the_tree() {
        let fact = |id: &str| FactV1 {
            id: id.into(),
            statement: format!("statement {id}"),
            kind: "decision".into(),
            applies_to: vec![],
            sources: vec![],
            from_contributions: vec![],
            anchors: vec![],
        };
        let a = ResearchContextDocumentV1 {
            facts: vec![fact("a"), fact("b")],
            ..Default::default()
        };
        let b = ResearchContextDocumentV1 {
            facts: vec![fact("b"), fact("a")],
            ..Default::default()
        };
        assert_eq!(document_tree_id(&a).unwrap(), document_tree_id(&b).unwrap());
    }
}
