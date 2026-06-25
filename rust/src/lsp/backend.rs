//! Backend abstraction for LSP-style code intelligence.
//!
//! Two backings implement this trait:
//!   A) `LspClient` (stdio rust-analyzer) — CI/headless fallback, see client.rs
//!   B) `JetBrainsHttpBackend` (in-IDE PSI over HTTP) — preferred, see `jetbrains_backend.rs`
//!
//! The 5 mandatory methods exist in both backings (today's behavior must not break).
//! The default-degrading methods return a clear "unsupported" error unless a backing
//! (Backing B) overrides them.

use lsp_types::{GotoDefinitionResponse, Location, Position, TextEdit, Uri, WorkspaceEdit};

/// Direction for `type_hierarchy` queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HierarchyDirection {
    Subtypes,
    Supertypes,
}

/// A node in a type hierarchy (super/subtype tree).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeHierarchyNode {
    pub name: String,
    /// Project-relative path of the declaring file.
    pub path: String,
    /// 1-indexed line of the declaration.
    pub line: u32,
    pub children: Vec<TypeHierarchyNode>,
}

/// A single symbol entry from a file's structure overview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolOverviewItem {
    pub name: String,
    pub kind: String,
    /// 1-indexed line.
    pub line: u32,
}

/// A single inspection/diagnostic result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectionDiag {
    /// Project-relative path.
    pub path: String,
    /// 1-indexed line.
    pub line: u32,
    pub severity: String,
    pub message: String,
}

/// A single available inspection (the `list` mode of the inspections action).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectionInfo {
    /// Stable short name / id of the inspection tool.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Severity token: ERROR | WARNING | `WEAK_WARNING` | INFO.
    pub severity: String,
}

/// Truncation metadata for capped result sets (Backing B caps; spec Phase 3/4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Truncation {
    pub truncated: bool,
    /// Total available matches/items (≥ returned count when truncated).
    pub total: u32,
}

/// A 0-based, half-open text range (LSP/wire convention: start inclusive, end exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRange0Based {
    pub start_line: u32,
    pub start_char: u32,
    pub end_line: u32,
    pub end_char: u32,
}

/// A resolved, ready-to-apply edit. The `name_path` → range resolution has already
/// happened in `ctx_refactor`; the backend only ever sees an absolute path + range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeEdit {
    /// Absolute, jail-checked path of the file to edit.
    pub abs_path: String,
    /// Project-relative path (for the wire body sent to Backing B).
    pub rel_path: String,
    /// The canonical edit boundary (same in IDE and headless paths).
    pub range: TextRange0Based,
    /// Final text to write into `range` (indentation already baked in by Rust).
    pub text: String,
    /// Optional md5-hex of the current content of `range`; mismatch → CONFLICT.
    pub expected_hash: Option<String>,
}

/// Outcome of applying a `RangeEdit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditResult {
    pub applied: bool,
    /// Range covering the newly written text after the edit.
    pub new_range: TextRange0Based,
    /// The text that now occupies `new_range`.
    pub edited_text: String,
    /// Compact human-readable diff (removed/added lines).
    pub diff: String,
}

/// Query for `rename_preview`: the target symbol is already resolved (`name_path` →
/// range) in `ctx_refactor`; the backend only ever sees an absolute + relative
/// path and a range, exactly like `RangeEdit` (no `name_path` on the wire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameQuery {
    /// Absolute, jail-checked path of the file containing the target symbol.
    pub abs_path: String,
    /// Project-relative path (wire body sent to Backing B).
    pub rel_path: String,
    /// Declaration span of the target symbol (start is what the IDE resolves from).
    pub target_range: TextRange0Based,
    pub new_name: String,
    /// Also rename matches inside comments/strings (`RenameProcessor` flag).
    pub search_comments: bool,
    /// Also rename non-code text occurrences (`RenameProcessor` flag).
    pub search_text_occurrences: bool,
}

/// A single semantic usage of the target symbol (declaration or reference),
/// returned by Backing B's `RenameProcessor.findUsages`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageSite {
    /// Project-relative path of the file holding this usage.
    pub path: String,
    /// 0-based range of the renamed identifier at this site.
    pub range: TextRange0Based,
    /// Optional one-line context snippet (display only; NOT part of `plan_hash`).
    pub context: Option<String>,
}

