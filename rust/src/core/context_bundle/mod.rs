// SPDX-License-Identifier: Apache-2.0
//! Budgeted context bundles for chat products (#1885).
//!
//! `lean-ctx pack --limit 128k` produces one self-contained XML document that
//! fits a chat input box: the files most relevant to the task in full, the
//! next tier as signatures, and everything else only in the directory tree.
//! The limit is a hard cap on the emitted document, measured in characters
//! (what chat UIs count) or `o200k_base` tokens.

pub(crate) mod collect;
pub(crate) mod limit;
pub(crate) mod rank;
pub(crate) mod render;
mod report;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::core::ib::intent::{TaskIntent, classify_text};

pub(crate) use limit::{Unit, parse_limit};
use render::Detail;

pub(crate) const DEFAULT_LIMIT: usize = 128_000;
pub(crate) const DEFAULT_KNOWLEDGE_CATEGORIES: &[&str] = &["decision", "architecture", "solution"];
pub(crate) const DEFAULT_KNOWLEDGE_LIMIT: usize = 10;

/// Share of the limit the directory tree may take before it is collapsed.
const TREE_PERCENT: usize = 15;
/// Share of the limit project knowledge may take before facts are dropped.
const KNOWLEDGE_PERCENT: usize = 20;
/// Largest share of the file budget one file may take in full (the most
/// relevant file is exempt: it is the one the reader needs most).
const MAX_FILE_PERCENT: usize = 40;
/// Withheld paths listed by name in the summary; the rest are counted.
const MAX_LISTED_WITHHELD: usize = 20;

/// What to bundle and how big it may get.
#[derive(Debug, Clone)]
pub(crate) struct BundleOptions {
    pub root: PathBuf,
    /// Directory or file to bundle, relative to `root` or absolute inside it.
    pub scope: Option<PathBuf>,
    pub limit: usize,
    pub unit: Unit,
    /// Task description; `None` or `auto` falls back to the session task.
    pub intent: Option<String>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// Knowledge categories to include (`all` for every category).
    pub knowledge: Option<Vec<String>>,
    pub knowledge_limit: usize,
    /// Also include machine-derived `auto:*` facts.
    pub knowledge_auto: bool,
    /// Withhold files in which secrets are detected.
    pub security_check: bool,
}

impl BundleOptions {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            scope: None,
            limit: DEFAULT_LIMIT,
            unit: Unit::Chars,
            intent: None,
            include: Vec::new(),
            exclude: Vec::new(),
            knowledge: None,
            knowledge_limit: DEFAULT_KNOWLEDGE_LIMIT,
            knowledge_auto: false,
            security_check: true,
        }
    }
}

/// Where a ranked file ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placement {
    Full,
    Signatures,
    /// Not included; the reason is shown in the report.
    Omitted(&'static str),
}

impl Placement {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Signatures => "signatures",
            Self::Omitted(_) => "omitted",
        }
    }
}

/// A ranked file and its placement.
#[derive(Debug, Clone)]
pub(crate) struct PlannedFile {
    pub path: String,
    pub score: f64,
    pub placement: Placement,
    /// Size of the emitted `<file>` element in the bundle's unit (0 when omitted).
    pub size: usize,
}

/// A file that was never a candidate for inclusion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Withheld {
    pub path: String,
    pub reason: String,
}

/// Where the task description came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntentSource {
    Argument,
    Session,
    None,
}

/// A rendered bundle plus everything needed to explain it.
#[derive(Debug, Clone)]
pub(crate) struct Bundle {
    pub xml: String,
    /// Size of `xml` in `unit`.
    pub size: usize,
    pub limit: usize,
    pub unit: Unit,
    pub intent: TaskIntent,
    pub intent_text: String,
    pub intent_source: IntentSource,
    /// Readable files in rank order.
    pub files: Vec<PlannedFile>,
    pub withheld: Vec<Withheld>,
    pub knowledge_facts: usize,
    pub walk_truncated: bool,
}

impl Bundle {
    pub(crate) fn fits(&self) -> bool {
        self.size <= self.limit
    }

