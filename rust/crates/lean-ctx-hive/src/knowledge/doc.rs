// SPDX-License-Identifier: Apache-2.0
//! Operations on the `ctxpkg.research-context.v1` document: applying a
//! proposal patch exactly like Hive does on merge, and rebasing pending
//! draft ops onto a newer revision. The wire types and their structural
//! validation live in `lean_ctx_protocol::hive::knowledge`.

use lean_ctx_protocol::hive::knowledge::{
    FactV1, PageItemV1, PageV1, PatchOpV1, ResearchContextDocumentV1,
};

fn fact<'a>(doc: &'a ResearchContextDocumentV1, id: &str) -> Option<&'a FactV1> {
    doc.facts.iter().find(|f| f.id == id)
}

fn page<'a>(doc: &'a ResearchContextDocumentV1, id: &str) -> Option<&'a PageV1> {
    doc.pages.iter().find(|p| p.id == id)
}

/// Equality of what a person wrote, ignoring credits assigned on merge.
pub fn same_content(a: &FactV1, b: &FactV1) -> bool {
    a.id == b.id
        && a.statement == b.statement
        && a.kind == b.kind
        && a.applies_to == b.applies_to
        && a.sources == b.sources
        && a.anchors == b.anchors
}

/// Apply `ops` in order; any op that does not fit the document is an error.
pub fn apply_patch(
    doc: &ResearchContextDocumentV1,
    ops: &[PatchOpV1],
) -> Result<ResearchContextDocumentV1, String> {
    let mut d = doc.clone();
    for op in ops {
        match op {
            PatchOpV1::AddFact { fact: new } => {
                if fact(&d, &new.id).is_some() {
                    return Err(format!("fact '{}' already exists", new.id));
                }
                d.facts.push(new.clone());
            }
            PatchOpV1::UpdateFact { id, fact: new } => {
                let pos = d
                    .facts
                    .iter()
                    .position(|f| &f.id == id)
                    .ok_or_else(|| format!("fact '{id}' does not exist"))?;
                let mut updated = new.clone();
                for credit in &d.facts[pos].from_contributions {
                    if !updated.from_contributions.contains(credit) {
                        updated.from_contributions.push(credit.clone());
                    }
                }
                d.facts[pos] = updated;
            }
            PatchOpV1::RetractFact { id } => {
                let before = d.facts.len();
                d.facts.retain(|f| &f.id != id);
                if d.facts.len() == before {
                    return Err(format!("fact '{id}' does not exist"));
                }
                d.relations.retain(|r| &r.from != id && &r.to != id);
                for page in &mut d.pages {
                    page.items.retain(
                        |item| !matches!(item, PageItemV1::Fact { id: shown } if shown == id),
                    );
                }
                d.pages.retain(|page| !page.items.is_empty());
            }
            PatchOpV1::AddRelation { relation } => {
                if d.relations.contains(relation) {
                    return Err(format!(
                        "relation {} -{}-> {} already exists",
                        relation.from, relation.kind, relation.to
                    ));
                }
                d.relations.push(relation.clone());
            }
            PatchOpV1::RemoveRelation { relation } => {
                let before = d.relations.len();
                d.relations.retain(|r| r != relation);
                if d.relations.len() == before {
                    return Err(format!(
                        "relation {} -{}-> {} does not exist",
                        relation.from, relation.kind, relation.to
                    ));
                }
            }
            PatchOpV1::AddGap { text } => {
                if !d.open_gaps.contains(text) {
                    d.open_gaps.push(text.clone());
                }
            }
            PatchOpV1::CloseGap { text } => {
                let before = d.open_gaps.len();
                d.open_gaps.retain(|g| g != text);
                if d.open_gaps.len() == before {
                    return Err(format!("open gap '{text}' does not exist"));
                }
            }
            PatchOpV1::AddPage { page: new } => {
                if page(&d, &new.id).is_some() {
                    return Err(format!("page '{}' already exists", new.id));
                }
                d.pages.push(new.clone());
            }
            PatchOpV1::UpdatePage { id, page: new } => {
                let pos = d
                    .pages
                    .iter()
                    .position(|p| &p.id == id)
                    .ok_or_else(|| format!("page '{id}' does not exist"))?;
                d.pages[pos] = new.clone();
            }
            PatchOpV1::RemovePage { id } => {
                let before = d.pages.len();
                d.pages.retain(|p| &p.id != id);
                if d.pages.len() == before {
                    return Err(format!("page '{id}' does not exist"));
                }
            }
        }
    }
    d.validate().map_err(|e| e.to_string())?;
    Ok(d)
}

