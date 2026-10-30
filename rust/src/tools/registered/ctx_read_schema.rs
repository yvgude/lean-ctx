// SPDX-License-Identifier: Apache-2.0
//! The `ctx_read` MCP tool definition. Split out of `ctx_read.rs` to keep that
//! file under the #660 LOC gate.

use rmcp::model::Tool;
use serde_json::json;

use crate::tool_defs::tool_def;

pub(super) fn ctx_read_tool_def() -> Tool {
    tool_def(
        "ctx_read",
        "Read files. Omit mode to explore: auto gives small files whole, large code as outline.\n\
         symbol=Name or lines:N-M reads just that part, also for edits.\n\
         To UNDERSTAND code run ctx_compose FIRST.",
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative to project root" },
                "paths": { "type": "array", "items": { "type": "string" }, "description": "Batch read" },
                "mode": {
                    "type": "string",
                    "description": "Default auto. full=complete(edit-ready; ≤turn budget, raw=true beyond) anchored=+N:hh|anchors(line ops) raw=exact-bytes signatures=API map=structure auto=smart diff=cache-delta lines:N-M=window -N=tail 5,10-20=multi reference=pointer task=focus"
                },
                "symbol": { "type": "string", "description": "Name or Class.method: only its lines" },
                "raw": { "type": "boolean", "description": "Verbatim except redaction (= mode=raw + fresh)" },
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