    pub(crate) fn count(&self, label: &str) -> usize {
        self.files
            .iter()
            .filter(|f| f.placement.label() == label)
            .count()
    }

    /// Human-readable allocation report.
    pub(crate) fn report(&self) -> String {
        report::render(self)
    }
}

/// Build a bundle. Errors are user-facing (bad path, bad glob).
pub(crate) fn build(opts: &BundleOptions) -> Result<Bundle, String> {
    let root = opts
        .root
        .canonicalize()
        .map_err(|e| format!("project root '{}': {e}", opts.root.display()))?;
    let scope = resolve_scope(&root, opts.scope.as_deref())?;
    let root_str = root.to_string_lossy().to_string();

    let mut collection = collect::collect(&root, &scope, &opts.include, &opts.exclude)?;
    let withheld = screen(&mut collection.candidates, opts.security_check);

    let (intent_text, intent, intent_source) = resolve_intent(opts.intent.as_deref(), &root_str);
    let readable = collection
        .candidates
        .iter()
        .filter(|c| c.content.is_some())
        .count();
    let edges: Vec<(String, String)> = if readable > 1 {
        crate::core::graph_index::load_or_build(&root_str)
            .edges
            .into_iter()
            .map(|e| (e.from, e.to))
            .collect()
    } else {
        Vec::new()
    };
    let changed = changed_files(&root);
    let ranked = rank::rank(&rank::RankInput {
        candidates: &collection.candidates,
        intent_text: &intent_text,
        intent,
        edges: &edges,
        changed: &changed,
    });

    let measure = |text: &str| opts.unit.measure(text);
    let facts = match &opts.knowledge {
        Some(categories) => knowledge_facts(
            &root_str,
            categories,
            opts.knowledge_limit,
            opts.knowledge_auto,
        ),
        None => Vec::new(),
    };
    let knowledge = fit_knowledge(&facts, opts.limit * KNOWLEDGE_PERCENT / 100, &measure);
    let paths: Vec<&str> = collection
        .candidates
        .iter()
        .map(|c| c.path.as_str())
        .collect();
    let tree = render::directory_tree(&paths, opts.limit * TREE_PERCENT / 100, measure);

    let frame = Frame {
        limit: opts.limit,
        unit: opts.unit,
        intent,
        intent_text: &intent_text,
        tree: &tree,
        knowledge: &knowledge.0,
        withheld: &withheld,
    };
    let mut slots: Vec<Slot> = ranked
        .iter()
        .map(|r| {
            let candidate = &collection.candidates[r.index];
            Slot {
                path: candidate.path.clone(),
                content: candidate.content.as_deref().unwrap_or_default(),
                score: r.score,
                placement: Placement::Omitted("over budget"),
                full: None,
                signatures: std::cell::OnceCell::new(),
            }
        })
        .collect();

    let xml = allocate(&frame, &mut slots, &measure);
    let size = measure(&xml);
    Ok(Bundle {
        xml,
        size,
        limit: opts.limit,
        unit: opts.unit,
        intent,
        intent_text,
        intent_source,
        files: slots
            .iter()
            .map(|s| PlannedFile {
                path: s.path.clone(),
                score: s.score,
                placement: s.placement,
                size: s.block().map_or(0, |(_, size)| size),
            })
            .collect(),
        withheld,
        knowledge_facts: knowledge.1,
        walk_truncated: collection.truncated,
    })
}

fn resolve_scope(root: &Path, scope: Option<&Path>) -> Result<PathBuf, String> {
    let Some(scope) = scope else {
        return Ok(root.to_path_buf());
    };
    let joined = if scope.is_absolute() {
        scope.to_path_buf()
    } else {
        root.join(scope)
    };
    let resolved = joined
        .canonicalize()
        .map_err(|e| format!("path '{}': {e}", scope.display()))?;
    if !resolved.starts_with(root) {
        return Err(format!(
            "path '{}' is outside the project root {}",
            scope.display(),
            root.display()
        ));
    }
    Ok(resolved)
}