/// A refactoring conflict surfaced by `RenameProcessor.preprocessUsages`
/// (name collision, visibility loss, override clash). `range` is optional —
/// some conflicts are scope-level, not tied to a single offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub path: String,
    pub range: Option<TextRange0Based>,
    pub message: String,
}

/// Outcome of `rename_preview`: every usage + every conflict. The `plan_hash`
/// is built in Rust from this (see `ctx_refactor::plan_hash`), never here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenamePlan {
    pub usages: Vec<UsageSite>,
    pub conflicts: Vec<Conflict>,
}

/// Apply request: same target addressing as `RenameQuery` plus the `force`
/// flag (passed through to `RenameProcessor`; Rust has already gated conflicts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameApply {
    pub abs_path: String,
    pub rel_path: String,
    pub target_range: TextRange0Based,
    pub new_name: String,
    pub force: bool,
}

/// Outcome of `rename_apply`: which files the IDE actually changed (no per-file
/// bodies — Multi-File would be too large; Rust re-reads via mtime validation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameResult {
    pub applied: bool,
    pub changed_paths: Vec<String>,
}

/// Where a `move` sends the symbol. Mirrors Serena's two-field dispatch
/// (`targetRelativePath` XOR `targetParentNamePath`, spec §3): the caller picks
/// the variant, the backend never sees a `name_path`. Both variants carry the
/// jail-checked `abs_path` plus the wire-facing `rel_path` (rebuilt by the IDE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveTarget {
    /// Move a file/class into a directory or file (`FileMoveProcessor` side).
    Path { abs_path: String, rel_path: String },
    /// Move a member into a parent symbol (`SymbolMoveProcessor` side); `range`
    /// is the parent declaration span used to resolve it in the IDE.
    Parent {
        abs_path: String,
        rel_path: String,
        range: TextRange0Based,
    },
}

/// Phase-1 `move` request: the resolved source span plus an already-resolved,
/// already-jailed target (the trait never resolves a `name_path` or a path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveQuery {
    pub abs_path: String,
    pub rel_path: String,
    pub src_range: TextRange0Based,
    pub target: MoveTarget,
}

/// Phase-2 `move` request: the query plus the `force` flag (Rust already gated
/// `plan_hash` + conflicts before this is built).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveApply {
    pub query: MoveQuery,
    pub force: bool,
}

/// Phase-1 `safe_delete` request: just the resolved source span. `*_preview`
/// returns the remaining (blocking) usages in the reused `RenamePlan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeDeleteQuery {
    pub abs_path: String,
    pub rel_path: String,
    pub src_range: TextRange0Based,
}

/// Phase-2 `safe_delete` request: `force` = Serena's `deleteEvenIfUsed`,
/// `propagate` = delete now-unreferenced dependencies too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeDeleteApply {
    pub query: SafeDeleteQuery,
    pub force: bool,
    pub propagate: bool,
}

/// Phase-1 `inline` request: resolved source span. `keep_definition` maps to the
/// `IntelliJ` inline processors' "inline all and keep declaration" flag (spec §3,
/// Befund 2). The trait never sees a `name_path` — exactly like `move/safe_delete`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineQuery {
    pub abs_path: String,
    pub rel_path: String,
    pub src_range: TextRange0Based,
    pub keep_definition: bool,
}

/// Phase-2 `inline` request. NO `force` field — inline conflicts are partly
/// non-overridable (spec §5.2, Entscheidung 4); the Rust gate is final.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineApply {
    pub query: InlineQuery,
}

/// Reformat scope (spec §5.3): the address is already resolved in `ctx_refactor`;
/// the trait sees only File / Region{range} / Symbol{range}, never a `name_path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReformatScope {
    File,
    Region { range: TextRange0Based },
    Symbol { range: TextRange0Based },
}

/// Single-Phase `reformat` request (spec §5.3): no usages, no `plan_hash`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReformatQuery {
    pub abs_path: String,
    pub rel_path: String,
    pub scope: ReformatScope,
    pub optimize_imports: bool,
}

/// Outcome of `reformat`: which files changed (Single-File in practice). A
/// dedicated type makes "reformat has no usage concept" explicit in the type
/// system (spec §5.4 Empfehlung).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReformatResult {
    pub applied: bool,
    pub changed_paths: Vec<String>,
}

