// SPDX-License-Identifier: Apache-2.0
//! Asking a semantic backend where a call goes, and mapping the answer back
//! onto the project graph.

use std::path::Path;

use lsp_types::{GotoDefinitionResponse, Location, Position, Uri};

use crate::core::call_graph::{CallGraphInputs, SymbolSpan};
use crate::lsp::backend::LspBackend;
use crate::lsp::capabilities::{SemanticCapabilities, encode_column};
use crate::lsp::client::{file_path_to_uri, uri_to_file_path};
use crate::lsp::router::{self, StartPolicy};

/// Outcome of one semantic definition lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Exactly one indexed project file defines the callee.
    Resolved {
        file: String,
        /// 1-based line of the definition.
        line: usize,
        /// Narrowest indexed symbol containing that line.
        symbol: Option<String>,
    },
    /// The definition lives outside the indexed project (std, dependency,
    /// generated or vendored code).
    External,
    /// The backend found no definition.
    NoResult,
    /// Definitions in several project files (e.g. an unresolved overload set).
    Ambiguous,
}

/// A [`Resolution`] plus the identity of the backend that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticAnswer {
    pub resolution: Resolution,
    pub backend: String,
}

/// Project-relative graph key of `path` if — and only if — it lies inside
/// `project_root` (both canonicalized, so `..` and symlinks cannot escape).
pub fn project_key(path: &str, project_root: &str) -> Option<String> {
    use crate::core::pathutil::safe_canonicalize;
    let root = safe_canonicalize(Path::new(project_root)).ok()?;
    let canon = safe_canonicalize(Path::new(path)).ok()?;
    canon.strip_prefix(&root).ok()?;
    Some(crate::core::index_paths::graph_relative_key(
        &canon.to_string_lossy(),
        &root.to_string_lossy(),
    ))
}

/// Narrowest indexed symbol in `file` whose span contains `line` (1-based).
/// Ties break by name so the answer is deterministic.
pub fn symbol_at<'a>(symbols: &'a [SymbolSpan], file: &str, line: usize) -> Option<&'a SymbolSpan> {
    symbols
        .iter()
        .filter(|s| s.file == file && s.start_line <= line && line <= s.end_line)
        .min_by(|a, b| {
            (a.end_line - a.start_line, &a.name).cmp(&(b.end_line - b.start_line, &b.name))
        })
}

fn locations(resp: GotoDefinitionResponse) -> Vec<Location> {
    match resp {
        GotoDefinitionResponse::Scalar(l) => vec![l],
        GotoDefinitionResponse::Array(v) => v,
        GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .map(|l| Location {
                uri: l.target_uri,
                range: l.target_selection_range,
            })
            .collect(),
    }
}

/// Maps backend locations onto the indexed project. Deterministic: candidate
/// files are compared as a sorted set, and the earliest line wins.
pub fn classify(locs: &[Location], project_root: &str, inputs: &CallGraphInputs) -> Resolution {
    if locs.is_empty() {
        return Resolution::NoResult;
    }
    let mut in_project: Vec<(String, usize)> = locs
        .iter()
        .filter_map(|l| {
            indexed_key(&l.uri, project_root, inputs)
                .map(|key| (key, l.range.start.line as usize + 1))
        })
        .collect();
    if in_project.is_empty() {
        return Resolution::External;
    }
    in_project.sort();
    in_project.dedup_by(|a, b| a.0 == b.0);
    if in_project.len() > 1 {
        return Resolution::Ambiguous;
    }
    let (file, line) = in_project.swap_remove(0);
    let symbol = symbol_at(&inputs.symbols, &file, line).map(|s| s.name.clone());
    Resolution::Resolved { file, line, symbol }
}

/// How a semantic query reaches its backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Access {
    /// Whether a server that is not running may be started.
    pub policy: StartPolicy,
    /// `false`: fail fast instead of waiting for a backend busy with another
    /// call — opportunistic work never delays interactive tools.
    pub wait: bool,
    /// Per-request cap (`None` = backend default).
    pub timeout: Option<std::time::Duration>,
    /// Absolute end of the caller's budget. Bounds a server start-up *and* —
    /// recomputed after it — the request, so start-up time is charged too.
    pub deadline: Option<std::time::Instant>,
}

impl Access {
    /// Time left until the deadline (`None` = unbounded).
    fn remaining(&self) -> Option<std::time::Duration> {
        self.deadline
            .map(|d| d.saturating_duration_since(std::time::Instant::now()))
    }