/// Record unreadable files and withhold files that look like they hold secrets.
fn screen(candidates: &mut [collect::Candidate], security_check: bool) -> Vec<Withheld> {
    let custom = if security_check {
        crate::core::config::Config::load()
            .secret_detection
            .custom_patterns
    } else {
        Vec::new()
    };
    let mut withheld = Vec::new();
    for candidate in candidates.iter_mut() {
        if let Some(reason) = candidate.skipped {
            withheld.push(Withheld {
                path: candidate.path.clone(),
                reason: reason.to_string(),
            });
            continue;
        }
        if !security_check {
            continue;
        }
        let Some(content) = candidate.content.as_deref() else {
            continue;
        };
        let matches = crate::core::secret_detection::detect_secrets_with_custom(content, &custom);
        if matches.is_empty() {
            continue;
        }
        let mut kinds: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for m in &matches {
            kinds.entry(m.pattern_name).or_default().push(m.line_number);
        }
        let detail = kinds
            .iter()
            .map(|(kind, lines)| {
                let lines: Vec<String> = lines.iter().take(3).map(|l| format!("L{l}")).collect();
                format!("{kind} {}", lines.join(","))
            })
            .collect::<Vec<_>>()
            .join("; ");
        candidate.content = None;
        candidate.skipped = Some("possible secret");
        withheld.push(Withheld {
            path: candidate.path.clone(),
            reason: format!("possible secret ({detail})"),
        });
    }
    // Possible secrets first: they are the entries a reader must not miss.
    withheld.sort_by(|a, b| {
        let secret = |w: &Withheld| !w.reason.starts_with("possible secret");
        secret(a).cmp(&secret(b)).then_with(|| a.path.cmp(&b.path))
    });
    withheld
}

fn resolve_intent(arg: Option<&str>, root: &str) -> (String, TaskIntent, IntentSource) {
    if let Some(text) = arg.map(str::trim)
        && !text.is_empty()
        && !text.eq_ignore_ascii_case("auto")
    {
        return (
            text.to_string(),
            classify_text(text).unwrap_or_default(),
            IntentSource::Argument,
        );
    }
    let task = crate::core::session::SessionState::load_latest_for_project_root(root)
        .and_then(|session| session.task)
        .filter(|task| !task.description.trim().is_empty());
    match task {
        Some(task) => {
            let intent = task
                .intent
                .as_deref()
                .and_then(classify_text)
                .or_else(|| classify_text(&task.description))
                .unwrap_or_default();
            (
                task.description.trim().to_string(),
                intent,
                IntentSource::Session,
            )
        }
        None => (String::new(), TaskIntent::Unknown, IntentSource::None),
    }
}

/// Root-relative paths changed in the working tree (tracked and untracked).
fn changed_files(root: &Path) -> HashSet<String> {
    let mut changed = HashSet::new();
    for args in [
        &["diff", "--name-only", "--relative", "HEAD"][..],
        &["ls-files", "--others", "--exclude-standard"][..],
    ] {
        if let Some(out) = crate::core::git_util::git_out(root, args) {
            changed.extend(out.lines().map(str::to_string));
        }
    }
    changed
}

/// Curated, current, public facts in the requested categories, newest first.
/// Categories match singular or plural (`decisions` selects `decision`);
/// machine-derived `auto:*` facts only join when `with_auto` is set.
fn knowledge_facts(
    root: &str,
    categories: &[String],
    limit: usize,
    with_auto: bool,
) -> Vec<String> {
    let Some(knowledge) = crate::core::knowledge::ProjectKnowledge::load(root) else {
        return Vec::new();
    };
    let wanted: Vec<String> = categories.iter().map(|c| c.trim().to_lowercase()).collect();
    let all = wanted.iter().any(|c| c == "all");
    let matches = |category: &str| {
        let category = category.to_lowercase();
        wanted
            .iter()
            .any(|w| *w == category || w.strip_suffix('s') == Some(category.as_str()))
    };
    let mut facts: Vec<&crate::core::knowledge::KnowledgeFact> = knowledge
        .facts
        .iter()
        .filter(|f| f.is_current())
        .filter(|f| f.sensitivity == crate::core::sensitivity::SensitivityLevel::Public)
        .filter(|f| with_auto || !crate::core::knowledge::is_machine_derived(&f.category, &f.key))
        .filter(|f| all || matches(&f.category))
        .filter(|f| crate::core::secret_detection::detect_secrets(&f.value).is_empty())
        .collect();
    facts.sort_by(|a, b| {
        b.last_confirmed
            .cmp(&a.last_confirmed)
            .then_with(|| a.category.cmp(&b.category))
            .then_with(|| a.key.cmp(&b.key))
    });
    facts
        .into_iter()
        .take(limit)
        .map(|f| render::fact_block(&f.category, &f.key, &f.value))
        .collect()
}