/// Code-intelligence backend. `Send` so instances can live in the global
/// `BACKENDS` cache (`Mutex<HashMap<String, Box<dyn LspBackend>>>`).
pub trait LspBackend: Send {
    // ── Mandatory (both backings) ──
    fn open_file(&mut self, uri: &Uri, language_id: &str, text: &str) -> Result<(), String>;
    fn references(
        &mut self,
        uri: &Uri,
        position: Position,
        scope: &str,
    ) -> Result<Vec<Location>, String>;
    fn definition(
        &mut self,
        uri: &Uri,
        position: Position,
    ) -> Result<GotoDefinitionResponse, String>;
    fn implementations(
        &mut self,
        uri: &Uri,
        position: Position,
        scope: &str,
    ) -> Result<Vec<Location>, String>;
    fn rename(
        &mut self,
        uri: &Uri,
        position: Position,
        new_name: &str,
    ) -> Result<Option<WorkspaceEdit>, String>;

    // ── Default-degrading (Backing B preferred; Backing A keeps the Err) ──
    fn declaration(&mut self, _uri: &Uri, _position: Position) -> Result<Vec<Location>, String> {
        Err("declaration requires the JetBrains backend".to_string())
    }
    fn type_hierarchy(
        &mut self,
        _uri: &Uri,
        _position: Position,
        _direction: HierarchyDirection,
    ) -> Result<TypeHierarchyNode, String> {
        Err("type_hierarchy requires the JetBrains backend".to_string())
    }
    fn symbols_overview(&mut self, uri: &Uri) -> Result<Vec<SymbolOverviewItem>, String> {
        // v2a §5.2: lossless headless default via the tree-sitter symbol index
        // (same source as ctx_symbol/ctx_outline). Backing B overrides with PSI.
        let abs = crate::lsp::client::uri_to_file_path(uri)
            .ok_or_else(|| "symbols_overview: bad uri".to_string())?;
        Ok(crate::lsp::edit_apply::overview_from_index(&abs))
    }
    fn format(&mut self, _uri: &Uri) -> Result<Vec<TextEdit>, String> {
        Err("format requires the JetBrains backend".to_string())
    }
    fn inspections(&mut self, _uri: &Uri) -> Result<Vec<InspectionDiag>, String> {
        Err("inspections requires the JetBrains backend".to_string())
    }
    fn list_inspections(&mut self) -> Result<Vec<InspectionInfo>, String> {
        Err("list_inspections requires the JetBrains backend".to_string())
    }

    /// Replace a symbol's full declaration range with `edit.text`.
    /// DEFAULT = headless local range write; `JetBrainsHttpBackend` overrides.
    fn replace_symbol_body(&mut self, edit: &RangeEdit) -> Result<EditResult, String> {
        crate::lsp::edit_apply::local_range_write(edit)
    }
    /// Insert a new sibling before the anchor symbol (range is zero-width at the
    /// anchor start line; indentation already baked into `edit.text`).
    fn insert_before_symbol(&mut self, edit: &RangeEdit) -> Result<EditResult, String> {
        crate::lsp::edit_apply::local_range_write(edit)
    }
    /// Insert a new sibling after the anchor symbol (range is zero-width at the
    /// line following the anchor; indentation already baked into `edit.text`).
    fn insert_after_symbol(&mut self, edit: &RangeEdit) -> Result<EditResult, String> {
        crate::lsp::edit_apply::local_range_write(edit)
    }

    /// Phase 1 of the Two-Phase rename: resolve all usages + conflicts of the
    /// target symbol. DEFAULT = `Err(BACKEND_REQUIRED)` — there is NO lossless
    /// headless usage search (spec §3); only Backing B (live IDE) overrides this.
    fn rename_preview(&mut self, _req: &RenameQuery) -> Result<RenamePlan, String> {
        Err("BACKEND_REQUIRED: rename requires a running JetBrains IDE".to_string())
    }
    /// Phase 2 of the Two-Phase rename: perform the Multi-File rename as ONE
    /// transaction (one Undo entry). DEFAULT = `Err(BACKEND_REQUIRED)`.
    fn rename_apply(&mut self, _req: &RenameApply) -> Result<RenameResult, String> {
        Err("BACKEND_REQUIRED: rename requires a running JetBrains IDE".to_string())
    }

