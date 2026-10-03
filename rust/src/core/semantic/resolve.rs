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

/// Identity of the backend already cached for `file`'s language — a pure
/// registry peek (see [`router::live_identity`]). Used to retire cached
/// answers produced by a different server or server version.
pub fn live_backend_identity(project_root: &str, file: &str) -> router::LiveIdentity {
    router::live_identity(
        &Path::new(project_root).join(file).to_string_lossy(),
        project_root,
    )
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
/// identity with the result.
fn query_at<T>(
    project_root: &str,
    file: &str,
    pos: (usize, usize),
    access: Access,
    supported: impl FnOnce(&SemanticCapabilities) -> bool,
    op: impl FnOnce(&mut dyn LspBackend, &Uri, Position) -> Result<T, String>,
) -> Result<(String, T), ResolveError> {
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

    // Set once the router handed us a backend: an error before that point is
    // about backend availability, not about this site.
    let mut reached = false;
    let mut unsupported = false;
    let result = router::with_backend_opts(
        &abs_str,
        project_root,
        access.policy,
        router::BackendOpts {
            wait: access.wait,
            start_timeout: access.remaining(),
        },
        |b, lang| {
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
            Ok((info.identity(), answer?))
        },
    );
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
    let (backend, resp) = query_at(
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

/// Indexed project files implementing the trait/interface declared at `pos`
/// in `file` (excluding `file` itself), sorted and deduplicated.
pub fn resolve_implementations(
    project_root: &str,
    file: &str,
    pos: (usize, usize),
    access: Access,
    inputs: &CallGraphInputs,
) -> Result<(String, Vec<String>), ResolveError> {
    let (backend, locs) = query_at(
        project_root,
        file,
        pos,
        access,
        |c| c.implementations,
        |b, uri, p| b.implementations(uri, p, "project"),
    )?;
    let mut files: Vec<String> = locs
        .iter()
        .filter_map(|l| indexed_key(&l.uri, project_root, inputs))
        .filter(|f| f != file)
        .collect();
    files.sort();
    files.dedup();
    Ok((backend, files))
}

/// Index spelling of the project file behind `uri`, if it is indexed.
fn indexed_key(uri: &Uri, project_root: &str, inputs: &CallGraphInputs) -> Option<String> {
    let path = uri_to_file_path(uri)?;
    let key = project_key(&path, project_root)?.replace('\\', "/");
    inputs
        .file_paths
        .iter()
        .find(|f| f.replace('\\', "/") == key)
        .cloned()
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