/// The `<knowledge>` section and how many facts it holds, trimmed to `max`.
fn fit_knowledge(
    facts: &[String],
    max: usize,
    measure: &impl Fn(&str) -> usize,
) -> (String, usize) {
    let mut kept = facts.len();
    while kept > 0 {
        let section = format!("<knowledge>\n{}</knowledge>\n", facts[..kept].concat());
        if measure(&section) <= max {
            return (section, kept);
        }
        kept -= 1;
    }
    (String::new(), 0)
}

/// Everything in the bundle except the files.
struct Frame<'a> {
    limit: usize,
    unit: Unit,
    intent: TaskIntent,
    intent_text: &'a str,
    tree: &'a str,
    knowledge: &'a str,
    withheld: &'a [Withheld],
}

struct Slot<'a> {
    path: String,
    content: &'a str,
    score: f64,
    placement: Placement,
    /// Rendered block and its size, computed on first use.
    full: Option<(String, usize)>,
    /// Signature block; the inner `None` means the file has no signatures.
    signatures: std::cell::OnceCell<Option<(String, usize)>>,
}

impl Slot<'_> {
    fn full(&mut self, measure: &impl Fn(&str) -> usize) -> usize {
        if self.full.is_none() {
            let block = render::file_block(&self.path, Detail::Full, self.content);
            let size = measure(&block);
            self.full = Some((block, size));
        }
        self.full.as_ref().map_or(0, |(_, size)| *size)
    }

    fn signatures(&self, measure: &impl Fn(&str) -> usize) -> Option<usize> {
        self.signatures
            .get_or_init(|| {
                let ext = Path::new(&self.path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or_default();
                let sigs = crate::core::signatures::extract_signatures(self.content, ext);
                (!sigs.is_empty()).then(|| {
                    let body: Vec<String> = sigs
                        .iter()
                        .map(crate::core::signatures::Signature::to_compact_located)
                        .collect();
                    let block =
                        render::file_block(&self.path, Detail::Signatures, &body.join("\n"));
                    let size = measure(&block);
                    (block, size)
                })
            })
            .as_ref()
            .map(|(_, size)| *size)
    }

    fn block(&self) -> Option<(&str, usize)> {
        let rendered = match self.placement {
            Placement::Full => self.full.as_ref(),
            Placement::Signatures => self.signatures.get().and_then(Option::as_ref),
            Placement::Omitted(_) => None,
        };
        rendered.map(|(block, size)| (block.as_str(), *size))
    }
}

