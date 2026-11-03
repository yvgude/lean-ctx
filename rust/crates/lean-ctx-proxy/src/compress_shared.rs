// SPDX-License-Identifier: Apache-2.0
//! Provider-neutral classification for content blocks compressed in place.

use serde_json::Value;

use super::tool_kind::{self, ToolResultKind};

/// Semantic type of a provider content block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    ToolUse,
    ToolResult,
    Text,
    Image,
    Other,
}

/// Classify the common content-block types used by provider request shapes.
pub fn classify_tool_kind(content: &Value) -> ToolKind {
    match content.get("type").and_then(Value::as_str) {
        Some("tool_use" | "function_call") => ToolKind::ToolUse,
        Some("tool_result" | "function_call_output") => ToolKind::ToolResult,
        Some("text" | "input_text" | "output_text") => ToolKind::Text,
        Some("image" | "image_url" | "input_image") => ToolKind::Image,
        _ => ToolKind::Other,
    }
}

/// Whether this block kind carries text that may be compressed in place.
pub const fn should_compress_content(kind: ToolKind) -> bool {
    matches!(kind, ToolKind::Text)
}

/// Resolve a tool name to its result kind, preserving the unknown-tool fallback.
pub fn tool_result_kind(tool_name: Option<&str>) -> ToolResultKind {
    tool_name.map_or(ToolResultKind::Other, tool_kind::classify_tool_name)
}