    /// Request timeout: the per-request cap, shortened to what is left.
    fn request_timeout(&self) -> Option<std::time::Duration> {
        match (self.timeout, self.remaining()) {
            (Some(cap), Some(left)) => Some(cap.min(left)),
            (cap, left) => cap.or(left),
        }
    }
}

/// Identity of the backend that answers for `file`'s language — a registry
/// peek (see [`router::live_identity`]), else an editor bridge announced for
/// the project. Used to retire cached answers produced by a different server
/// or server version.
pub fn live_backend_identity(project_root: &str, file: &str) -> router::LiveIdentity {
    let live = router::live_identity(
        &Path::new(project_root).join(file).to_string_lossy(),
        project_root,
    );
    if live == router::LiveIdentity::NotRunning
        && let Some(bridge) = editor_bridge_for(project_root)
    {
        return router::LiveIdentity::Known(bridge.identity(project_root));
    }
    live
}

/// The editor bridge that answers for `project_root` when the router has no
/// backend to offer: no JetBrains IDE attached (it would take precedence),
/// and an editor announced a live bridge for exactly this project.
fn editor_bridge_for(project_root: &str) -> Option<crate::lsp::editor_bridge::BridgeFile> {
    if crate::lsp::port_discovery::read_port_file(project_root).is_some() {
        return None;
    }
    crate::lsp::editor_bridge::discover(project_root)
}

/// Whether a semantic question for `file` can get an answer without
/// starting anything — or may start a server (`eager`). Lets on-demand
/// callers skip the work of preparing a query that would only fail.
pub fn backend_may_answer(project_root: &str, file: &str, policy: StartPolicy) -> bool {
    use crate::lsp::port_discovery::{health_ok, pid_alive, read_port_file};
    policy == StartPolicy::Lazy
        // A port file an IDE crash left behind does not count.
        || read_port_file(project_root).is_some_and(|pf| pid_alive(pf.pid) && health_ok(&pf))
        || live_backend_identity(project_root, file) != router::LiveIdentity::NotRunning
}

/// Why a lookup produced no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// No usable backend for this language (none running under
    /// [`StartPolicy::ReuseOnly`], failed to start, or lacks the feature).
    /// Holds for every call site of the language in this run.
    Unavailable(String),
    /// This site only: unreadable file, timeout, protocol error.
    Site(String),
}

/// Runs one positional query against the backend for `file`: confines the
/// file to the project, syncs its content, encodes the position for the
/// backend's negotiated encoding, and classifies failures. `supported` checks
/// the needed capability; `op` performs the request. Returns the backend
/// identity with the result, and whether the backend capped (truncated) it.
fn query_at<T>(
    project_root: &str,
    file: &str,
    pos: (usize, usize),
    access: Access,
    supported: impl FnOnce(&SemanticCapabilities) -> bool,
    op: impl FnOnce(&mut dyn LspBackend, &Uri, Position) -> Result<T, String>,
) -> Result<(String, T, bool), ResolveError> {
    let abs = Path::new(project_root).join(file);
    let abs_str = abs.to_string_lossy().to_string();
    if project_key(&abs_str, project_root).is_none() {
        return Err(ResolveError::Site(format!("{file} is outside the project")));
    }
    let content = std::fs::read_to_string(&abs)
        .map_err(|e| ResolveError::Site(format!("read {file}: {e}")))?;
    let line_text = content
        .lines()
        .nth(pos.0.saturating_sub(1))
        .ok_or_else(|| ResolveError::Site(format!("{file}: line {} out of range", pos.0)))?;

    if access.remaining().is_some_and(|left| left.is_zero()) {
        return Err(ResolveError::Site("semantic budget exhausted".into()));
    }

    // Set once a backend was handed to us: an error before that point is
    // about backend availability, not about this site.
    let mut reached = false;
    let mut unsupported = false;
    let run = |b: &mut dyn LspBackend, lang: &str| {
        reached = true;
        let info = b.backend_info();
        if !supported(&info.capabilities) {
            unsupported = true;
            return Err(format!("{} lacks the required capability", info.identity()));
        }
        // Recomputed after a possible start-up, which consumed budget too.
        let timeout = access.request_timeout();
        if timeout.is_some_and(|t| t.is_zero()) {
            return Err("semantic budget exhausted during start-up".into());
        }
        let uri: Uri = file_path_to_uri(&abs_str)?;
        b.open_file(&uri, lang, &content)?;
        let position = Position {
            line: u32::try_from(pos.0.saturating_sub(1)).unwrap_or(u32::MAX),
            character: encode_column(line_text, pos.1, info.utf8_positions),
        };
        b.set_request_timeout(timeout);
        let answer = op(b, &uri, position);
        b.set_request_timeout(None);
        let truncated = b.last_truncation().is_some_and(|t| t.truncated);
        Ok((info.identity(), answer?, truncated))
    };
    // An editor bridge answers only when the router has nothing running for
    // this language; it never displaces a live server or IDE.
    let bridge = (router::live_identity(&abs_str, project_root)
        == router::LiveIdentity::NotRunning)
        .then(|| editor_bridge_for(project_root))
        .flatten();
    let result = match bridge {
        Some(bridge) => {
            let lang = Path::new(file)
                .extension()
                .and_then(|e| e.to_str())
                .and_then(crate::lsp::config::language_for_extension)
                .unwrap_or("");
            let result = run(&mut bridge.backend(project_root), lang);
            if result.is_err() {
                // The editor may have closed: rediscover next time.
                crate::lsp::editor_bridge::forget(project_root);
            }
            result
        }
        None => router::with_backend_opts(
            &abs_str,
            project_root,
            access.policy,
            router::BackendOpts {
                wait: access.wait,
                start_timeout: access.remaining(),
            },
            run,
        ),
    };
    result.map_err(|e| {
        if reached && !unsupported {
            ResolveError::Site(e)
        } else {
            ResolveError::Unavailable(e)
        }
    })
}

