// SPDX-License-Identifier: Apache-2.0

use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, get_str_array};
use crate::tools::ctx_doc::{Args, handle};
use lean_ctx::tool_defs::tool_def;

pub struct CtxDocTool;

impl McpTool for CtxDocTool {
    fn name(&self) -> &'static str {
        "ctx_doc"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_doc",
            "Project documentation: pages of sections, bound to code. In the linked Hive vault, else local to this Instance.\n\
             WORKFLOW: action=list or search first, then show slug (outline=true, section=<heading-slug>).\n\
             Reads are offline. action=write slug content='# Title\\n…' creates or revises a page;\n\
             action=edit slug find replace changes one unique stretch; action=note adds an addendum.\n\
             anchors=[file:…] binds a page to code; action=status lists stale pages, confirm re-pins after a check.\n\
             Vault writes queue in the background; local writes save at once as revisions\n\
             (local only: action=history slug, show slug rev=N, move slug to=new, delete slug). Link pages as [text](/slug#heading).",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "description": "list|show|search|status|write|edit|note|confirm|history|move|delete" },
                    "to": { "type": "string", "description": "move: the new slug" },
                    "rev": { "type": "integer", "description": "show/list (local docs): as of this revision" },
                    "anchors": { "type": "array", "items": { "type": "string" }, "description": "write: code the page derives from (file:src/x.rs, dir:src/core, glob:src/**/*.rs, shape:docs); a change makes the page stale" },
                    "slug": { "type": "string", "description": "Page slug, e.g. guide/retry (list: a prefix)" },
                    "section": { "type": "string", "description": "show: one section by its heading slug" },
                    "outline": { "type": "boolean", "description": "show: headings only" },
                    "query": { "type": "string", "description": "search words" },
                    "content": { "type": "string", "description": "write: the whole page markdown; note: the note" },
                    "find": { "type": "string", "description": "edit: exact text that occurs once" },
                    "replace": { "type": "string", "description": "edit: its replacement" },
                    "type": { "type": "string", "description": "write (new page): reference|guide|design|…" },
                    "status": { "type": "string", "description": "write (new page): draft|canonical" }
                },
                "required": ["action"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let owned = |k: &str| get_str(args, k);
        let action = owned("action").unwrap_or_else(|| "list".to_owned());
        let (slug, section, query, content, find, replace, kind, status) = (
            owned("slug"),
            owned("section"),
            owned("query"),
            owned("content"),
            owned("find"),
            owned("replace"),
            owned("type"),
            owned("status"),
        );
        let anchors = get_str_array(args, "anchors").unwrap_or_default();
        let to = owned("to");
        let agent = ctx.agent_id.as_ref().and_then(|agent| {
            crate::server::bounded_lock::read(agent, "ctx_doc agent").and_then(|a| a.clone())
        });
        let call = Args {
            anchors: &anchors,
            action: &action,
            slug: slug.as_deref(),
            section: section.as_deref(),
            query: query.as_deref(),
            content: content.as_deref(),
            find: find.as_deref(),
            replace: replace.as_deref(),
            kind: kind.as_deref(),
            status: status.as_deref(),
            to: to.as_deref(),
            rev: args.get("rev").and_then(Value::as_u64),
            author: agent.map(lean_ctx_doc::local::Author::agent),
            outline: args
                .get("outline")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        Ok(ToolOutput::simple(handle(&ctx.project_root, &call)))
    }
}
