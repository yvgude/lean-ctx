//! Pluggable read-modes, compressors, and chunkers (`extension-registry-v1`).
//!
//! Previously these sets were hardcoded. This module turns them into registries
//! that extensions can extend at runtime. Built-ins register through the *same*
//! path as extensions — no special-casing — so a third-party read-mode,
//! compressor, or chunker is a first-class citizen, discoverable via
//! `GET /v1/capabilities`.
//!
//! The performance-critical in-core read modes keep their optimized path; this
//! registry is the stable, named extension seam (text→text / text→chunks
//! transforms) and the home extensions plug into.

// The `name()` trait methods return `&str` (not `&'static str`) on purpose so an
// extension can return a runtime-owned name. Built-ins return literals, which
// trips `unnecessary_literal_bound`; the flexibility is intentional.
#![allow(clippy::unnecessary_literal_bound)]

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};

/// A named text→text transform (e.g. a domain compression dictionary).
pub trait Compressor: Send + Sync {
    /// Stable registry name.
    fn name(&self) -> &str;
    /// Compress `input`, optionally honoring a soft byte budget.
    fn compress(&self, input: &str, budget: Option<usize>) -> String;
}

/// A named splitter that turns text into index chunks.
pub trait Chunker: Send + Sync {
    /// Stable registry name.
    fn name(&self) -> &str;
    /// Split `input` into chunks.
    fn chunk(&self, input: &str) -> Vec<String>;
}

/// A named read-mode renderer operating on a file's source + path.
pub trait ReadMode: Send + Sync {
    /// Stable registry name.
    fn name(&self) -> &str;
    /// Render the read output for `source` at `path`.
    fn render(&self, source: &str, path: &str) -> String;
}

/// Registry of pluggable read-modes, compressors, and chunkers.
#[derive(Default)]
pub struct ExtensionRegistry {
    read_modes: BTreeMap<String, Arc<dyn ReadMode>>,
    compressors: BTreeMap<String, Arc<dyn Compressor>>,
    chunkers: BTreeMap<String, Arc<dyn Chunker>>,
}

impl ExtensionRegistry {
    /// An empty registry (no built-ins). Use [`ExtensionRegistry::with_builtins`]
    /// for the production set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry seeded with the built-in transforms — registered through the
    /// same public API extensions use.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        reg.register_read_mode(Arc::new(FullReadMode));
        reg.register_compressor(Arc::new(IdentityCompressor));
        reg.register_compressor(Arc::new(WhitespaceCompressor));
        // Non-code compressors (prose/markdown) tuned for prose/web/data corpora
        // (EPIC 12.14), registered through the same public path.
        crate::core::nc_compress::register_into(&mut reg);
        reg.register_chunker(Arc::new(LineChunker::default()));
        reg.register_chunker(Arc::new(ParagraphChunker));
        // Format-aware chunkers (csv/json/eml/html) register through the same
        // public path so they are first-class + conformance-checked (EPIC 12.13).
        crate::core::extractors::register_into(&mut reg);
        // Opt-in WASM compressors discovered from `LEAN_CTX_WASM_DIR` (EPIC 12.8).
        // First-class once registered: discoverable via `/v1/capabilities` and
        // checked by the conformance scorecard like any other compressor.
        #[cfg(feature = "wasm")]
        if let Ok(dir) = std::env::var("LEAN_CTX_WASM_DIR") {
            crate::core::wasm_ext::register_compressors_from_dir(&mut reg, dir);
        }
        reg
    }

    /// Register (or replace) a read-mode by its name.
    pub fn register_read_mode(&mut self, handler: Arc<dyn ReadMode>) {
        self.read_modes.insert(handler.name().to_string(), handler);
    }

    /// Register (or replace) a compressor by its name.
    pub fn register_compressor(&mut self, handler: Arc<dyn Compressor>) {
        self.compressors.insert(handler.name().to_string(), handler);
    }

    /// Register (or replace) a chunker by its name.
    pub fn register_chunker(&mut self, handler: Arc<dyn Chunker>) {
        self.chunkers.insert(handler.name().to_string(), handler);
    }

    /// Look up a read-mode by name.
    #[must_use]
    pub fn read_mode(&self, name: &str) -> Option<Arc<dyn ReadMode>> {
        self.read_modes.get(name).cloned()
    }

    /// Look up a compressor by name.
    #[must_use]
    pub fn compressor(&self, name: &str) -> Option<Arc<dyn Compressor>> {
        self.compressors.get(name).cloned()
    }

    /// Look up a chunker by name.
    #[must_use]
    pub fn chunker(&self, name: &str) -> Option<Arc<dyn Chunker>> {
        self.chunkers.get(name).cloned()
    }

    /// Registered read-mode names (sorted).
    #[must_use]
    pub fn read_mode_names(&self) -> Vec<String> {
        self.read_modes.keys().cloned().collect()
    }

    /// Registered compressor names (sorted).
    #[must_use]
    pub fn compressor_names(&self) -> Vec<String> {
        self.compressors.keys().cloned().collect()
    }

    /// Registered chunker names (sorted).
    #[must_use]
    pub fn chunker_names(&self) -> Vec<String> {
        self.chunkers.keys().cloned().collect()
    }
}

