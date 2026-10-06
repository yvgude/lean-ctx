// SPDX-License-Identifier: Apache-2.0
//! Per-language semantic coverage: how many of a language's file-level call
//! edges are verified, and whether a language server for it can run here.
//! Shared by `ctx_graph status`, `lean-ctx doctor` and the dashboard legend.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::core::property_graph::{CodeGraph, EdgeKind};

use super::evidence::{EdgeEvidence, EvidenceGrade};

/// Evidence counts of one language's file-level `calls` edges.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct GradeCounts {
    pub verified: usize,
    pub resolved: usize,
    pub heuristic: usize,
    /// Edges without typed evidence (older graphs, other producers).
    pub unannotated: usize,
}

impl GradeCounts {
    pub fn total(&self) -> usize {
        self.verified + self.resolved + self.heuristic + self.unannotated
    }

    /// Verified share in whole percent (0 when there are no edges).
    pub fn verified_percent(&self) -> usize {
        (self.verified * 100).checked_div(self.total()).unwrap_or(0)
    }
}

/// `calls` edge evidence grouped by the caller file's language (the
/// `LanguageId` spelling: `rust`, `typescript`, …), sorted by language.
pub fn coverage_by_language(graph: &CodeGraph) -> BTreeMap<&'static str, GradeCounts> {
    let mut out: BTreeMap<&'static str, GradeCounts> = BTreeMap::new();
    for (source, _, metadata) in graph
        .file_edges_of_kind(&EdgeKind::Calls)
        .unwrap_or_default()
    {
        let Some(lang) = crate::core::language_capabilities::language_for_path(&source) else {
            continue;
        };
        let counts = out.entry(lang.id_str()).or_default();
        match EdgeEvidence::from_metadata(metadata.as_deref()).map(|e| e.grade) {
            Some(EvidenceGrade::VerifiedSemantic) => counts.verified += 1,
            Some(EvidenceGrade::ResolvedStructural) => counts.resolved += 1,
            Some(EvidenceGrade::HeuristicStructural) => counts.heuristic += 1,
            None => counts.unannotated += 1,
        }
    }
    out
}

/// Whether lean-ctx can start a standalone language server for a language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerStatus {
    /// Server binary lean-ctx would start (`rust-analyzer`, `gopls`, …).
    pub binary: String,
    /// The binary exists *and* can run (a rustup proxy without the installed
    /// component does not count).
    pub runnable: bool,
    /// How to install it, when it cannot run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<&'static str>,
}

/// Standalone-server status for a language (`LanguageId` spelling) in
/// `project_root` — the server the router would start there (a project's own
/// TypeScript counts); `None` when lean-ctx knows no standalone server for it
/// (a JetBrains IDE may still serve it).
pub fn server_status(language: &str, project_root: &str) -> Option<ServerStatus> {
    let default = crate::lsp::config::default_servers()
        .remove(language)?
        .command;
    let resolved = resolved_binary_cached(language, project_root);
    // JavaScript is served by the TypeScript server.
    let hint_lang = if language == "javascript" {
        "typescript"
    } else {
        language
    };
    Some(ServerStatus {
        install_hint: resolved
            .is_none()
            .then(|| crate::lsp::config::install_hint_for_language(hint_lang)),
        runnable: resolved.is_some(),
        binary: resolved.unwrap_or(default),
    })
}

/// The server binary for `language` in `project_root`, if one can run.
/// Resolving may spawn `<proxy> --version`; status surfaces (dashboard
/// polling) ask repeatedly, so answers are kept for a minute.
fn resolved_binary_cached(language: &str, project_root: &str) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex, PoisonError};
    use std::time::{Duration, Instant};
    type Key = (String, String);
    static CACHE: LazyLock<Mutex<HashMap<Key, (Option<String>, Instant)>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    const TTL: Duration = Duration::from_mins(1);

    let key = (language.to_string(), project_root.to_string());
    if let Some((binary, at)) = CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        && at.elapsed() < TTL
    {
        return binary.clone();
    }
    // Exactly what the router would start (a configured `[lsp]` path counts).
    let binary = crate::lsp::router::standalone_server(language, project_root)
        .ok()
        .map(|s| s.binary_name());
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    cache.retain(|_, (_, at)| at.elapsed() < TTL);
    cache.insert(key, (binary.clone(), Instant::now()));
    binary
}