    /// Phase 1 of the Two-Phase move: resolve all usages + conflicts of the
    /// target at the new location. DEFAULT = `Err(BACKEND_REQUIRED)` (no lossless
    /// headless move; only Backing B overrides — spec §5.5).
    fn move_preview(&mut self, _req: &MoveQuery) -> Result<RenamePlan, String> {
        Err("BACKEND_REQUIRED: move requires a running JetBrains IDE".to_string())
    }
    /// Phase 2 of the Two-Phase move: perform the Multi-File move as ONE Undo
    /// transaction. DEFAULT = `Err(BACKEND_REQUIRED)`.
    fn move_apply(&mut self, _req: &MoveApply) -> Result<RenameResult, String> {
        Err("BACKEND_REQUIRED: move requires a running JetBrains IDE".to_string())
    }
    /// Phase 1 of the Two-Phase safe-delete: report the REMAINING (blocking)
    /// references as `usages`/`conflicts`. DEFAULT = `Err(BACKEND_REQUIRED)`.
    fn safe_delete_preview(&mut self, _req: &SafeDeleteQuery) -> Result<RenamePlan, String> {
        Err("BACKEND_REQUIRED: safe_delete requires a running JetBrains IDE".to_string())
    }
    /// Phase 2 of the Two-Phase safe-delete: delete the symbol (force =
    /// deleteEvenIfUsed) as ONE Undo transaction. DEFAULT = `Err(BACKEND_REQUIRED)`.
    fn safe_delete_apply(&mut self, _req: &SafeDeleteApply) -> Result<RenameResult, String> {
        Err("BACKEND_REQUIRED: safe_delete requires a running JetBrains IDE".to_string())
    }

    /// Phase 1 of the Two-Phase inline: resolve all substitution sites + conflicts.
    /// DEFAULT = `Err(BACKEND_REQUIRED)` — only Backing B (live IDE) overrides (spec §5.4).
    fn inline_preview(&mut self, _req: &InlineQuery) -> Result<RenamePlan, String> {
        Err("BACKEND_REQUIRED: inline requires a running JetBrains IDE".to_string())
    }
    /// Phase 2 of the Two-Phase inline: substitute at every call site as ONE Undo
    /// transaction. Hard refusal (recursive, multiple returns, override) → UNSUPPORTED
    /// at the backend. DEFAULT = `Err(BACKEND_REQUIRED)`.
    fn inline_apply(&mut self, _req: &InlineApply) -> Result<RenameResult, String> {
        Err("BACKEND_REQUIRED: inline requires a running JetBrains IDE".to_string())
    }
    /// Single-Phase reformat (spec §5.3): no preview, no `plan_hash`.
    /// DEFAULT = `Err(BACKEND_REQUIRED)`.
    fn reformat(&mut self, _req: &ReformatQuery) -> Result<ReformatResult, String> {
        Err("BACKEND_REQUIRED: reformat requires a running JetBrains IDE".to_string())
    }

    // ── Self-management (liveness) ──
    /// Whether a cached instance of this backend is no longer valid and must be
    /// evicted + re-selected. Backing A (in-process LSP) is never stale → default `false`.
    /// Backing B overrides: the IDE may have closed/restarted since caching.
    fn is_stale(&self, _project_root: &str) -> bool {
        false
    }
    /// Truncation metadata of the most recent capped call, or `None` (Backing A,
    /// or no capped call yet). Lets `ctx_refactor` surface "(truncated …)".
    fn last_truncation(&self) -> Option<Truncation> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_types_construct_and_clone() {
        let q = RenameQuery {
            abs_path: "/proj/a.rs".into(),
            rel_path: "a.rs".into(),
            target_range: TextRange0Based {
                start_line: 0,
                start_char: 0,
                end_line: 0,
                end_char: 3,
            },
            new_name: "bar".into(),
            search_comments: false,
            search_text_occurrences: false,
        };
        let q2 = q.clone();
        assert_eq!(q2.new_name, "bar");

        let plan = RenamePlan {
            usages: vec![UsageSite {
                path: "a.rs".into(),
                range: TextRange0Based {
                    start_line: 1,
                    start_char: 4,
                    end_line: 1,
                    end_char: 7,
                },
                context: Some("foo()".into()),
            }],
            conflicts: vec![Conflict {
                path: "a.rs".into(),
                range: None,
                message: "name already exists".into(),
            }],
        };
        assert_eq!(plan.usages.len(), 1);
        assert_eq!(plan.conflicts[0].message, "name already exists");

        let apply = RenameApply {
            abs_path: "/proj/a.rs".into(),
            rel_path: "a.rs".into(),
            target_range: q.target_range,
            new_name: "bar".into(),
            force: true,
        };
        let res = RenameResult {
            applied: true,
            changed_paths: vec!["a.rs".into()],
        };
        assert!(apply.force);
        assert!(res.applied);
    }