/// Place files greedily by rank, then shrink until the whole document fits.
fn allocate(frame: &Frame<'_>, slots: &mut [Slot<'_>], measure: &impl Fn(&str) -> usize) -> String {
    // Upper bound for the summary: every count at its maximum width.
    let overhead = measure(&assemble(frame, slots, slots.len()));
    let budget = frame.limit.saturating_sub(overhead);
    let per_file_cap = budget * MAX_FILE_PERCENT / 100;
    let mut remaining = budget;
    for (rank, slot) in slots.iter_mut().enumerate() {
        if remaining == 0 {
            break;
        }
        let full = slot.full(measure);
        if full <= remaining && (rank == 0 || full <= per_file_cap) {
            slot.placement = Placement::Full;
            remaining -= full;
            continue;
        }
        match slot.signatures(measure) {
            Some(size) if size <= remaining => {
                slot.placement = Placement::Signatures;
                remaining -= size;
            }
            Some(_) => slot.placement = Placement::Omitted("over budget"),
            None => slot.placement = Placement::Omitted("over budget; no signatures"),
        }
    }

    // Token counts are not strictly additive and the summary estimate is a
    // bound, so measure the real document and shed the lowest-ranked files.
    loop {
        let xml = assemble(frame, slots, 0);
        let size = measure(&xml);
        if size <= frame.limit {
            return xml;
        }
        let mut excess = size - frame.limit;
        let mut changed = false;
        for slot in slots.iter_mut().rev() {
            if excess == 0 {
                break;
            }
            match slot.placement {
                Placement::Full => {
                    let full = slot.full(measure);
                    match slot.signatures(measure) {
                        Some(sig) if sig < full => {
                            slot.placement = Placement::Signatures;
                            excess = excess.saturating_sub(full - sig);
                        }
                        _ => {
                            slot.placement = Placement::Omitted("over budget");
                            excess = excess.saturating_sub(full);
                        }
                    }
                    changed = true;
                }
                Placement::Signatures => {
                    let sig = slot.signatures(measure).unwrap_or(0);
                    slot.placement = Placement::Omitted("over budget");
                    excess = excess.saturating_sub(sig);
                    changed = true;
                }
                Placement::Omitted(_) => {}
            }
        }
        if !changed {
            return xml;
        }
    }
}

/// Render the document. `pad_counts` > 0 renders the summary as if every
/// count were that large, giving a size bound before allocation.
fn assemble(frame: &Frame<'_>, slots: &[Slot<'_>], pad_counts: usize) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<bundle generator=\"lean-ctx\" limit=\"{}\" unit=\"{}\" intent=\"{}\">",
        frame.limit, frame.unit, frame.intent
    );
    if !frame.intent_text.is_empty() {
        let _ = writeln!(out, "<task>{}</task>", render::escape(frame.intent_text));
    }
    out.push_str(&summary(frame, slots, pad_counts));
    out.push_str("<directory_structure>\n");
    out.push_str(frame.tree);
    out.push_str("</directory_structure>\n");
    out.push_str(frame.knowledge);
    out.push_str("<files>\n");
    if pad_counts == 0 {
        for slot in slots {
            if let Some((block, _)) = slot.block() {
                out.push_str(block);
            }
        }
    }
    out.push_str("</files>\n</bundle>\n");
    out
}

fn summary(frame: &Frame<'_>, slots: &[Slot<'_>], pad_counts: usize) -> String {
    let count = |label: &str| {
        if pad_counts > 0 {
            pad_counts
        } else {
            slots
                .iter()
                .filter(|s| s.placement.label() == label)
                .count()
        }
    };
    let mut out = String::from("<summary>\n");
    let _ = writeln!(
        out,
        "Context bundle for a {} {} input limit. Files are ordered by relevance to the task.",
        frame.limit, frame.unit
    );
    let _ = writeln!(
        out,
        "Files: {} full, {} signatures only (declarations with line numbers), {} omitted to fit the limit, {} withheld.",
        count("full"),
        count("signatures"),
        count("omitted"),
        frame.withheld.len()
    );
    out.push_str(
        "Omitted and withheld files appear only in the directory structure; ask for them by path.\n",
    );
    if !frame.withheld.is_empty() {
        out.push_str("Withheld:\n");
        for w in frame.withheld.iter().take(MAX_LISTED_WITHHELD) {
            let _ = writeln!(
                out,
                "- {}: {}",
                render::escape(&w.path),
                render::escape(&w.reason)
            );
        }
        if frame.withheld.len() > MAX_LISTED_WITHHELD {
            let _ = writeln!(
                out,
                "- and {} more",
                frame.withheld.len() - MAX_LISTED_WITHHELD
            );
        }
    }
    out.push_str("</summary>\n");
    out
}
