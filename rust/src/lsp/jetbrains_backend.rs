//! Backing B: in-IDE `JetBrains` PSI backend over HTTP/JSON (127.0.0.1).
//! Synchronous (`ureq`) — matches the synchronous `McpTool::handle` path and does
//! not block the Tokio runtime. Phase 1 implements references/definition/
//! implementations; rename + the degrading ops follow in later phases.

use std::time::Duration;

use lsp_types::{GotoDefinitionResponse, Location, Position, Range, Uri, WorkspaceEdit};
use serde_json::Value;

use crate::lsp::backend::{
    EditResult, HierarchyDirection, InspectionDiag, InspectionInfo, LspBackend, RangeEdit,
    SymbolOverviewItem, TextRange0Based, TypeHierarchyNode,
};
use crate::lsp::client::file_path_to_uri;

const REQUEST_TIMEOUT_SECS: u64 = 30;

pub struct JetBrainsHttpBackend {
    base_url: String,
    token: String,
    /// Absolute project root, to rejoin project-relative wire paths.
    project_root: String,
    /// IDE process id from the discovered port file — for cheap staleness checks.
    pid: u32,
    /// IDE listen port — re-compared against the port file to detect restarts.
    port: u16,
    /// Truncation meta of the most recent capped call (references/implementations/
    /// `type_hierarchy/symbols_overview`), surfaced by `ctx_refactor`.
    last_meta: Option<crate::lsp::backend::Truncation>,
}

impl JetBrainsHttpBackend {
    /// Canonicalize the project root ONCE so project-relative wire paths rejoin
    /// byte-identically with the Kotlin side (port-file key = sha256(realpath)[..16]).
    /// Mirrors `port_discovery::project_hash` canonicalization. On error (e.g. path
    /// does not exist), fall back to the raw root with a trailing-slash trim.
    fn canonical_root(project_root: &str) -> String {
        let canonical = std::fs::canonicalize(project_root).map_or_else(
            |_| project_root.to_string(),
            |p| p.to_string_lossy().to_string(),
        );
        canonical
            .strip_suffix('/')
            .unwrap_or(&canonical)
            .to_string()
    }

