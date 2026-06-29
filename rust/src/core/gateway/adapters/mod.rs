//! L4 typed adapters (deeper addon integration, #1096–#1101).
//!
//! Where the generic pipeline ([`super::postprocess`]) treats addon output as
//! opaque text, a *typed* adapter understands a category's payload and folds it
//! into the matching lean-ctx store or retrieval path:
//!
//! | category        | example addons            | what the adapter does                         |
//! |-----------------|---------------------------|-----------------------------------------------|
//! | `codebase-pack` | Repomix                   | pack → archive handle (`ctx_expand`)          |
//! | `code-graph`    | Graphify                  | nodes/edges → property graph (`ctx_callgraph`) |
//! | `code-symbols`  | Serena                    | references → property-graph call edges         |
//! | `memory`        | Mem0/OpenMemory/Cognee    | memories → `ctx_knowledge` facts               |
//! | `compression`   | Headroom/RTK              | downstream as a named `Compressor`             |
//!
//! Routing is config-driven: the owning `[[gateway.servers]]` entry carries an
//! `integration` slug (set at install from the addon's category, or by hand),
//! which the proxy already has in scope — no catalog lookup on the hot path.

pub mod code_graph;
pub mod code_symbols;
pub mod codebase_pack;
pub mod compression;
pub mod memory;

/// The category of deep integration applied to one downstream server's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationKind {
    /// No typed adapter — generic L1–L3 only.
    None,
    /// Repository packer (Repomix): pack → retrievable handle.
    CodebasePack,
    /// Code-graph tool (Graphify): nodes/edges → property graph.
    CodeGraph,
    /// Symbol/LSP tool (Serena): references → property-graph edges.
    CodeSymbols,
    /// Memory tool (Mem0/Cognee/Letta): memories → knowledge facts.
    Memory,
    /// Compressor (Headroom/RTK): registered as a named lean-ctx compressor.
    Compression,
}

impl IntegrationKind {
    /// Parse a canonical (or common-alias) integration slug.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "codebase-pack" | "pack" | "repomix" => Self::CodebasePack,
            "code-graph" | "graph" | "callgraph" => Self::CodeGraph,
            "code-symbols" | "symbols" | "lsp" => Self::CodeSymbols,
            "memory" | "mem" => Self::Memory,
            "compression" | "compress" | "compressor" => Self::Compression,
            _ => Self::None,
        }
    }

    /// First recognizable adapter among an addon's free-form categories.
    #[must_use]
    pub fn from_categories(categories: &[String]) -> Self {
        categories
            .iter()
            .map(|c| Self::parse(c))
            .find(|k| !k.is_none())
            .unwrap_or(Self::None)
    }

    /// Canonical slug (round-trips through [`Self::parse`]).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::CodebasePack => "codebase-pack",
            Self::CodeGraph => "code-graph",
            Self::CodeSymbols => "code-symbols",
            Self::Memory => "memory",
            Self::Compression => "compression",
        }
    }

    #[must_use]
    pub fn is_none(self) -> bool {
        matches!(self, Self::None)
    }
}

/// Side-channel ingestion: spawn a typed background job that folds the output
/// into the matching store. Returns `true` when a typed adapter handled it (the
/// caller then skips the generic L3 indexer); `false` to fall through to L3.
#[must_use]
pub fn ingest_spawn(
    kind: IntegrationKind,
    server: &str,
    tool: &str,
    text: &str,
    project_root: &str,
) -> bool {
    let (server, tool, text, root) = (
        server.to_string(),
        tool.to_string(),
        text.to_string(),
        project_root.to_string(),
    );
    match kind {
        IntegrationKind::CodeGraph => {
            std::thread::spawn(move || code_graph::ingest(&server, &tool, &text, &root));
            true
        }
        IntegrationKind::CodeSymbols => {
            std::thread::spawn(move || code_symbols::ingest(&server, &tool, &text, &root));
            true
        }
        IntegrationKind::Memory => {
            std::thread::spawn(move || memory::ingest(&server, &tool, &text, &root));
            true
        }
        // codebase-pack / compression keep the generic L3 indexer.
        IntegrationKind::None | IntegrationKind::CodebasePack | IntegrationKind::Compression => {
            false
        }
    }
}

/// Model-facing text transform applied before the generic L1/L2 path. Returns
/// `Some(text)` when a typed adapter rewrote the output, else `None`.
#[must_use]
pub fn transform(
    kind: IntegrationKind,
    server: &str,
    tool: &str,
    text: &str,
    budget_tokens: usize,
) -> Option<String> {
    match kind {
        IntegrationKind::CodebasePack => {
            codebase_pack::transform(server, tool, text, budget_tokens)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_and_aliases() {
        assert_eq!(
            IntegrationKind::parse("codebase-pack"),
            IntegrationKind::CodebasePack
        );
        assert_eq!(
            IntegrationKind::parse("repomix"),
            IntegrationKind::CodebasePack
        );
        assert_eq!(IntegrationKind::parse("GRAPH"), IntegrationKind::CodeGraph);
        assert_eq!(IntegrationKind::parse("mem"), IntegrationKind::Memory);
        assert_eq!(
            IntegrationKind::parse("compressor"),
            IntegrationKind::Compression
        );
        assert_eq!(IntegrationKind::parse("whatever"), IntegrationKind::None);
    }

    #[test]
    fn slug_round_trips() {
        for k in [
            IntegrationKind::CodebasePack,
            IntegrationKind::CodeGraph,
            IntegrationKind::CodeSymbols,
            IntegrationKind::Memory,
            IntegrationKind::Compression,
        ] {
            assert_eq!(IntegrationKind::parse(k.as_str()), k);
        }
    }

    #[test]
    fn from_categories_finds_first_match() {
        let cats = vec!["workflow".into(), "graph".into(), "search".into()];
        assert_eq!(
            IntegrationKind::from_categories(&cats),
            IntegrationKind::CodeGraph
        );
        let none = vec!["workflow".into(), "plans".into()];
        assert_eq!(
            IntegrationKind::from_categories(&none),
            IntegrationKind::None
        );
    }

    #[test]
    fn untyped_kinds_do_not_claim_ingestion() {
        assert!(!ingest_spawn(IntegrationKind::None, "s", "t", "x", "/tmp"));
        assert!(!ingest_spawn(
            IntegrationKind::CodebasePack,
            "s",
            "t",
            "x",
            "/tmp"
        ));
        assert!(!ingest_spawn(
            IntegrationKind::Compression,
            "s",
            "t",
            "x",
            "/tmp"
        ));
    }
}