/// Asks the semantic backend for the definition of the callee at `pos`
/// (`(1-based line, 0-based byte column)`) in `caller_file`.
pub fn resolve_definition(
    project_root: &str,
    caller_file: &str,
    pos: (usize, usize),
    access: Access,
    inputs: &CallGraphInputs,
) -> Result<SemanticAnswer, ResolveError> {
    let (backend, resp, _) = query_at(
        project_root,
        caller_file,
        pos,
        access,
        |c| c.definition,
        |b, uri, p| b.definition(uri, p),
    )?;
    Ok(SemanticAnswer {
        resolution: classify(&locations(resp), project_root, inputs),
        backend,
    })
}

/// A relation answer: the related indexed files (the asking file excluded,
/// sorted, deduplicated), and whether the answer is complete. Only a
/// complete answer may be cached and may settle the asking file — a capped
/// list, or nothing from a server still indexing, may not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedFiles {
    pub files: Vec<String>,
    pub definitive: bool,
}

/// Indexed project files implementing the trait/interface declared at `pos`
/// in `file`. An empty answer is not definitive: a cold server reports none
/// while it indexes.
pub fn resolve_implementations(
    project_root: &str,
    file: &str,
    pos: (usize, usize),
    access: Access,
    inputs: &CallGraphInputs,
) -> Result<(String, RelatedFiles), ResolveError> {
    let (backend, locs, truncated) = query_at(
        project_root,
        file,
        pos,
        access,
        |c| c.implementations,
        |b, uri, p| b.implementations(uri, p, "project"),
    )?;
    let files = other_indexed(
        locs.iter()
            .map(|l| indexed_key(&l.uri, project_root, inputs)),
        file,
    );
    let definitive = !truncated && !files.is_empty();
    Ok((backend, RelatedFiles { files, definitive }))
}

/// Index spelling of the project file behind `uri`, if it is indexed.
fn indexed_key(uri: &Uri, project_root: &str, inputs: &CallGraphInputs) -> Option<String> {
    let path = uri_to_file_path(uri)?;
    indexed_path(&path, project_root, inputs)
}

/// Index spelling of `path` — absolute, or relative to the project root as
/// IDE bridges report it — if it is an indexed project file.
fn indexed_path(path: &str, project_root: &str, inputs: &CallGraphInputs) -> Option<String> {
    let absolute = if Path::new(path).is_absolute() {
        path.to_string()
    } else {
        Path::new(project_root)
            .join(path)
            .to_string_lossy()
            .to_string()
    };
    let key = project_key(&absolute, project_root)?.replace('\\', "/");
    inputs
        .file_paths
        .iter()
        .find(|f| f.replace('\\', "/") == key)
        .cloned()
}