/// Re-apply draft ops onto a newer base, dropping ops the base already
/// contains (the same fact was merged meanwhile). Ops that clash with the
/// new base are an error; the caller then keeps the draft on its old base.
pub fn rebase_ops(
    base: &ResearchContextDocumentV1,
    ops: &[PatchOpV1],
) -> Result<Vec<PatchOpV1>, String> {
    let mut d = base.clone();
    let mut kept = Vec::new();
    for op in ops {
        let redundant = match op {
            PatchOpV1::AddFact { fact: new } => match fact(&d, &new.id) {
                Some(existing) if same_content(existing, new) => true,
                Some(_) => {
                    return Err(format!(
                        "fact '{}' was added upstream with different content",
                        new.id
                    ));
                }
                None => false,
            },
            PatchOpV1::UpdateFact { id, fact: new } => match fact(&d, id) {
                Some(existing) => same_content(existing, new),
                None => return Err(format!("fact '{id}' was retracted upstream")),
            },
            PatchOpV1::RetractFact { id } => fact(&d, id).is_none(),
            PatchOpV1::AddRelation { relation } => d.relations.contains(relation),
            PatchOpV1::RemoveRelation { relation } => !d.relations.contains(relation),
            PatchOpV1::AddGap { text } => d.open_gaps.contains(text),
            PatchOpV1::CloseGap { text } => !d.open_gaps.contains(text),
            PatchOpV1::AddPage { page: new } => match page(&d, &new.id) {
                Some(existing) if existing == new => true,
                Some(_) => {
                    return Err(format!(
                        "page '{}' was added upstream with different content",
                        new.id
                    ));
                }
                None => false,
            },
            PatchOpV1::UpdatePage { id, page: new } => match page(&d, id) {
                Some(existing) => existing == new,
                None => return Err(format!("page '{id}' was removed upstream")),
            },
            PatchOpV1::RemovePage { id } => page(&d, id).is_none(),
        };
        if redundant {
            continue;
        }
        d = apply_patch(&d, std::slice::from_ref(op))?;
        kept.push(op.clone());
    }
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::hive::knowledge::RelationV1;

    fn fact(id: &str, statement: &str) -> FactV1 {
        FactV1 {
            id: id.into(),
            statement: statement.into(),
            kind: "decision".into(),
            applies_to: vec![],
            sources: vec![],
            from_contributions: vec![],
            anchors: vec![],
        }
    }

    fn add(id: &str, statement: &str) -> PatchOpV1 {
        PatchOpV1::AddFact {
            fact: fact(id, statement),
        }
    }

    #[test]
    fn patches_apply_and_keep_credits_on_update() {
        let mut base = ResearchContextDocumentV1::default();
        let mut credited = fact("a", "one");
        credited.from_contributions = vec!["p1".into()];
        base.facts.push(credited);
        let doc = apply_patch(
            &base,
            &[
                PatchOpV1::UpdateFact {
                    id: "a".into(),
                    fact: fact("a", "one, revised"),
                },
                add("b", "two"),
                PatchOpV1::AddRelation {
                    relation: RelationV1 {
                        from: "b".into(),
                        to: "a".into(),
                        kind: "supports".into(),
                    },
                },
            ],
        )
        .unwrap();
        assert_eq!(doc.facts[0].from_contributions, vec!["p1".to_string()]);
        assert_eq!(doc.relations.len(), 1);
        assert!(apply_patch(&doc, &[add("b", "again")]).is_err());
        let retracted = apply_patch(&doc, &[PatchOpV1::RetractFact { id: "a".into() }]).unwrap();
        assert!(
            retracted.relations.is_empty(),
            "relations of a retracted fact go too"
        );
    }

    #[test]
    fn rebase_drops_merged_ops_and_refuses_clashes() {
        let ops = vec![add("a", "one"), add("b", "two")];
        let mut base = ResearchContextDocumentV1::default();
        base.facts.push(fact("a", "one"));
        assert_eq!(rebase_ops(&base, &ops).unwrap(), vec![add("b", "two")]);
        base.facts[0].statement = "different".into();
        assert!(rebase_ops(&base, &ops).is_err());
    }
}
