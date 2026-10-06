// SPDX-License-Identifier: Apache-2.0
//! The `ctx_read` MCP tool definition. Split out of `ctx_read.rs` to keep that
//! file under the #660 LOC gate.

use rmcp::model::Tool;
use serde_json::json;

use crate::tool_defs::tool_def;

pub(super) fn ctx_read_tool_def() -> Tool {
    tool_def(
        "ctx_read",
        "Read source files. mode recommended — choose by intent (see `mode` below); defaults to auto when omitted.\n\
         To UNDERSTAND code run ctx_compose FIRST; ctx_read after it identified files.\n\
         anchored → edit by reference via ctx_patch (no exact-recall).",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path" },
                "paths": { "type": "array", "items": { "type": "string" }, "description": "Batch read" },
                "mode": {
                    "type": "string",
                    "description": "Recommended (defaults to auto). full=complete(edit-ready; ≤turn budget, raw=true beyond) anchored=full+N:hh|anchors(edit via ctx_patch) raw=exact-bytes signatures=API map=structure auto=smart diff=cache-delta lines:N-M=window -N=tail 5,10-20=multi reference=quotes task=focus"
                },
                "raw": { "type": "boolean", "description": "Verbatim (= mode=raw + fresh)" },
                "start_line": { "type": "integer", "description": "1-based" },
                "offset": { "type": "integer", "description": "start_line alias" },
                "limit": { "type": "integer", "description": "Max lines" },
                "fresh": { "type": "boolean", "description": "Bypass cache" },
                "aggressiveness": { "type": "number", "description": "0.0–1.0 density (entropy/task)" },
                "protect": { "type": "array", "items": { "type": "string" }, "description": "Symbols kept verbatim" },
                "engine_interface": super::engine::interface_schema()
            },
            "required": []
        }),
    )
}
