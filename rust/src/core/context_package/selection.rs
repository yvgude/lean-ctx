// SPDX-License-Identifier: Apache-2.0

//! V01 — what `pack create` and `ctx_pack create` pack: a selection of
//! layers and facts, and a preview that shows exactly that without writing.
//!
//! One build serves both: the preview renders the package the create would
//! store, so the two cannot drift. Facts merged from a received package go
//! in only when that package grants redistribution (V02); private session
//! text never goes in (the session layer carries the journal only, and the
//! admission of every install refuses what the policy withholds).

use std::collections::HashMap;
use std::fmt::Write as _;

use super::builder::{ContentScope, PackageBuilder};
use super::content::PackageContent;
use super::manifest::{PackageLayer, PackageManifest};
use crate::core::knowledge::KnowledgeFact;

/// Layers built when the caller names none.
pub(crate) const DEFAULT_LAYERS: &[&str] = &["knowledge", "graph", "session", "gotchas"];
/// Layers a caller may name.
pub(crate) const SELECTABLE_LAYERS: &[&str] =
    &["knowledge", "graph", "session", "patterns", "gotchas"];

/// The caller's choice. Empty lists mean "no restriction"; facts pass when
/// they match every category/key restriction given, or are named in `facts`.
#[derive(Debug, Clone, Default)]
pub(crate) struct PackSelection {
    pub layers: Vec<String>,
    pub categories: Vec<String>,
    pub keys: Vec<String>,
    /// Explicit facts as `category/key`.
    pub facts: Vec<String>,
}

impl PackSelection {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if let Some(unknown) = self
            .layers
            .iter()
            .find(|layer| !SELECTABLE_LAYERS.contains(&layer.as_str()))
        {
            return Err(format!(
                "unknown layer `{unknown}` (choose from {})",
                SELECTABLE_LAYERS.join(", ")
            ));
        }
        if let Some(bad) = self.facts.iter().find(|fact| {
            fact.split_once('/')
                .is_none_or(|(c, k)| c.is_empty() || k.is_empty())
        }) {
            return Err(format!("fact `{bad}` must be written as category/key"));
        }
        Ok(())
    }

    pub(crate) fn wants(&self, layer: &str) -> bool {
        if self.layers.is_empty() {
            DEFAULT_LAYERS.contains(&layer)
        } else {
            self.layers.iter().any(|l| l == layer)
        }
    }

    fn filters_facts(&self) -> bool {
        !(self.categories.is_empty() && self.keys.is_empty() && self.facts.is_empty())
    }

    fn admits(&self, fact: &KnowledgeFact) -> bool {
        if !self.filters_facts() {
            return true;
        }
        let named = self
            .facts
            .iter()
            .any(|f| f.split_once('/') == Some((fact.category.as_str(), fact.key.as_str())));
        let by_filter = !(self.categories.is_empty() && self.keys.is_empty())
            && (self.categories.is_empty() || self.categories.contains(&fact.category))
            && (self.keys.is_empty() || self.keys.contains(&fact.key));
        named || by_filter
    }
}

/// Everything about the package besides its content.
#[derive(Debug, Clone, Default)]
pub(crate) struct PackRequest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,
    pub tags: Vec<String>,
    pub level: u32,
    pub scope: Option<String>,
    pub private: bool,
    pub redistributable: bool,
    pub selection: PackSelection,
}

/// A fact the selection left out for a reason the caller must see.
#[derive(Debug, Clone)]
pub(crate) struct Withheld {
    pub fact: String,
    pub reason: String,
}

/// The package a create would store, plus what it left out.
pub(crate) struct PackPlan {
    pub manifest: PackageManifest,
    pub content: PackageContent,
    /// Facts merged from a package that grants no redistribution.
    pub withheld: Vec<Withheld>,
    /// Facts the selection did not take (count only).
    pub not_selected: usize,
    /// `--fact` names that match no fact.
    pub unmatched: Vec<String>,
}