/// Process-global registry, seeded with built-ins on first access.
pub fn global() -> &'static RwLock<ExtensionRegistry> {
    static REGISTRY: OnceLock<RwLock<ExtensionRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(ExtensionRegistry::with_builtins()))
}

// ----------------------------------------------------------------------------
// Built-in implementations (real, not stubs).
// ----------------------------------------------------------------------------

/// `full`: return the source verbatim (the byte-faithful default read mode).
struct FullReadMode;
impl ReadMode for FullReadMode {
    fn name(&self) -> &str {
        "full"
    }
    fn render(&self, source: &str, _path: &str) -> String {
        source.to_string()
    }
}

/// `identity`: pass content through unchanged (honoring a hard byte budget).
struct IdentityCompressor;
impl Compressor for IdentityCompressor {
    fn name(&self) -> &str {
        "identity"
    }
    fn compress(&self, input: &str, budget: Option<usize>) -> String {
        truncate_to_budget(input.to_string(), budget)
    }
}

/// `whitespace`: collapse runs of blank lines and strip trailing whitespace.
struct WhitespaceCompressor;
impl Compressor for WhitespaceCompressor {
    fn name(&self) -> &str {
        "whitespace"
    }
    fn compress(&self, input: &str, budget: Option<usize>) -> String {
        let mut out = String::with_capacity(input.len());
        let mut blank_run = 0u32;
        for line in input.lines() {
            if line.trim().is_empty() {
                blank_run += 1;
                if blank_run > 1 {
                    continue;
                }
                out.push('\n');
            } else {
                blank_run = 0;
                out.push_str(line.trim_end());
                out.push('\n');
            }
        }
        truncate_to_budget(out, budget)
    }
}

/// `lines`: fixed-size, non-overlapping windows of source lines.
struct LineChunker {
    window: usize,
}
impl Default for LineChunker {
    fn default() -> Self {
        Self { window: 50 }
    }
}
impl Chunker for LineChunker {
    fn name(&self) -> &str {
        "lines"
    }
    fn chunk(&self, input: &str) -> Vec<String> {
        let lines: Vec<&str> = input.lines().collect();
        if lines.is_empty() {
            return Vec::new();
        }
        lines
            .chunks(self.window.max(1))
            .map(|w| w.join("\n"))
            .collect()
    }
}

/// `paragraph`: split on blank-line boundaries.
struct ParagraphChunker;
impl Chunker for ParagraphChunker {
    fn name(&self) -> &str {
        "paragraph"
    }
    fn chunk(&self, input: &str) -> Vec<String> {
        input
            .split("\n\n")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    }
}

/// Truncate `s` to at most `budget` bytes, never splitting a UTF-8 char.
pub(crate) fn truncate_to_budget(mut s: String, budget: Option<usize>) -> String {
    if let Some(b) = budget {
        if s.len() > b {
            let mut end = b;
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_registered() {
        let reg = ExtensionRegistry::with_builtins();
        assert_eq!(reg.read_mode_names(), vec!["full"]);
        // identity/whitespace built-ins plus the non-code compressors
        // (markdown/prose) from `core::nc_compress` (EPIC 12.14).
        assert_eq!(
            reg.compressor_names(),
            vec!["identity", "markdown", "prose", "whitespace"]
        );
        // Built-in line/paragraph chunkers plus the format-aware chunkers
        // (csv/json/eml/html) registered by `core::extractors` (EPIC 12.13).
        assert_eq!(
            reg.chunker_names(),
            vec!["csv", "eml", "html", "json", "lines", "paragraph"]
        );
    }

    #[test]
    fn whitespace_compressor_collapses_blanks() {
        let reg = ExtensionRegistry::with_builtins();
        let c = reg.compressor("whitespace").unwrap();
        let out = c.compress("a\n\n\n\nb  \n", None);
        assert_eq!(out, "a\n\nb\n");
    }

    #[test]
    fn identity_compressor_honors_budget_on_char_boundary() {
        let reg = ExtensionRegistry::with_builtins();
        let c = reg.compressor("identity").unwrap();
        // 'ä' is two bytes; a 3-byte budget must not split it.
        let out = c.compress("aäb", Some(2));
        assert_eq!(out, "a");
    }

    #[test]
    fn chunkers_split_as_expected() {
        let reg = ExtensionRegistry::with_builtins();
        let para = reg.chunker("paragraph").unwrap();
        assert_eq!(
            para.chunk("one\n\ntwo\n\n\nthree"),
            vec!["one", "two", "three"]
        );
        let lines = reg.chunker("lines").unwrap();
        assert_eq!(lines.chunk("a\nb\nc").len(), 1);
    }

    struct UpperCompressor;
    impl Compressor for UpperCompressor {
        fn name(&self) -> &str {
            "uppercase"
        }
        fn compress(&self, input: &str, _budget: Option<usize>) -> String {
            input.to_uppercase()
        }
    }

    #[test]
    fn extension_can_register_and_run_custom_compressor() {
        let mut reg = ExtensionRegistry::with_builtins();
        reg.register_compressor(Arc::new(UpperCompressor));
        assert!(reg.compressor_names().contains(&"uppercase".to_string()));
        let c = reg.compressor("uppercase").unwrap();
        assert_eq!(c.compress("hi", None), "HI");
    }

    #[test]
    fn global_registry_seeds_builtins() {
        let reg = global().read().unwrap();
        assert!(reg.compressor("identity").is_some());
    }
}
