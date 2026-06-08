//! Adapter: a manifest-declared plugin tool ([`PluginToolSpec`]) presented as a
//! native MCP tool (EPIC 12.11). Registered dynamically in `build_registry()`,
//! so developers add tools by shipping a manifest — never by forking.

use std::sync::Arc;

use rmcp::model::Tool;
use rmcp::ErrorData;
use serde_json::{Map, Value};

use crate::core::plugins::tools::PluginToolSpec;
use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};

/// Native MCP tool backed by a plugin's sandboxed subprocess.
pub struct PluginTool {
    /// Leaked, `'static` registry name (see [`PluginTool::from_spec`]).
    name: &'static str,
    spec: PluginToolSpec,
}

impl PluginTool {
    /// Build a registrable tool from a discovered spec.
    ///
    /// The MCP registry keys tools by `&'static str`. Plugin tools are
    /// discovered once at startup and live for the whole process, so leaking the
    /// name is a bounded, intentional allocation (a handful of tool names).
    #[must_use]
    pub fn from_spec(spec: PluginToolSpec) -> Self {
        let name: &'static str = Box::leak(spec.name.clone().into_boxed_str());
        Self { name, spec }
    }
}

impl McpTool for PluginTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn tool_def(&self) -> Tool {
        let schema: Map<String, Value> = if let Value::Object(map) = &self.spec.input_schema {
            map.clone()
        } else {
            let mut map = Map::new();
            map.insert("type".to_string(), Value::String("object".to_string()));
            map
        };
        let description = if self.spec.description.is_empty() {
            format!("Plugin tool provided by '{}'", self.spec.plugin_name)
        } else {
            self.spec.description.clone()
        };
        Tool::new(self.name, description, Arc::new(schema))
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let args_json = serde_json::to_string(&Value::Object(args.clone()))
            .unwrap_or_else(|_| "{}".to_string());
        match crate::core::plugins::tools::invoke(&self.spec, &args_json) {
            Ok(text) => Ok(ToolOutput::simple(text)),
            Err(e) => Err(ErrorData::internal_error(e, None)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn spec() -> PluginToolSpec {
        PluginToolSpec {
            plugin_name: "weather".into(),
            plugin_dir: PathBuf::from("/tmp"),
            name: "weather_lookup".into(),
            description: "Look up weather".into(),
            command: "cat".into(),
            timeout_ms: 2000,
            input_schema: serde_json::json!({"type": "object"}),
            policy: crate::core::plugins::sandbox::SandboxPolicy::strict(),
        }
    }

    #[test]
    fn tool_def_reflects_spec() {
        let tool = PluginTool::from_spec(spec());
        assert_eq!(tool.name(), "weather_lookup");
        let def = tool.tool_def();
        assert_eq!(def.name.as_ref(), "weather_lookup");
        assert_eq!(def.description.as_deref(), Some("Look up weather"));
    }

    #[test]
    fn missing_description_falls_back() {
        let mut s = spec();
        s.description = String::new();
        let tool = PluginTool::from_spec(s);
        let def = tool.tool_def();
        assert!(def.description.as_deref().unwrap_or("").contains("weather"));
    }
}