/// Build the selected package in memory. Nothing is written.
pub(crate) fn plan(
    project_root: &str,
    request: &PackRequest,
    session: Option<&crate::core::session::SessionState>,
) -> Result<PackPlan, String> {
    request.selection.validate()?;
    let selection = request.selection.clone();

    // V02: facts merged from a package keep its rights.
    let facts = crate::core::knowledge::KnowledgeSnapshot::collect(project_root).facts;
    let registry = super::LocalRegistry::open().ok();
    let mut grants: HashMap<String, bool> = HashMap::new();
    for source in facts.iter().filter_map(|f| f.imported_from.as_deref()) {
        if let Some((name, version)) = super::rights::package_source(source) {
            grants.entry(source.to_string()).or_insert_with(|| {
                registry
                    .as_ref()
                    .is_some_and(|r| super::rights::source_grants(r, name, version))
            });
        }
    }
    let repackable = move |fact: &KnowledgeFact| {
        fact.imported_from
            .as_deref()
            .and_then(|source| grants.get(source))
            .is_none_or(|granted| *granted)
    };

    let wants_knowledge = selection.wants("knowledge") || selection.wants("patterns");
    let mut withheld = Vec::new();
    let mut not_selected = 0;
    for fact in &facts {
        if !wants_knowledge || !selection.admits(fact) {
            not_selected += 1;
        } else if !repackable(fact) {
            withheld.push(Withheld {
                fact: format!("{}/{}", fact.category, fact.key),
                reason: format!(
                    "merged from {} — that package grants no redistribution",
                    fact.imported_from.as_deref().unwrap_or_default()
                ),
            });
        }
    }
    let unmatched = selection
        .facts
        .iter()
        .filter(|named| {
            !facts
                .iter()
                .any(|f| named.split_once('/') == Some((f.category.as_str(), f.key.as_str())))
        })
        .cloned()
        .collect();

    let scope = ContentScope {
        knowledge_extras: !selection.filters_facts(),
        gotchas: selection.wants("gotchas"),
        code: selection.wants("graph"),
        facts: Some(Box::new({
            let selection = selection.clone();
            move |fact: &KnowledgeFact| {
                wants_knowledge && selection.admits(fact) && repackable(fact)
            }
        })),
    };

    let mut builder = PackageBuilder::new(&request.name, &request.version)
        .description(&request.description)
        .tags(request.tags.clone())
        .level(request.level)
        .content_scope(scope);
    if let Some(author) = &request.author {
        builder = builder.author(author);
    }
    if let Some(scope) = &request.scope {
        builder = builder.scope(scope);
    }
    if request.private {
        builder = builder.private();
    }
    if request.redistributable {
        builder = builder.redistributable();
    }
    // V02: no project-root hash in the manifest — it is an unsalted hash of
    // a local path, and a package leaves this machine.
    if request.level >= 2 {
        builder.build_context_graph(project_root);
    }
    if wants_knowledge {
        builder = builder.add_knowledge_from_project(project_root);
    }
    if selection.wants("patterns") {
        builder = builder.add_patterns_from_project(project_root);
    }
    if selection.wants("graph") {
        builder = builder.add_graph_from_project(project_root);
    }
    if selection.wants("session")
        && let Some(session) = session
    {
        builder = builder.add_session(session);
    }
    if selection.wants("gotchas") {
        builder = builder.add_gotchas_from_project(project_root);
    }
    let (manifest, content) = builder.build()?;
    Ok(PackPlan {
        manifest,
        content,
        withheld,
        not_selected,
        unmatched,
    })
}

fn short(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or_default();
    if line.chars().count() <= max && line.len() == text.len() {
        return line.to_string();
    }
    let cut: String = line.chars().take(max).collect();
    format!("{cut}…")
}

fn source_of(fact: &KnowledgeFact) -> String {
    use crate::core::knowledge::FactOrigin;
    let origin = match &fact.origin {
        FactOrigin::Provider(p) => format!("provider {}/{}", p.provider, p.resource),
        FactOrigin::Derived(_) => "derived".into(),
        FactOrigin::Local => "written locally".into(),
        FactOrigin::Unverified => "no recorded source".into(),
    };
    match &fact.imported_from {
        Some(from) => format!("{origin}, merged from {from}"),
        None => origin,
    }
}

fn validity_of(fact: &KnowledgeFact) -> String {
    match (fact.valid_from, fact.valid_until) {
        (_, Some(until)) => format!(
            "no longer current, valid until {}",
            until.format("%Y-%m-%d")
        ),
        (Some(from), None) => format!("current since {}", from.format("%Y-%m-%d")),
        (None, None) => "current".into(),
    }
}

