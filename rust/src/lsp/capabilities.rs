// SPDX-License-Identifier: Apache-2.0
//! What a concrete semantic backend can actually do.
//!
//! LSP servers advertise their features in the `initialize` handshake; the
//! router and every semantic consumer ask [`SemanticCapabilities`] instead of
//! assuming that "an LSP server" supports a given request.

use lsp_types::{OneOf, ServerCapabilities};

/// Which kind of backend answers semantic queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticBackendKind {
    /// Stdio language server managed by lean-ctx (`LspClient`).
    Lsp,
    /// Live JetBrains IDE over the plugin's HTTP bridge.
    JetBrains,
    /// Editor extension's semantic bridge (VS Code, Cursor, Windsurf).
    Editor,
}

impl SemanticBackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lsp => "lsp",
            Self::JetBrains => "jetbrains",
            Self::Editor => "editor",
        }
    }
}

/// Negotiated feature set of one backend instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SemanticCapabilities {
    pub definition: bool,
    pub declaration: bool,
    pub references: bool,
    pub implementations: bool,
    pub type_hierarchy: bool,
    pub call_hierarchy: bool,
    pub document_symbols: bool,
    pub rename: bool,
}

impl SemanticCapabilities {
    /// Everything — the JetBrains bridge exposes the full PSI feature set.
    pub const ALL: Self = Self {
        definition: true,
        declaration: true,
        references: true,
        implementations: true,
        type_hierarchy: true,
        call_hierarchy: true,
        document_symbols: true,
        rename: true,
    };

    /// Maps the server's `initialize` answer. A provider counts as present
    /// unless the server explicitly answers `false`.
    pub fn from_server(caps: &ServerCapabilities) -> Self {
        fn one_of<T>(p: Option<&OneOf<bool, T>>) -> bool {
            matches!(p, Some(OneOf::Left(true) | OneOf::Right(_)))
        }
        Self {
            definition: one_of(caps.definition_provider.as_ref()),
            declaration: caps
                .declaration_provider
                .as_ref()
                .is_some_and(|p| !matches!(p, lsp_types::DeclarationCapability::Simple(false))),
            references: one_of(caps.references_provider.as_ref()),
            implementations: caps.implementation_provider.as_ref().is_some_and(|p| {
                !matches!(
                    p,
                    lsp_types::ImplementationProviderCapability::Simple(false)
                )
            }),
            // lsp-types has no `typeHierarchyProvider` field: the stdio
            // client reads it from the raw `initialize` answer.
            type_hierarchy: false,
            call_hierarchy: caps.call_hierarchy_provider.as_ref().is_some_and(|p| {
                !matches!(p, lsp_types::CallHierarchyServerCapability::Simple(false))
            }),
            document_symbols: one_of(caps.document_symbol_provider.as_ref()),
            rename: one_of(caps.rename_provider.as_ref()),
        }
    }
}

/// Identity + capabilities of one backend instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBackendInfo {
    pub kind: SemanticBackendKind,
    /// `serverInfo.name` from `initialize` (e.g. `rust-analyzer`), if reported.
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    pub capabilities: SemanticCapabilities,
    /// Negotiated LSP position encoding: `true` = UTF-8 byte offsets, `false`
    /// = UTF-16 code units (the protocol default, also used by the JetBrains
    /// bridge). Callers convert tree-sitter byte columns accordingly.
    pub utf8_positions: bool,
}

/// Converts a 0-based byte column within `line_text` into the character
/// offset a backend expects. Columns inside a multi-byte char snap back to
/// that char's start; columns past the end clamp to the line length.
pub fn encode_column(line_text: &str, byte_col: usize, utf8_positions: bool) -> u32 {
    let mut col = byte_col.min(line_text.len());
    while !line_text.is_char_boundary(col) {
        col -= 1;
    }
    let prefix = &line_text[..col];
    let units = if utf8_positions {
        prefix.len()
    } else {
        prefix.encode_utf16().count()
    };
    u32::try_from(units).unwrap_or(u32::MAX)
}

/// Some servers report build metadata as their version (gopls: a multi-KB
/// JSON build info). The version ends up in every evidence record and cache
/// key, so anything that is not a short plain token is reduced to its
/// release (`Version` field of a JSON object, if present) plus a content
/// hash — still unique per build, but bounded.
pub fn compact_server_version(raw: &str) -> String {
    const MAX: usize = 64;
    /// Build info beyond this is not parsed, only hashed.
    const MAX_PARSED: usize = 64 * 1024;
    let raw = raw.trim();
    if raw.len() <= MAX && !raw.contains(['{', '\n', '"']) {
        return raw.to_string();
    }
    let digest = blake3::hash(raw.as_bytes()).to_hex();
    let release = (raw.len() <= MAX_PARSED)
        .then(|| serde_json::from_str::<serde_json::Value>(raw).ok())
        .flatten()
        .and_then(|v| v.get("Version")?.as_str().map(str::to_string))
        .filter(|v| v.len() <= MAX && !v.contains(['{', '\n', '"']));
    match release {
        Some(v) => format!("{v}+{}", &digest[..12]),
        None => format!("build+{}", &digest[..12]),
    }
}

impl SemanticBackendInfo {
    /// Stable identity used to key cached semantic results: a different server
    /// or server version may resolve differently, so its results never mix.
    pub fn identity(&self) -> String {
        format!(
            "{}:{}@{}",
            self.kind.as_str(),
            self.server_name.as_deref().unwrap_or("unknown"),
            self.server_version.as_deref().unwrap_or("unknown")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{ImplementationProviderCapability, OneOf, ServerCapabilities};

    #[test]
    fn negotiated_capabilities_follow_the_server_answer() {
        let caps = ServerCapabilities {
            definition_provider: Some(OneOf::Left(true)),
            references_provider: Some(OneOf::Left(false)),
            implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
            ..Default::default()
        };
        let negotiated = SemanticCapabilities::from_server(&caps);
        assert!(negotiated.definition);
        assert!(!negotiated.references, "explicit false must be honoured");
        assert!(negotiated.implementations);
        assert!(!negotiated.rename, "absent provider means unsupported");
    }

    #[test]
    fn byte_columns_are_encoded_per_negotiated_encoding() {
        // `ä` is 2 bytes / 1 UTF-16 unit; `😀` is 4 bytes / 2 UTF-16 units.
        let line = "ä😀.save()";
        let save = line.find("save").unwrap(); // byte 7
        assert_eq!(encode_column(line, save, true), 7);
        assert_eq!(encode_column(line, save, false), 4);
        assert_eq!(
            encode_column(line, 3, false),
            1,
            "inside 😀 snaps to its start"
        );
        assert_eq!(encode_column(line, 999, true), line.len() as u32);
    }

    #[test]
    fn server_versions_are_bounded_but_stay_unique_per_build() {
        assert_eq!(compact_server_version("1.15.0"), "1.15.0");
        // gopls reports its JSON build info as the version.
        let a =
            r#"{"GoVersion":"go1.26","Deps":[{"Path":"x","Version":"v1"}],"Version":"v0.23.0"}"#;
        let b =
            r#"{"GoVersion":"go1.27","Deps":[{"Path":"x","Version":"v1"}],"Version":"v0.23.0"}"#;
        let (ca, cb) = (compact_server_version(a), compact_server_version(b));
        assert!(ca.starts_with("v0.23.0+") && ca.len() <= 24, "{ca}");
        assert_ne!(ca, cb, "a different build keeps a different identity");
        let blob = "x".repeat(500);
        assert!(compact_server_version(&blob).starts_with("build+"));
    }
}