/// What a backend negotiated for a language at its last start (the
/// `initialize` handshake), as recorded in `<data dir>/semantic-backends.json`
/// — across processes, so `lean-ctx doctor` sees what the MCP server's
/// backend can do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct NegotiatedBackend {
    /// Backend identity (`lsp:rust-analyzer@1.97.1`, `jetbrains:…`).
    pub identity: String,
    /// Semantic features the backend offered, among those lean-ctx uses.
    pub features: Vec<String>,
    /// Features lean-ctx uses that the backend did not offer.
    pub missing: Vec<String>,
}

/// The features lean-ctx's semantic layer uses, in display order.
fn used_features(
    caps: &crate::lsp::capabilities::SemanticCapabilities,
) -> [(&'static str, bool); 4] {
    [
        ("definition", caps.definition),
        ("references", caps.references),
        ("implementations", caps.implementations),
        ("type hierarchy", caps.type_hierarchy),
    ]
}

fn negotiated_path() -> Option<std::path::PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("semantic-backends.json"))
}

fn read_negotiated() -> BTreeMap<String, NegotiatedBackend> {
    negotiated_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Records what a freshly started backend negotiated for `language`.
/// Written only when it changed; best effort (a status display, not state
/// anything depends on).
pub fn record_negotiated(language: &str, info: &crate::lsp::capabilities::SemanticBackendInfo) {
    let (offered, absent): (Vec<_>, Vec<_>) = used_features(&info.capabilities)
        .into_iter()
        .partition(|(_, on)| *on);
    let entry = NegotiatedBackend {
        identity: info.identity(),
        features: offered.into_iter().map(|(n, _)| n.to_string()).collect(),
        missing: absent.into_iter().map(|(n, _)| n.to_string()).collect(),
    };
    if read_negotiated().get(language) == Some(&entry) {
        return;
    }
    let Some(path) = negotiated_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Backends for several languages (or in several processes) may start at
    // once: the read-modify-write runs under a cross-process lock so no
    // start drops another language's entry.
    let Ok(lock) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("json.lock"))
    else {
        return;
    };
    if crate::core::file_lock::acquire_exclusive_timeout(&lock, std::time::Duration::from_secs(2))
        .is_err()
    {
        return;
    }
    let mut all = read_negotiated();
    if all.get(language) != Some(&entry) {
        all.insert(language.to_string(), entry);
        if let Ok(json) = serde_json::to_vec_pretty(&all) {
            let _ = crate::core::atomic_fs::try_atomic_write(&path, &json, None);
        }
    }
    let _ = fs2::FileExt::unlock(&lock);
}

/// What the last backend started for `language` negotiated, if one ever ran.
pub fn negotiated(language: &str) -> Option<NegotiatedBackend> {
    read_negotiated().remove(language)
}