impl PackPlan {
    /// What the package holds: counts, fact status, what was left out. The
    /// preview also lists every item (title, status, source, validity) and
    /// whether the install's admission would take the package as built.
    pub(crate) fn render(&self, preview: bool) -> String {
        let m = &self.manifest;
        let c = &self.content;
        let mut out = String::new();
        if preview {
            let _ = writeln!(
                out,
                "Preview of {}@{} — nothing was written",
                m.name, m.version
            );
        }
        let layers: Vec<_> = m.layers.iter().map(PackageLayer::as_str).collect();
        let _ = writeln!(out, "  Layers: {}", layers.join(", "));
        if let Some(knowledge) = &c.knowledge {
            let verified = knowledge
                .facts
                .iter()
                .filter(|f| f.status() == crate::core::knowledge::FactStatus::Verified)
                .count();
            let _ = writeln!(
                out,
                "  Knowledge facts: {} ({verified} verified, {} assumption)",
                knowledge.facts.len(),
                knowledge.facts.len() - verified
            );
            for fact in knowledge.facts.iter().filter(|_| preview) {
                let _ = writeln!(
                    out,
                    "    - [{}] {}/{}: {} (source: {}; {})",
                    fact.status().as_str(),
                    fact.category,
                    fact.key,
                    short(&fact.value, 80),
                    source_of(fact),
                    validity_of(fact)
                );
            }
            if !knowledge.patterns.is_empty() || !knowledge.insights.is_empty() {
                let _ = writeln!(
                    out,
                    "  Knowledge patterns: {}, insights: {}",
                    knowledge.patterns.len(),
                    knowledge.insights.len()
                );
            }
        }
        if let Some(patterns) = &c.patterns {
            let _ = writeln!(out, "  Patterns: {}", patterns.patterns.len());
            for p in patterns.patterns.iter().filter(|_| preview) {
                let _ = writeln!(
                    out,
                    "    - {}: {}",
                    p.pattern_type,
                    short(&p.description, 80)
                );
            }
        }
        if let Some(session) = &c.session {
            let _ = writeln!(
                out,
                "  Session: task {}; {} finding(s), {} decision(s), {} next step(s), {} file(s)",
                session
                    .task_description
                    .as_deref()
                    .map_or_else(|| "(none)".to_string(), |t| format!("\"{}\"", short(t, 80))),
                session.findings.len(),
                session.decisions.len(),
                session.next_steps.len(),
                session.files_touched.len()
            );
        }
        if let Some(graph) = &c.graph {
            let _ = writeln!(
                out,
                "  Code graph: {} node(s), {} edge(s)",
                graph.nodes.len(),
                graph.edges.len()
            );
        }
        if let Some(graph) = &c.context_graph {
            let _ = writeln!(
                out,
                "  Context graph: {} node(s), {} edge(s)",
                graph.nodes.len(),
                graph.edges.len()
            );
        }
        if let Some(gotchas) = &c.gotchas {
            let _ = writeln!(out, "  Gotchas: {}", gotchas.gotchas.len());
            for g in gotchas.gotchas.iter().filter(|_| preview) {
                let _ = writeln!(out, "    - [{}] {}", g.severity, short(&g.trigger, 80));
            }
        }
        if self.not_selected > 0 {
            let _ = writeln!(out, "  Not selected: {} fact(s)", self.not_selected);
        }
        for w in &self.withheld {
            let _ = writeln!(out, "  Withheld: {} — {}", w.fact, w.reason);
        }
        for name in &self.unmatched {
            let _ = writeln!(out, "  WARNING: fact `{name}` matches no fact");
        }
        let redistribution = if m.redistributable == Some(true) {
            "granted to recipients"
        } else {
            "not granted (recipients cannot pass it on)"
        };
        let _ = writeln!(out, "  Redistribution: {redistribution}");
        let _ = writeln!(out, "  Size: {} bytes", m.integrity.byte_size);
        if preview {
            match super::admission::admit_parts(m, c) {
                Ok(()) => {
                    let _ = writeln!(out, "  Admission: would be stored as shown");
                }
                Err(refusal) => {
                    let _ = writeln!(out, "  Admission: would be refused — {refusal}");
                }
            }
        }
        out
    }
}