    #[allow(clippy::needless_pass_by_value)] // public ctor; callers own String
    #[must_use]
    pub fn new(port: u16, token: String, project_root: String, pid: u32) -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            token,
            project_root: Self::canonical_root(&project_root),
            pid,
            port,
            last_meta: None,
        }
    }

    #[cfg(test)]
    fn project_root_for_test(&self) -> &str {
        &self.project_root
    }

    fn post(&self, endpoint: &str, body: &Value) -> Result<Value, String> {
        let url = format!("{}{endpoint}", self.base_url);
        // ureq 3.x + repo convention (NO `json` feature): serialize via serde_json,
        // send raw bytes, read response body as string, parse. Per-request timeout via
        // `.config().timeout_global(..).build()`. Pattern mirrors port_discovery.rs + llm_enhance.rs.
        let payload = serde_json::to_vec(body).map_err(|e| format!("serialize request: {e}"))?;
        let resp = ureq::post(&url)
            .config()
            .timeout_global(Some(Duration::from_secs(REQUEST_TIMEOUT_SECS)))
            .build()
            .header("X-LeanCtx-Token", &self.token)
            .header("Content-Type", "application/json")
            .send(payload.as_slice())
            .map_err(|e| format!("JetBrains backend request to {endpoint} failed: {e}"))?;
        let text = resp
            .into_body()
            .read_to_string()
            .map_err(|e| format!("JetBrains backend: read response: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("JetBrains backend: parse response: {e}"))
    }

    /// Project-relative path → absolute file URI (Rust rejoins, spec §6).
    fn rel_to_uri(&self, rel: &str) -> Option<Uri> {
        let abs = format!("{}/{}", self.project_root, rel);
        file_path_to_uri(&abs).ok()
    }

    fn parse_position(v: &Value) -> Option<Position> {
        let line = v.get("line")?.as_u64()? as u32;
        let character = v.get("character")?.as_u64()? as u32;
        Some(Position { line, character })
    }

    fn parse_locations(&self, v: &Value) -> Vec<Location> {
        v.get("locations")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|loc| {
                        let rel = loc.get("path")?.as_str()?;
                        let uri = self.rel_to_uri(rel)?;
                        let range = loc.get("range")?;
                        let start = Self::parse_position(range.get("start")?)?;
                        let end = Self::parse_position(range.get("end")?)?;
                        Some(Location {
                            uri,
                            range: Range { start, end },
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn parse_type_hierarchy(v: &Value) -> TypeHierarchyNode {
        fn node(v: &Value) -> TypeHierarchyNode {
            TypeHierarchyNode {
                name: v
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string(),
                path: v
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                line: v.get("line").and_then(Value::as_u64).unwrap_or(0) as u32,
                children: v
                    .get("children")
                    .and_then(Value::as_array)
                    .map(|arr| arr.iter().map(node).collect())
                    .unwrap_or_default(),
            }
        }
        v.get("tree").map_or_else(
            || TypeHierarchyNode {
                name: String::new(),
                path: String::new(),
                line: 0,
                children: vec![],
            },
            node,
        )
    }

    fn parse_symbols(v: &Value) -> Vec<SymbolOverviewItem> {
        v.get("symbols")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| {
                        Some(SymbolOverviewItem {
                            name: s.get("name")?.as_str()?.to_string(),
                            kind: s.get("kind")?.as_str()?.to_string(),
                            line: s.get("line")?.as_u64()? as u32,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn parse_inspections(v: &Value) -> Vec<InspectionDiag> {
        v.get("diagnostics")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|d| {
                        Some(InspectionDiag {
                            path: d.get("path")?.as_str()?.to_string(),
                            line: d.get("line")?.as_u64()? as u32,
                            severity: d.get("severity")?.as_str()?.to_string(),
                            message: d.get("message")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn parse_inspection_list(v: &Value) -> Vec<InspectionInfo> {
        v.get("inspections")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|i| {
                        Some(InspectionInfo {
                            id: i.get("id")?.as_str()?.to_string(),
                            name: i.get("name")?.as_str()?.to_string(),
                            severity: i.get("severity")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn parse_truncation(v: &Value, shown: u32) -> Option<crate::lsp::backend::Truncation> {
        let truncated = v.get("truncated").and_then(Value::as_bool)?;
        let total = v
            .get("total")
            .and_then(Value::as_u64)
            .map_or(shown, |n| n as u32);
        Some(crate::lsp::backend::Truncation { truncated, total })
    }

    fn parse_edit_result(v: &Value, fallback_text: &str) -> EditResult {
        let pos = |obj: &Value, key: &str| -> (u32, u32) {
            let p = obj.get(key);
            let line = p
                .and_then(|p| p.get("line"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            let ch = p
                .and_then(|p| p.get("character"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
            (line, ch)
        };
        let nr = v.get("newRange");
        let (sl, sc) = nr.map_or((0, 0), |r| pos(r, "start"));
        let (el, ec) = nr.map_or((0, 0), |r| pos(r, "end"));
        EditResult {
            applied: v.get("applied").and_then(Value::as_bool).unwrap_or(false),
            new_range: TextRange0Based {
                start_line: sl,
                start_char: sc,
                end_line: el,
                end_char: ec,
            },
            edited_text: v
                .get("editedText")
                .and_then(Value::as_str)
                .unwrap_or(fallback_text)
                .to_string(),
            diff: String::new(), // Rust builds the diff in ctx_refactor from old/new
        }
    }

    /// `{path}` request body (file-level ops, no position).
    fn path_body(&self, uri: &Uri) -> Value {
        let abs = crate::lsp::client::uri_to_file_path(uri).unwrap_or_default();
        let rel = abs
            .strip_prefix(&self.project_root)
            .map(|s| s.strip_prefix('/').unwrap_or(s).to_string())
            .unwrap_or(abs);
        serde_json::json!({ "path": rel })
    }

    /// Build the `{path, line, character}` request body. `position` is already
    /// 0-based (LSP convention) — sent verbatim. `uri` → project-relative path.
    fn position_body(&self, uri: &Uri, position: Position) -> Value {
        let abs = crate::lsp::client::uri_to_file_path(uri).unwrap_or_default();
        let rel = abs
            .strip_prefix(&self.project_root)
            .map(|s| s.strip_prefix('/').unwrap_or(s).to_string())
            .unwrap_or(abs);
        serde_json::json!({
            "path": rel,
            "line": position.line,
            "character": position.character,
        })
    }

    /// POST a resolved edit to the plugin and parse the result. The wire range is
    /// the canonical tree-sitter range (byte-identical to the headless path).
    fn post_edit(&self, endpoint: &str, edit: &RangeEdit) -> Result<EditResult, String> {
        let mut body = serde_json::json!({
            "path": edit.rel_path,
            "range": {
                "start": { "line": edit.range.start_line, "character": edit.range.start_char },
                "end":   { "line": edit.range.end_line,   "character": edit.range.end_char },
            },
            "text": edit.text,
        });
        if let Some(h) = &edit.expected_hash {
            body["expected_hash"] = serde_json::json!(h);
        }
        let resp = self.post(endpoint, &body)?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        Ok(Self::parse_edit_result(&resp, &edit.text))
    }

    /// Parse a `{start,end}` range object into `TextRange0Based`.
    fn parse_range0(v: &Value) -> Option<crate::lsp::backend::TextRange0Based> {
        let start = Self::parse_position(v.get("start")?)?;
        let end = Self::parse_position(v.get("end")?)?;
        Some(crate::lsp::backend::TextRange0Based {
            start_line: start.line,
            start_char: start.character,
            end_line: end.line,
            end_char: end.character,
        })
    }

    fn parse_rename_plan(v: &Value) -> crate::lsp::backend::RenamePlan {
        use crate::lsp::backend::{Conflict, RenamePlan, UsageSite};
        let usages = v
            .get("usages")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|u| {
                        Some(UsageSite {
                            path: u.get("path")?.as_str()?.to_string(),
                            range: Self::parse_range0(u.get("range")?)?,
                            context: u.get("context").and_then(Value::as_str).map(String::from),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let conflicts = v
            .get("conflicts")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        Some(Conflict {
                            path: c.get("path")?.as_str()?.to_string(),
                            range: c.get("range").and_then(Self::parse_range0),
                            message: c.get("message")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        RenamePlan { usages, conflicts }
    }

    /// Common `{path, range, new_name}` request body for both rename endpoints.
    fn rename_body(
        rel_path: &str,
        range: crate::lsp::backend::TextRange0Based,
        new_name: &str,
    ) -> Value {
        serde_json::json!({
            "path": rel_path,
            "range": {
                "start": { "line": range.start_line, "character": range.start_char },
                "end":   { "line": range.end_line,   "character": range.end_char },
            },
            "new_name": new_name,
        })
    }

    /// Request body for `/movePreview` + `/moveApply`. `target` mirrors the
    /// `MoveTarget` variant (kind=path → `{path}`, kind=parent → `{path,range}`).
    fn move_body(
        rel_path: &str,
        src_range: crate::lsp::backend::TextRange0Based,
        target: &crate::lsp::backend::MoveTarget,
    ) -> Value {
        use crate::lsp::backend::MoveTarget;
        let target_json = match target {
            MoveTarget::Path { rel_path: tp, .. } => serde_json::json!({
                "kind": "path",
                "path": tp,
            }),
            MoveTarget::Parent {
                rel_path: pp,
                range,
                ..
            } => serde_json::json!({
                "kind": "parent",
                "path": pp,
                "range": {
                    "start": { "line": range.start_line, "character": range.start_char },
                    "end":   { "line": range.end_line,   "character": range.end_char },
                },
            }),
        };
        serde_json::json!({
            "path": rel_path,
            "range": {
                "start": { "line": src_range.start_line, "character": src_range.start_char },
                "end":   { "line": src_range.end_line,   "character": src_range.end_char },
            },
            "target": target_json,
        })
    }

    /// Request body for `/safeDeletePreview` (force/propagate ignored there) +
    /// `/safeDeleteApply`.
    fn safe_delete_body(
        rel_path: &str,
        src_range: crate::lsp::backend::TextRange0Based,
        force: bool,
        propagate: bool,
    ) -> Value {
        serde_json::json!({
            "path": rel_path,
            "range": {
                "start": { "line": src_range.start_line, "character": src_range.start_char },
                "end":   { "line": src_range.end_line,   "character": src_range.end_char },
            },
            "force": force,
            "propagate": propagate,
        })
    }

    /// Parse a `{applied, changed_paths}` apply response (shared by rename/move/
    /// `safe_delete` apply). Error envelopes are handled by the caller.
    fn parse_apply_result(resp: &Value) -> crate::lsp::backend::RenameResult {
        let changed_paths = resp
            .get("changed_paths")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        crate::lsp::backend::RenameResult {
            applied: resp
                .get("applied")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            changed_paths,
        }
    }

    /// Build an error message from a backend error envelope: the structured `code` plus
    /// `": message"` when a non-empty detail message is present (else just the code). Keeps
    /// the code prefix that callers/tests match on while preserving the human-readable detail.
    fn error_from_envelope(err: &Value) -> String {
        let code = err
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("INTERNAL");
        match err.get("message").and_then(Value::as_str) {
            Some(m) if !m.is_empty() => format!("{code}: {m}"),
            _ => code.to_string(),
        }
    }

    /// Request body for `/inlinePreview` + `/inlineApply` (no force — spec §5.2).
    fn inline_body(
        rel_path: &str,
        src_range: crate::lsp::backend::TextRange0Based,
        keep_definition: bool,
    ) -> Value {
        serde_json::json!({
            "path": rel_path,
            "range": {
                "start": { "line": src_range.start_line, "character": src_range.start_char },
                "end":   { "line": src_range.end_line,   "character": src_range.end_char },
            },
            "keep_definition": keep_definition,
        })
    }

    /// Request body for `/reformat`. scope.kind ∈ {file, region, symbol};
    /// region/symbol carry a 0-based range, file omits it.
    fn reformat_body(
        rel_path: &str,
        scope: &crate::lsp::backend::ReformatScope,
        optimize_imports: bool,
    ) -> Value {
        use crate::lsp::backend::ReformatScope;
        let scope_json = match scope {
            ReformatScope::File => serde_json::json!({ "kind": "file" }),
            ReformatScope::Region { range } => serde_json::json!({
                "kind": "region",
                "range": {
                    "start": { "line": range.start_line, "character": range.start_char },
                    "end":   { "line": range.end_line,   "character": range.end_char },
                },
            }),
            ReformatScope::Symbol { range } => serde_json::json!({
                "kind": "symbol",
                "range": {
                    "start": { "line": range.start_line, "character": range.start_char },
                    "end":   { "line": range.end_line,   "character": range.end_char },
                },
            }),
        };
        serde_json::json!({
            "path": rel_path,
            "scope": scope_json,
            "optimize_imports": optimize_imports,
        })
    }

    /// Parse a `{applied, changed_paths}` reformat response.
    fn parse_reformat_result(resp: &Value) -> crate::lsp::backend::ReformatResult {
        let changed_paths = resp
            .get("changed_paths")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| p.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        crate::lsp::backend::ReformatResult {
            applied: resp
                .get("applied")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            changed_paths,
        }
    }
}

impl LspBackend for JetBrainsHttpBackend {
    fn open_file(&mut self, _uri: &Uri, _language_id: &str, _text: &str) -> Result<(), String> {
        // The IDE already has the file in its VFS/index — no explicit open needed.
        Ok(())
    }

    fn references(
        &mut self,
        uri: &Uri,
        position: Position,
        scope: &str,
    ) -> Result<Vec<Location>, String> {
        let mut body = self.position_body(uri, position);
        body["scope"] = serde_json::json!(scope);
        let resp = self.post("/references", &body)?;
        let locs = self.parse_locations(&resp);
        self.last_meta = Self::parse_truncation(&resp, locs.len() as u32);
        Ok(locs)
    }

    fn definition(
        &mut self,
        uri: &Uri,
        position: Position,
    ) -> Result<GotoDefinitionResponse, String> {
        let body = self.position_body(uri, position);
        let resp = self.post("/definition", &body)?;
        Ok(GotoDefinitionResponse::Array(self.parse_locations(&resp)))
    }

    fn implementations(
        &mut self,
        uri: &Uri,
        position: Position,
        scope: &str,
    ) -> Result<Vec<Location>, String> {
        let mut body = self.position_body(uri, position);
        body["scope"] = serde_json::json!(scope);
        let resp = self.post("/implementations", &body)?;
        let locs = self.parse_locations(&resp);
        self.last_meta = Self::parse_truncation(&resp, locs.len() as u32);
        Ok(locs)
    }

    fn declaration(&mut self, uri: &Uri, position: Position) -> Result<Vec<Location>, String> {
        let body = self.position_body(uri, position);
        let resp = self.post("/declaration", &body)?;
        Ok(self.parse_locations(&resp))
    }

    fn type_hierarchy(
        &mut self,
        uri: &Uri,
        position: Position,
        direction: HierarchyDirection,
    ) -> Result<TypeHierarchyNode, String> {
        let mut body = self.position_body(uri, position);
        body["direction"] = serde_json::json!(match direction {
            HierarchyDirection::Supertypes => "supertypes",
            HierarchyDirection::Subtypes => "subtypes",
        });
        let resp = self.post("/type_hierarchy", &body)?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        self.last_meta = Self::parse_truncation(&resp, 0);
        Ok(Self::parse_type_hierarchy(&resp))
    }

    fn symbols_overview(&mut self, uri: &Uri) -> Result<Vec<SymbolOverviewItem>, String> {
        let body = self.path_body(uri);
        let resp = self.post("/symbols_overview", &body)?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        let items = Self::parse_symbols(&resp);
        self.last_meta = Self::parse_truncation(&resp, items.len() as u32);
        Ok(items)
    }

    fn inspections(&mut self, uri: &Uri) -> Result<Vec<InspectionDiag>, String> {
        let body = self.path_body(uri);
        let resp = self.post("/inspections", &body)?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        let diags = Self::parse_inspections(&resp);
        self.last_meta = Self::parse_truncation(&resp, diags.len() as u32);
        Ok(diags)
    }

    fn list_inspections(&mut self) -> Result<Vec<InspectionInfo>, String> {
        let resp = self.post("/list_inspections", &serde_json::json!({ "path": "" }))?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        let items = Self::parse_inspection_list(&resp);
        self.last_meta = Self::parse_truncation(&resp, items.len() as u32);
        Ok(items)
    }

    fn replace_symbol_body(&mut self, edit: &RangeEdit) -> Result<EditResult, String> {
        self.post_edit("/replaceSymbolBody", edit)
    }

    fn insert_before_symbol(&mut self, edit: &RangeEdit) -> Result<EditResult, String> {
        self.post_edit("/insertBeforeSymbol", edit)
    }

    fn insert_after_symbol(&mut self, edit: &RangeEdit) -> Result<EditResult, String> {
        self.post_edit("/insertAfterSymbol", edit)
    }

    fn rename_preview(
        &mut self,
        req: &crate::lsp::backend::RenameQuery,
    ) -> Result<crate::lsp::backend::RenamePlan, String> {
        let mut body = Self::rename_body(&req.rel_path, req.target_range, &req.new_name);
        body["search_comments"] = serde_json::json!(req.search_comments);
        body["search_text_occurrences"] = serde_json::json!(req.search_text_occurrences);
        let resp = self.post("/renamePreview", &body)?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        Ok(Self::parse_rename_plan(&resp))
    }

    fn rename_apply(
        &mut self,
        req: &crate::lsp::backend::RenameApply,
    ) -> Result<crate::lsp::backend::RenameResult, String> {
        let mut body = Self::rename_body(&req.rel_path, req.target_range, &req.new_name);
        body["force"] = serde_json::json!(req.force);
        let resp = self.post("/renameApply", &body)?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        Ok(Self::parse_apply_result(&resp))
    }

    fn move_preview(
        &mut self,
        req: &crate::lsp::backend::MoveQuery,
    ) -> Result<crate::lsp::backend::RenamePlan, String> {
        let body = Self::move_body(&req.rel_path, req.src_range, &req.target);
        let resp = self.post("/movePreview", &body)?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        Ok(Self::parse_rename_plan(&resp))
    }

    fn move_apply(
        &mut self,
        req: &crate::lsp::backend::MoveApply,
    ) -> Result<crate::lsp::backend::RenameResult, String> {
        let mut body = Self::move_body(&req.query.rel_path, req.query.src_range, &req.query.target);
        body["force"] = serde_json::json!(req.force);
        let resp = self.post("/moveApply", &body)?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        Ok(Self::parse_apply_result(&resp))
    }

    fn safe_delete_preview(
        &mut self,
        req: &crate::lsp::backend::SafeDeleteQuery,
    ) -> Result<crate::lsp::backend::RenamePlan, String> {
        let body = Self::safe_delete_body(&req.rel_path, req.src_range, false, false);
        let resp = self.post("/safeDeletePreview", &body)?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        Ok(Self::parse_rename_plan(&resp))
    }

    fn safe_delete_apply(
        &mut self,
        req: &crate::lsp::backend::SafeDeleteApply,
    ) -> Result<crate::lsp::backend::RenameResult, String> {
        let body = Self::safe_delete_body(
            &req.query.rel_path,
            req.query.src_range,
            req.force,
            req.propagate,
        );
        let resp = self.post("/safeDeleteApply", &body)?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        Ok(Self::parse_apply_result(&resp))
    }

    fn inline_preview(
        &mut self,
        req: &crate::lsp::backend::InlineQuery,
    ) -> Result<crate::lsp::backend::RenamePlan, String> {
        let body = Self::inline_body(&req.rel_path, req.src_range, req.keep_definition);
        let resp = self.post("/inlinePreview", &body)?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        Ok(Self::parse_rename_plan(&resp))
    }

    fn inline_apply(
        &mut self,
        req: &crate::lsp::backend::InlineApply,
    ) -> Result<crate::lsp::backend::RenameResult, String> {
        let body = Self::inline_body(
            &req.query.rel_path,
            req.query.src_range,
            req.query.keep_definition,
        );
        let resp = self.post("/inlineApply", &body)?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        Ok(Self::parse_apply_result(&resp))
    }

    fn reformat(
        &mut self,
        req: &crate::lsp::backend::ReformatQuery,
    ) -> Result<crate::lsp::backend::ReformatResult, String> {
        let body = Self::reformat_body(&req.rel_path, &req.scope, req.optimize_imports);
        let resp = self.post("/reformat", &body)?;
        if let Some(err) = resp.get("error") {
            return Err(Self::error_from_envelope(err));
        }
        Ok(Self::parse_reformat_result(&resp))
    }

    fn rename(
        &mut self,
        _uri: &Uri,
        _position: Position,
        _new_name: &str,
    ) -> Result<Option<WorkspaceEdit>, String> {
        // Symbolic edits are v2 (spec §9 v2-Ausblick). Phase 1 skeleton: not yet.
        Err("rename via JetBrains backend is not implemented yet (v2 edit spec)".to_string())
    }

    fn is_stale(&self, project_root: &str) -> bool {
        // Cheap re-check: port file gone, or pid/port changed (IDE restarted),
        // or our cached pid is dead → stale. NO HTTP (health is not pinged per call).
        match crate::lsp::port_discovery::read_port_file(project_root) {
            Some(pf) => {
                pf.pid != self.pid
                    || pf.port != self.port
                    || !crate::lsp::port_discovery::pid_alive(self.pid)
            }
            None => true,
        }
    }

    fn last_truncation(&self) -> Option<crate::lsp::backend::Truncation> {
        self.last_meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Spins up a one-shot TCP server returning a canned HTTP/JSON response,
    /// so we can assert the wire→Location mapping without a real IDE.
    fn mock_once(json_body: &'static str) -> u16 {
        // Advertised request body size, so we can fully drain the request before
        // replying. On Windows, dropping a socket that still holds unconsumed
        // inbound bytes makes the OS RST the connection (os error 10053 / 10054),
        // which aborts the client mid-response: the request line + headers + body
        // must all be read first.
        fn content_length(headers: &[u8]) -> usize {
            let text = String::from_utf8_lossy(headers);
            for line in text.lines() {
                let lower = line.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    return v.trim().parse().unwrap_or(0);
                }
            }
            0
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
                let mut req: Vec<u8> = Vec::with_capacity(2048);
                let mut buf = [0u8; 2048];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                        let body_have = req.len() - (pos + 4);
                        if body_have >= content_length(&req[..pos]) {
                            break; // full request consumed
                        }
                    }
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    json_body.len(),
                    json_body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
                // Half-close and wait for the client to finish reading + close, so
                // the full response reaches it before the socket is dropped.
                let _ = stream.shutdown(std::net::Shutdown::Write);
                let _ = stream.read(&mut buf);
            }
        });
        port
    }

    #[test]
    fn references_parses_wire_locations() {
        let body = r#"{"locations":[{"path":"src/main.rs","range":{"start":{"line":5,"character":13},"end":{"line":5,"character":18}}}]}"#;
        let port = mock_once(body);
        let mut backend = JetBrainsHttpBackend::new(
            port,
            "tok".to_string(),
            "/proj".to_string(),
            std::process::id(),
        );
        let uri = file_path_to_uri("/proj/src/main.rs").unwrap();
        let locs = backend
            .references(
                &uri,
                Position {
                    line: 5,
                    character: 13,
                },
                "project",
            )
            .expect("should parse");
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].range.start.line, 5);
        assert_eq!(locs[0].range.start.character, 13);
        assert!(locs[0].uri.as_str().ends_with("/proj/src/main.rs"));
    }

    #[test]
    fn type_hierarchy_parses_wire_tree() {
        use crate::lsp::backend::HierarchyDirection;
        let body = r#"{"tree":{"name":"Animal","path":"A.kt","line":1,"children":[{"name":"Dog","path":"A.kt","line":2,"children":[]}]},"truncated":false}"#;
        let port = mock_once(body);
        let mut backend = JetBrainsHttpBackend::new(
            port,
            "tok".to_string(),
            "/proj".to_string(),
            std::process::id(),
        );
        let uri = file_path_to_uri("/proj/A.kt").unwrap();
        let tree = backend
            .type_hierarchy(
                &uri,
                Position {
                    line: 0,
                    character: 0,
                },
                HierarchyDirection::Subtypes,
            )
            .expect("should parse");
        assert_eq!(tree.name, "Animal");
        assert_eq!(tree.line, 1);
        assert_eq!(tree.children.len(), 1);
        assert_eq!(tree.children[0].name, "Dog");
        assert_eq!(tree.children[0].path, "A.kt");
    }

    #[test]
    fn symbols_overview_parses_wire_items() {
        let body = r#"{"symbols":[{"name":"Animal","kind":"interface","line":1},{"name":"main","kind":"function","line":9}],"truncated":false,"total":2}"#;
        let port = mock_once(body);
        let mut backend = JetBrainsHttpBackend::new(
            port,
            "tok".to_string(),
            "/proj".to_string(),
            std::process::id(),
        );
        let uri = file_path_to_uri("/proj/A.kt").unwrap();
        let items = backend.symbols_overview(&uri).expect("should parse");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, "interface");
        assert_eq!(items[1].name, "main");
        assert_eq!(items[1].line, 9);
    }

    #[test]
    fn inspections_parses_wire_diags() {
        let body = r#"{"diagnostics":[{"path":"A.kt","line":3,"severity":"WARNING","message":"unused variable"}],"truncated":false,"total":1}"#;
        let port = mock_once(body);
        let mut backend = JetBrainsHttpBackend::new(
            port,
            "tok".to_string(),
            "/proj".to_string(),
            std::process::id(),
        );
        let uri = file_path_to_uri("/proj/A.kt").unwrap();
        let diags = backend.inspections(&uri).expect("should parse");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].path, "A.kt");
        assert_eq!(diags[0].line, 3);
        assert_eq!(diags[0].severity, "WARNING");
        assert_eq!(diags[0].message, "unused variable");
    }

    #[test]
    fn replace_symbol_body_parses_wire_result() {
        let port = mock_once(
            r#"{"applied":true,
                "newRange":{"start":{"line":1,"character":0},"end":{"line":1,"character":3}},
                "editedText":"NEW"}"#,
        );
        let mut be = JetBrainsHttpBackend::new(port, "tok".into(), "/tmp/proj".to_string(), 1234);
        let edit = crate::lsp::backend::RangeEdit {
            abs_path: "/tmp/proj/Foo.kt".into(),
            rel_path: "Foo.kt".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 1,
                start_char: 0,
                end_line: 1,
                end_char: 4,
            },
            text: "NEW".into(),
            expected_hash: None,
        };
        let res = be.replace_symbol_body(&edit).unwrap();
        assert!(res.applied);
        assert_eq!(res.edited_text, "NEW");
        assert_eq!(res.new_range.end_char, 3);
    }

    #[test]
    fn edit_maps_error_envelope_to_err() {
        let port = mock_once(r#"{"error":{"code":"CONFLICT","message":"stale"}}"#);
        let mut be = JetBrainsHttpBackend::new(port, "tok".into(), "/tmp/proj".to_string(), 1234);
        let edit = crate::lsp::backend::RangeEdit {
            abs_path: "/tmp/proj/Foo.kt".into(),
            rel_path: "Foo.kt".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 0,
                end_line: 0,
                end_char: 0,
            },
            text: "x".into(),
            expected_hash: None,
        };
        assert_eq!(
            be.replace_symbol_body(&edit).unwrap_err(),
            "CONFLICT: stale"
        );
    }

    #[test]
    fn inspections_maps_error_envelope_to_err() {
        let body = r#"{"error":{"code":"UNSUPPORTED_LANGUAGE","message":"only kotlin"}}"#;
        let port = mock_once(body);
        let mut backend = JetBrainsHttpBackend::new(
            port,
            "tok".to_string(),
            "/proj".to_string(),
            std::process::id(),
        );
        let uri = file_path_to_uri("/proj/A.kt").unwrap();
        let err = backend.inspections(&uri).expect_err("envelope → Err");
        assert_eq!(err, "UNSUPPORTED_LANGUAGE: only kotlin");
    }

    #[test]
    fn list_inspections_parses_wire_items() {
        let body = r#"{"inspections":[{"id":"UnusedSymbol","name":"Unused declaration","severity":"WARNING"}],"truncated":true,"total":342}"#;
        let port = mock_once(body);
        let mut backend = JetBrainsHttpBackend::new(
            port,
            "tok".to_string(),
            "/proj".to_string(),
            std::process::id(),
        );
        let items = backend.list_inspections().expect("should parse");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "UnusedSymbol");
        assert_eq!(items[0].name, "Unused declaration");
        assert_eq!(items[0].severity, "WARNING");
        let meta = backend.last_truncation().expect("meta recorded");
        assert!(meta.truncated);
        assert_eq!(meta.total, 342);
    }

    #[test]
    fn references_records_truncation_meta() {
        let body = r#"{"locations":[{"path":"a.rs","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}],"truncated":true,"total":742}"#;
        let port = mock_once(body);
        let mut backend = JetBrainsHttpBackend::new(
            port,
            "tok".to_string(),
            "/proj".to_string(),
            std::process::id(),
        );
        let uri = file_path_to_uri("/proj/a.rs").unwrap();
        let _ = backend
            .references(
                &uri,
                Position {
                    line: 0,
                    character: 0,
                },
                "project",
            )
            .unwrap();
        let meta = backend.last_truncation().expect("meta recorded");
        assert!(meta.truncated);
        assert_eq!(meta.total, 742);
    }

    #[test]
    fn is_stale_true_when_no_port_file() {
        // Unlikely root → no port file → cached backend is stale.
        let backend = JetBrainsHttpBackend::new(
            12345,
            "tok".to_string(),
            "/nonexistent/leanctx/proj/xyz".to_string(),
            999_999_999,
        );
        assert!(backend.is_stale("/nonexistent/leanctx/proj/xyz"));
    }

    #[test]
    fn is_stale_false_for_matching_live_pid() {
        let _lock = crate::core::data_dir::test_env_lock();
        // A port file describing THIS process (pid alive) + matching port/token
        // must be considered fresh. We stage a port file via the data-dir env.
        let tmp = std::env::temp_dir().join(format!("leanctx-stale-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let root = tmp.to_string_lossy().to_string();
        // Write a port file at the discovery path for `root`.
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", &tmp);
        let pf_path = crate::lsp::port_discovery::port_file_path(&root).unwrap();
        let pid = std::process::id();
        // Serialize via serde so the path is JSON-escaped. On Windows `root`
        // contains backslashes (C:\...\Temp\...), which are invalid raw JSON string
        // escapes — hand-built JSON would fail to parse and read_port_file would
        // return None, making is_stale wrongly report "stale".
        std::fs::write(
            &pf_path,
            serde_json::json!({
                "port": 4567,
                "token": "tok",
                "pid": pid,
                "project_root": root,
                "ide_version": "x",
            })
            .to_string(),
        )
        .unwrap();
        let backend = JetBrainsHttpBackend::new(4567, "tok".to_string(), root.clone(), pid);
        assert!(
            !backend.is_stale(&root),
            "matching live pid+port must be fresh"
        );
        // Different cached pid → stale even though the file is live.
        let other = JetBrainsHttpBackend::new(4567, "tok".to_string(), root.clone(), pid + 1);
        assert!(other.is_stale(&root), "pid mismatch must be stale");
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn canonical_root_strips_trailing_slash_and_resolves_realpath() {
        // Existing dir with a trailing slash → canonical form has no trailing slash
        // and matches sha2's canonicalize (port_discovery::project_hash parity).
        let tmp = std::env::temp_dir();
        let with_slash = format!("{}/", tmp.to_string_lossy());
        let backend =
            JetBrainsHttpBackend::new(1, "t".to_string(), with_slash.clone(), std::process::id());
        let expected = std::fs::canonicalize(&tmp)
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert_eq!(backend.project_root_for_test(), expected);
        assert!(!backend.project_root_for_test().ends_with('/'));
    }

    #[test]
    fn canonical_root_falls_back_to_raw_for_nonexistent() {
        let raw = "/nonexistent/leanctx/xyz";
        let backend =
            JetBrainsHttpBackend::new(1, "t".to_string(), raw.to_string(), std::process::id());
        assert_eq!(backend.project_root_for_test(), raw);
    }

    #[test]
    fn rename_preview_parses_usages_and_conflicts() {
        let body = r#"{"usages":[
            {"path":"src/a.rs","range":{"start":{"line":5,"character":4},"end":{"line":5,"character":7}},"context":"foo()"},
            {"path":"src/b.rs","range":{"start":{"line":1,"character":0},"end":{"line":1,"character":3}}}
          ],"conflicts":[
            {"path":"src/a.rs","range":{"start":{"line":9,"character":0},"end":{"line":9,"character":3}},"message":"name clash"}
          ]}"#;
        let port = mock_once(body);
        let mut be = JetBrainsHttpBackend::new(port, "tok".into(), "/proj".to_string(), 1234);
        let q = crate::lsp::backend::RenameQuery {
            abs_path: "/proj/src/a.rs".into(),
            rel_path: "src/a.rs".into(),
            target_range: crate::lsp::backend::TextRange0Based {
                start_line: 5,
                start_char: 4,
                end_line: 5,
                end_char: 7,
            },
            new_name: "bar".into(),
            search_comments: false,
            search_text_occurrences: false,
        };
        let plan = be.rename_preview(&q).unwrap();
        assert_eq!(plan.usages.len(), 2);
        assert_eq!(plan.usages[0].path, "src/a.rs");
        assert_eq!(plan.usages[0].context.as_deref(), Some("foo()"));
        assert_eq!(plan.usages[1].context, None);
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].message, "name clash");
    }

    #[test]
    fn rename_preview_maps_error_envelope() {
        let port = mock_once(r#"{"error":{"code":"INDEXING","message":"busy"}}"#);
        let mut be = JetBrainsHttpBackend::new(port, "tok".into(), "/proj".to_string(), 1234);
        let q = crate::lsp::backend::RenameQuery {
            abs_path: "/proj/a.rs".into(),
            rel_path: "a.rs".into(),
            target_range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 0,
                end_line: 0,
                end_char: 1,
            },
            new_name: "y".into(),
            search_comments: false,
            search_text_occurrences: false,
        };
        assert_eq!(be.rename_preview(&q).unwrap_err(), "INDEXING: busy");
    }

    #[test]
    fn rename_apply_parses_changed_paths() {
        let body = r#"{"applied":true,"changed_paths":["src/a.rs","src/b.rs"]}"#;
        let port = mock_once(body);
        let mut be = JetBrainsHttpBackend::new(port, "tok".into(), "/proj".to_string(), 1234);
        let a = crate::lsp::backend::RenameApply {
            abs_path: "/proj/src/a.rs".into(),
            rel_path: "src/a.rs".into(),
            target_range: crate::lsp::backend::TextRange0Based {
                start_line: 5,
                start_char: 4,
                end_line: 5,
                end_char: 7,
            },
            new_name: "bar".into(),
            force: false,
        };
        let res = be.rename_apply(&a).unwrap();
        assert!(res.applied);
        assert_eq!(res.changed_paths, vec!["src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn move_body_path_and_parent_variants() {
        use crate::lsp::backend::{MoveTarget, TextRange0Based};
        let r = TextRange0Based {
            start_line: 2,
            start_char: 0,
            end_line: 2,
            end_char: 12,
        };

        let path_body = JetBrainsHttpBackend::move_body(
            "Widget.kt",
            r,
            &MoveTarget::Path {
                abs_path: "/p/app/moved".into(),
                rel_path: "app/moved".into(),
            },
        );
        assert_eq!(path_body["path"], "Widget.kt");
        assert_eq!(path_body["target"]["kind"], "path");
        assert_eq!(path_body["target"]["path"], "app/moved");
        assert!(path_body["target"].get("range").is_none());

        let pr = TextRange0Based {
            start_line: 0,
            start_char: 0,
            end_line: 5,
            end_char: 1,
        };
        let parent_body = JetBrainsHttpBackend::move_body(
            "Widget.kt",
            r,
            &MoveTarget::Parent {
                abs_path: "/p/Other.kt".into(),
                rel_path: "Other.kt".into(),
                range: pr,
            },
        );
        assert_eq!(parent_body["target"]["kind"], "parent");
        assert_eq!(parent_body["target"]["path"], "Other.kt");
        assert_eq!(parent_body["target"]["range"]["start"]["line"], 0);
        assert_eq!(parent_body["target"]["range"]["end"]["line"], 5);
    }

    #[test]
    fn safe_delete_body_carries_flags() {
        use crate::lsp::backend::TextRange0Based;
        let r = TextRange0Based {
            start_line: 2,
            start_char: 0,
            end_line: 2,
            end_char: 12,
        };
        let body = JetBrainsHttpBackend::safe_delete_body("Widget.kt", r, true, false);
        assert_eq!(body["path"], "Widget.kt");
        assert_eq!(body["range"]["start"]["line"], 2);
        assert_eq!(body["force"], true);
        assert_eq!(body["propagate"], false);
    }

    #[test]
    fn inline_body_carries_keep_definition() {
        let r = crate::lsp::backend::TextRange0Based {
            start_line: 2,
            start_char: 4,
            end_line: 2,
            end_char: 7,
        };
        let body = JetBrainsHttpBackend::inline_body("Calc.kt", r, true);
        assert_eq!(body["path"], "Calc.kt");
        assert_eq!(body["keep_definition"], true);
        assert_eq!(body["range"]["start"]["line"], 2);
    }

    #[test]
    fn reformat_body_encodes_scope_variants() {
        use crate::lsp::backend::{ReformatScope, TextRange0Based};
        let file = JetBrainsHttpBackend::reformat_body("M.kt", &ReformatScope::File, true);
        assert_eq!(file["scope"]["kind"], "file");
        assert_eq!(file["optimize_imports"], true);
        let region = JetBrainsHttpBackend::reformat_body(
            "M.kt",
            &ReformatScope::Region {
                range: TextRange0Based {
                    start_line: 9,
                    start_char: 0,
                    end_line: 19,
                    end_char: 0,
                },
            },
            false,
        );
        assert_eq!(region["scope"]["kind"], "region");
        assert_eq!(region["scope"]["range"]["start"]["line"], 9);
        let sym = JetBrainsHttpBackend::reformat_body(
            "M.kt",
            &ReformatScope::Symbol {
                range: TextRange0Based {
                    start_line: 3,
                    start_char: 0,
                    end_line: 5,
                    end_char: 1,
                },
            },
            false,
        );
        assert_eq!(sym["scope"]["kind"], "symbol");
    }

    #[test]
    fn parse_reformat_result_reads_changed_paths() {
        let v = serde_json::json!({ "applied": true, "changed_paths": ["M.kt"] });
        let r = JetBrainsHttpBackend::parse_reformat_result(&v);
        assert!(r.applied);
        assert_eq!(r.changed_paths, vec!["M.kt".to_string()]);
    }
}