/// One line per language for terminal output, e.g.
/// `  rust        96/120 verified (80%) · rust-analyzer ✓ · offers definition, references, implementations; no type hierarchy`,
/// from [`coverage_by_language`]. Sorted by language; deterministic.
pub fn coverage_lines(
    coverage: &BTreeMap<&'static str, GradeCounts>,
    project_root: &str,
) -> Vec<String> {
    let negotiated = read_negotiated();
    coverage
        .iter()
        .map(|(lang, c)| {
            let server = match server_status(lang, project_root) {
                Some(s) if s.runnable => format!("{} ✓", s.binary),
                Some(s) => format!(
                    "{} not installed ({})",
                    s.binary,
                    s.install_hint.unwrap_or("see docs")
                ),
                None => "no standalone server (JetBrains IDE only)".to_string(),
            };
            let features = negotiated
                .get(*lang)
                .map(|n| {
                    let mut s = format!(" · offers {}", n.features.join(", "));
                    if !n.missing.is_empty() {
                        s.push_str(&format!("; no {}", n.missing.join(", no ")));
                    }
                    s
                })
                .unwrap_or_default();
            format!(
                "  {lang:<11} {}/{} verified ({}%) · {server}{features}",
                c.verified,
                c.total(),
                c.verified_percent()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property_graph::Node;
    use crate::core::semantic::EvidenceOrigin;

    #[test]
    #[allow(clippy::many_single_char_names)] // graph test nodes
    fn calls_are_counted_per_caller_language_and_grade() {
        let g = CodeGraph::open_in_memory().unwrap();
        let id = |p: &str| g.upsert_node(&Node::file(p)).unwrap();
        let (a, b, t, u) = (
            id("src/a.rs"),
            id("src/b.rs"),
            id("web/t.ts"),
            id("web/u.ts"),
        );
        let ev = |grade| EdgeEvidence::new(grade, EvidenceOrigin::Enrichment, None, 1);
        g.upsert_edge_with_evidence(a, b, &EdgeKind::Calls, &ev(EvidenceGrade::VerifiedSemantic))
            .unwrap();
        g.upsert_edge_with_evidence(
            b,
            a,
            &EdgeKind::Calls,
            &ev(EvidenceGrade::HeuristicStructural),
        )
        .unwrap();
        g.upsert_edge_with_evidence(
            t,
            u,
            &EdgeKind::Calls,
            &ev(EvidenceGrade::ResolvedStructural),
        )
        .unwrap();

        let cov = coverage_by_language(&g);
        let rust = cov["rust"];
        assert_eq!((rust.verified, rust.heuristic, rust.total()), (1, 1, 2));
        assert_eq!(rust.verified_percent(), 50);
        assert_eq!(cov["typescript"].resolved, 1);
        assert_eq!(
            GradeCounts::default().verified_percent(),
            0,
            "no division by zero"
        );
    }

    /// What a backend negotiated survives the process that started it and
    /// shows up on the coverage line — including what it lacks.
    #[test]
    fn negotiated_features_are_recorded_and_shown() {
        use crate::lsp::capabilities::{
            SemanticBackendInfo, SemanticBackendKind, SemanticCapabilities,
        };
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let info = SemanticBackendInfo {
            kind: SemanticBackendKind::Lsp,
            server_name: Some("rust-analyzer".into()),
            server_version: Some("1.97.1".into()),
            capabilities: SemanticCapabilities {
                definition: true,
                references: true,
                implementations: true,
                ..SemanticCapabilities::default()
            },
            utf8_positions: true,
        };
        record_negotiated("rust", &info);
        let n = negotiated("rust").expect("recorded");
        assert_eq!(n.identity, "lsp:rust-analyzer@1.97.1");
        assert_eq!(n.missing, ["type hierarchy"]);
        assert_eq!(negotiated("go"), None);

        let coverage = BTreeMap::from([("rust", GradeCounts::default())]);
        let line = &coverage_lines(&coverage, "/nonexistent/project")[0];
        assert!(
            line.ends_with("· offers definition, references, implementations; no type hierarchy"),
            "{line}"
        );

        // Backends starting at once must not drop each other's entries.
        let langs = ["go", "python", "typescript", "java", "c", "lua"];
        std::thread::scope(|s| {
            for lang in langs {
                let info = info.clone();
                s.spawn(move || record_negotiated(lang, &info));
            }
        });
        for lang in langs.iter().chain(&["rust"]) {
            assert!(negotiated(lang).is_some(), "{lang} entry lost");
        }
    }
}