    #[test]
    fn move_and_safe_delete_types_construct_and_clone() {
        let mt = MoveTarget::Path {
            abs_path: "/proj/app/moved".into(),
            rel_path: "app/moved".into(),
        };
        let mq = MoveQuery {
            abs_path: "/proj/Widget.kt".into(),
            rel_path: "Widget.kt".into(),
            src_range: TextRange0Based {
                start_line: 2,
                start_char: 0,
                end_line: 2,
                end_char: 12,
            },
            target: mt.clone(),
        };
        let ma = MoveApply {
            query: mq.clone(),
            force: true,
        };
        assert_eq!(ma.query.target, mt);

        let parent = MoveTarget::Parent {
            abs_path: "/proj/Other.kt".into(),
            rel_path: "Other.kt".into(),
            range: TextRange0Based {
                start_line: 0,
                start_char: 0,
                end_line: 5,
                end_char: 1,
            },
        };
        assert_ne!(parent, mt);

        let sq = SafeDeleteQuery {
            abs_path: "/proj/Widget.kt".into(),
            rel_path: "Widget.kt".into(),
            src_range: TextRange0Based {
                start_line: 2,
                start_char: 0,
                end_line: 2,
                end_char: 12,
            },
        };
        let sa = SafeDeleteApply {
            query: sq.clone(),
            force: true,
            propagate: false,
        };
        assert_eq!(sa.query, sq);
        assert!(sa.force);
        assert!(!sa.propagate);
    }

    #[test]
    fn inline_and_reformat_types_construct_and_clone() {
        let range = TextRange0Based {
            start_line: 1,
            start_char: 0,
            end_line: 1,
            end_char: 9,
        };
        let iq = InlineQuery {
            abs_path: "/p/Calc.kt".into(),
            rel_path: "Calc.kt".into(),
            src_range: range,
            keep_definition: false,
        };
        assert_eq!(iq.clone(), iq);
        let ia = InlineApply { query: iq.clone() };
        assert!(!ia.clone().query.keep_definition);
        let rq = ReformatQuery {
            abs_path: "/p/M.kt".into(),
            rel_path: "M.kt".into(),
            scope: ReformatScope::File,
            optimize_imports: true,
        };
        assert_eq!(rq.clone(), rq);
        assert!(matches!(
            ReformatQuery {
                scope: ReformatScope::Region { range },
                ..rq.clone()
            }
            .scope,
            ReformatScope::Region { .. }
        ));
        let rr = ReformatResult {
            applied: true,
            changed_paths: vec!["M.kt".into()],
        };
        assert_eq!(rr.clone(), rr);
    }

    #[test]
    fn headless_inline_and_reformat_default_is_backend_required() {
        struct Bare2;
        // minimal LspBackend impl reusing the existing `Bare` pattern (mandatory methods only)
        impl LspBackend for Bare2 {
            fn open_file(&mut self, _u: &Uri, _l: &str, _t: &str) -> Result<(), String> {
                Ok(())
            }
            fn references(
                &mut self,
                _u: &Uri,
                _p: Position,
                _s: &str,
            ) -> Result<Vec<Location>, String> {
                Ok(vec![])
            }
            fn definition(
                &mut self,
                _u: &Uri,
                _p: Position,
            ) -> Result<GotoDefinitionResponse, String> {
                Ok(GotoDefinitionResponse::Array(vec![]))
            }
            fn implementations(
                &mut self,
                _u: &Uri,
                _p: Position,
                _s: &str,
            ) -> Result<Vec<Location>, String> {
                Ok(vec![])
            }
            fn rename(
                &mut self,
                _u: &Uri,
                _p: Position,
                _n: &str,
            ) -> Result<Option<WorkspaceEdit>, String> {
                Ok(None)
            }
        }
        let q = InlineQuery {
            abs_path: "/a".into(),
            rel_path: "a".into(),
            src_range: TextRange0Based {
                start_line: 0,
                start_char: 0,
                end_line: 0,
                end_char: 0,
            },
            keep_definition: false,
        };
        assert!(
            Bare2
                .inline_preview(&q)
                .unwrap_err()
                .contains("BACKEND_REQUIRED")
        );
        assert!(
            Bare2
                .inline_apply(&InlineApply { query: q.clone() })
                .unwrap_err()
                .contains("BACKEND_REQUIRED")
        );
        let rq = ReformatQuery {
            abs_path: "/a".into(),
            rel_path: "a".into(),
            scope: ReformatScope::File,
            optimize_imports: false,
        };
        assert!(
            Bare2
                .reformat(&rq)
                .unwrap_err()
                .contains("BACKEND_REQUIRED")
        );
    }

