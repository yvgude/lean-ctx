use std::fmt;

#[derive(Debug, Clone)]
pub struct CompetitorProfile {
    pub name: &'static str,
    pub version: &'static str,
    pub compression_pct: Option<f64>,
    pub source: &'static str,
    pub url: &'static str,
    pub supports_search: bool,
    pub supports_caching: bool,
    pub supports_multi_mode: bool,
    pub supports_session_memory: bool,
    pub feature_count: usize,
}

impl fmt::Display for CompetitorProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name, self.version)
    }
}

#[must_use]
pub fn all_competitors() -> Vec<CompetitorProfile> {
    vec![
        CompetitorProfile {
            name: "Raw file read",
            version: "baseline",
            compression_pct: Some(0.0),
            source: "Baseline — no compression applied",
            url: "",
            supports_search: false,
            supports_caching: false,
            supports_multi_mode: false,
            supports_session_memory: false,
            feature_count: 1,
        },
        CompetitorProfile {
            name: "Repomix",
            version: "--compress",
            compression_pct: Some(70.0),
            source: "Repomix docs (Tree-sitter compress mode)",
            url: "https://github.com/yamadashy/repomix",
            supports_search: false,
            supports_caching: false,
            supports_multi_mode: false,
            supports_session_memory: false,
            feature_count: 3,
        },
        CompetitorProfile {
            name: "aider /map",
            version: "repo-map",
            compression_pct: Some(85.0),
            source: "aider docs (repo-map with ctags/tree-sitter)",
            url: "https://aider.chat/docs/repomap.html",
            supports_search: false,
            supports_caching: true,
            supports_multi_mode: false,
            supports_session_memory: false,
            feature_count: 4,
        },
        CompetitorProfile {
            name: "codebase-memory-mcp",
            version: "graph-queries",
            compression_pct: Some(99.2),
            source: "arXiv paper (graph-query extraction only)",
            url: "https://github.com/nicobailey/codebase-memory-mcp",
            supports_search: true,
            supports_caching: true,
            supports_multi_mode: false,
            supports_session_memory: true,
            feature_count: 5,
        },
        CompetitorProfile {
            name: "TokenForge",
            version: "full-stack",
            // Code engine is AST folding at 40-70%; we list the top of the
            // published range to match how Repomix's "up to 70%" is reported.
            compression_pct: Some(70.0),
            source: "TokenForge README (tree-sitter code folding 40-70%; \
                     full-stack: code/command/conversation/json/mcp-schema)",
            url: "https://github.com/Manavarya09/tokenforge",
            supports_search: false,
            supports_caching: true,
            supports_multi_mode: true,
            supports_session_memory: true,
            feature_count: 6,
        },
    ]
}

#[must_use]
pub fn competitor_count() -> usize {
    all_competitors().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_competitors_non_empty() {
        let c = all_competitors();
        assert!(c.len() >= 3);
    }

    #[test]
    fn baseline_is_zero_compression() {
        let c = all_competitors();
        let baseline = c.iter().find(|p| p.name == "Raw file read").unwrap();
        assert_eq!(baseline.compression_pct, Some(0.0));
    }

    #[test]
    fn display_includes_version() {
        let c = &all_competitors()[1];
        let s = format!("{c}");
        assert!(s.contains(c.version));
    }
}