/// Sorted, deduplicated indexed files other than `file`.
fn other_indexed(paths: impl Iterator<Item = Option<String>>, file: &str) -> Vec<String> {
    let set: std::collections::BTreeSet<String> = paths.flatten().filter(|f| f != file).collect();
    set.into_iter().collect()
}

/// Indexed project files declaring a direct supertype of the type declared
/// at `pos` in `file`. Not definitive when the backend could not place a
/// type there (a cold or still-indexing server) or capped the tree; a type
/// with no supertypes in the project is a definitive empty answer.
pub fn resolve_supertypes(
    project_root: &str,
    file: &str,
    pos: (usize, usize),
    access: Access,
    inputs: &CallGraphInputs,
) -> Result<(String, RelatedFiles), ResolveError> {
    let (backend, node, truncated) = query_at(
        project_root,
        file,
        pos,
        access,
        |c| c.type_hierarchy,
        |b, uri, p| b.type_hierarchy(uri, p, crate::lsp::backend::HierarchyDirection::Supertypes),
    )?;
    let files = other_indexed(
        node.children
            .iter()
            .map(|c| indexed_path(&c.path, project_root, inputs)),
        file,
    );
    let definitive = !truncated && !node.name.is_empty();
    Ok((backend, RelatedFiles { files, definitive }))
}

/// Indexed project files that reference the symbol declared at `pos` in
/// `file` (its own file excluded). The answer includes the declaration
/// itself, so an empty one means the server had nothing yet; an answer
/// naming only `file` is definitive: nothing else uses the symbol. A capped
/// list is never definitive.
pub fn resolve_references(
    project_root: &str,
    file: &str,
    pos: (usize, usize),
    access: Access,
    inputs: &CallGraphInputs,
) -> Result<(String, RelatedFiles), ResolveError> {
    let (backend, locs, truncated) = query_at(
        project_root,
        file,
        pos,
        access,
        |c| c.references,
        |b, uri, p| b.references(uri, p, "project"),
    )?;
    let definitive = !truncated && !locs.is_empty();
    let files = other_indexed(
        locs.iter()
            .map(|l| indexed_key(&l.uri, project_root, inputs)),
        file,
    );
    Ok((backend, RelatedFiles { files, definitive }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(path: &std::path::Path, line: u32) -> Location {
        Location {
            uri: file_path_to_uri(&path.to_string_lossy()).unwrap(),
            range: lsp_types::Range {
                start: Position { line, character: 0 },
                end: Position { line, character: 1 },
            },
        }
    }

    #[test]
    fn classify_maps_locations_onto_the_indexed_project_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        for f in ["src/repo.rs", "src/other.rs", "src/gen.rs"] {
            std::fs::write(root.join(f), "x").unwrap();
        }
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("std.rs"), "x").unwrap();

        let inputs = CallGraphInputs {
            project_root: root.to_string_lossy().to_string(),
            // gen.rs exists on disk but is not indexed.
            file_paths: vec!["src/repo.rs".into(), "src/other.rs".into()],
            symbols: vec![
                SymbolSpan {
                    file: "src/repo.rs".into(),
                    name: "Repo".into(),
                    start_line: 1,
                    end_line: 20,
                    ..Default::default()
                },
                SymbolSpan {
                    file: "src/repo.rs".into(),
                    name: "save".into(),
                    start_line: 5,
                    end_line: 9,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let r = root.to_string_lossy().to_string();

        assert_eq!(
            classify(&[loc(&root.join("src/repo.rs"), 6)], &r, &inputs),
            Resolution::Resolved {
                file: "src/repo.rs".into(),
                line: 7,
                symbol: Some("save".into()),
            },
            "0-based LSP line → 1-based, narrowest enclosing symbol"
        );
        assert_eq!(
            classify(&[loc(&outside.path().join("std.rs"), 0)], &r, &inputs),
            Resolution::External
        );
        assert_eq!(
            classify(&[loc(&root.join("src/gen.rs"), 0)], &r, &inputs),
            Resolution::External,
            "unindexed project files never become graph targets"
        );
        assert_eq!(
            classify(
                &[
                    loc(&root.join("src/repo.rs"), 1),
                    loc(&root.join("src/other.rs"), 1)
                ],
                &r,
                &inputs
            ),
            Resolution::Ambiguous
        );
        assert_eq!(classify(&[], &r, &inputs), Resolution::NoResult);
    }
}