    #[test]
    fn headless_rename_default_is_backend_required() {
        // HeadlessBackend inherits the Trait default → BACKEND_REQUIRED, no apply.
        let mut be = crate::lsp::edit_apply::HeadlessBackend;
        let q = RenameQuery {
            abs_path: "/x".into(),
            rel_path: "x".into(),
            target_range: TextRange0Based {
                start_line: 0,
                start_char: 0,
                end_line: 0,
                end_char: 1,
            },
            new_name: "y".into(),
            search_comments: false,
            search_text_occurrences: false,
        };
        let err = be.rename_preview(&q).unwrap_err();
        assert!(err.starts_with("BACKEND_REQUIRED"), "got: {err}");
        let a = RenameApply {
            abs_path: "/x".into(),
            rel_path: "x".into(),
            target_range: q.target_range,
            new_name: "y".into(),
            force: false,
        };
        assert!(
            be.rename_apply(&a)
                .unwrap_err()
                .starts_with("BACKEND_REQUIRED")
        );
    }

    #[test]
    fn headless_move_and_safe_delete_default_is_backend_required() {
        // A backend that only implements the mandatory methods inherits the four
        // v2c Err defaults (no lossless headless move/delete — spec §4 inherited §3).
        struct Bare;
        impl LspBackend for Bare {
            fn open_file(&mut self, _u: &lsp_types::Uri, _l: &str, _t: &str) -> Result<(), String> {
                Ok(())
            }
            fn references(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _s: &str,
            ) -> Result<Vec<lsp_types::Location>, String> {
                Ok(vec![])
            }
            fn definition(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
            ) -> Result<lsp_types::GotoDefinitionResponse, String> {
                Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
            }
            fn implementations(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _s: &str,
            ) -> Result<Vec<lsp_types::Location>, String> {
                Ok(vec![])
            }
            fn rename(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _n: &str,
            ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
                Ok(None)
            }
        }
        let mut b = Bare;
        let mq = MoveQuery {
            abs_path: "/p/a.kt".into(),
            rel_path: "a.kt".into(),
            src_range: TextRange0Based {
                start_line: 0,
                start_char: 0,
                end_line: 0,
                end_char: 1,
            },
            target: MoveTarget::Path {
                abs_path: "/p/x".into(),
                rel_path: "x".into(),
            },
        };
        assert!(
            b.move_preview(&mq)
                .unwrap_err()
                .starts_with("BACKEND_REQUIRED")
        );
        assert!(
            b.move_apply(&MoveApply {
                query: mq,
                force: false
            })
            .unwrap_err()
            .starts_with("BACKEND_REQUIRED")
        );
        let sq = SafeDeleteQuery {
            abs_path: "/p/a.kt".into(),
            rel_path: "a.kt".into(),
            src_range: TextRange0Based {
                start_line: 0,
                start_char: 0,
                end_line: 0,
                end_char: 1,
            },
        };
        assert!(
            b.safe_delete_preview(&sq)
                .unwrap_err()
                .starts_with("BACKEND_REQUIRED")
        );
        assert!(
            b.safe_delete_apply(&SafeDeleteApply {
                query: sq,
                force: false,
                propagate: false
            })
            .unwrap_err()
            .starts_with("BACKEND_REQUIRED")
        );
    }
}
