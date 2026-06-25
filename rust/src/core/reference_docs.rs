//! Generated reference documentation.
//!
//! Renders Markdown appendices directly from the in-code single sources of
//! truth (the MCP tool registry and the `Config` schema) so the published
//! reference can never drift from the actual feature surface.
//!
//! - `mcp-tools.md`   — every registered MCP tool, from `manifest_value()`.
//! - `config-keys.md` — every recognized `config.toml` key, from `ConfigSchema`.
//!
//! Used by the `gen_docs` example (writes the files) and by drift tests /
//! the CI gate (compare on-disk vs. freshly rendered).

use std::path::PathBuf;

use serde_json::Value;

use crate::core::config::schema::ConfigSchema;

const DO_NOT_EDIT: &str = "<!-- GENERATED FILE — do not edit by hand. Run: `cargo run --example gen_docs --features dev-tools` -->";

/// Directory the generated reference docs live in (`docs/reference/generated`).
#[must_use]
pub fn generated_dir() -> PathBuf {
    let rust_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = rust_dir.parent().unwrap_or(&rust_dir);
    repo_root.join("docs/reference/generated")
}

/// Every generated reference document as `(filename, markdown)` pairs.
/// This is the canonical list shared by the writer and the drift tests.
#[must_use]
pub fn generated_docs() -> Vec<(&'static str, String)> {
    vec![
        ("mcp-tools.md", mcp_tools_markdown()),
        ("config-keys.md", config_keys_markdown()),
    ]
}

/// True when on-disk content equals freshly generated content, ignoring
/// line-ending differences. Windows checkouts may store the committed docs
/// with CRLF while the generator emits LF; the drift gate compares *content*,
/// not byte-exact line endings.
#[must_use]
pub fn content_matches(on_disk: &str, generated: &str) -> bool {
    normalize_newlines(on_disk) == normalize_newlines(generated)
}

fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n")
}

// ---------------------------------------------------------------------------
// MCP tools
// ---------------------------------------------------------------------------

/// Markdown reference for every registered MCP tool (granular profile),
/// rendered from the same manifest the editors consume.
#[must_use]
pub fn mcp_tools_markdown() -> String {
    let manifest = crate::core::mcp_manifest::manifest_value();
    let mut tools: Vec<&Value> = manifest
        .get("tools")
        .and_then(|t| t.get("granular"))
        .and_then(|g| g.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    tools.sort_by(|a, b| tool_name(a).cmp(tool_name(b)));

    let mut out = String::new();
    out.push_str("# Appendix — MCP Tools (generated)\n\n");
    out.push_str(DO_NOT_EDIT);
    out.push_str("\n\n");
    out.push_str(
        "Source of truth: `rust/src/server/registry.rs` and the tool definitions it registers.\n\n",
    );
    out.push_str(&format!(
        "lean-ctx registers **{} MCP tools** (granular profile). Each entry below lists the \
         tool name, what it does, and its parameters (`*` marks required).\n\n",
        tools.len()
    ));

    for tool in tools {
        let name = tool_name(tool);
        out.push_str(&format!("## `{name}`\n\n"));

        let desc = tool
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .trim();
        if !desc.is_empty() {
            out.push_str(desc);
            out.push_str("\n\n");
        }

        let params = render_tool_params(tool);
        if params.is_empty() {
            out.push_str("Parameters: _none_\n\n");
        } else {
            out.push_str(&format!("Parameters: {params}\n\n"));
        }
    }
    out
}

fn tool_name(tool: &Value) -> &str {
    tool.get("name").and_then(|n| n.as_str()).unwrap_or("")
}

/// Render the parameter list of a tool as inline code spans, sorted, with
/// required parameters marked by a trailing `*`.
fn render_tool_params(tool: &Value) -> String {
    let schema = tool.get("input_schema");
    let props = schema
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.as_object());
    let Some(props) = props else {
        return String::new();
    };
    let required: Vec<&str> = schema
        .and_then(|s| s.get("required"))
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut names: Vec<&String> = props.keys().collect();
    names.sort();
    names
        .iter()
        .map(|n| {
            if required.contains(&n.as_str()) {
                format!("`{n}`*")
            } else {
                format!("`{n}`")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Config keys
// ---------------------------------------------------------------------------

/// Markdown reference for every recognized `config.toml` key, rendered from
/// the `Config` schema (types, defaults, allowed values, env overrides).
#[must_use]
pub fn config_keys_markdown() -> String {
    let schema = ConfigSchema::generate();

    let mut out = String::new();
    out.push_str("# Appendix — Configuration Keys (generated)\n\n");
    out.push_str(DO_NOT_EDIT);
    out.push_str("\n\n");
    out.push_str("Source of truth: `rust/src/core/config/schema.rs`.\n\n");
    out.push_str(
        "lean-ctx reads `~/.lean-ctx/config.toml` (and a project `.lean-ctx.toml` overlay). \
         Below is every recognized key with its type, default, and environment-variable \
         override where one exists.\n\n",
    );

    // `root` first (top-level keys), then named sections alphabetically.
    if let Some(root) = schema.sections.get("root") {
        out.push_str("## Top-level keys\n\n");
        if !root.description.trim().is_empty() {
            out.push_str(&format!("{}\n\n", root.description.trim()));
        }
        out.push_str(&render_section_keys(root));
    }

    for (name, section) in &schema.sections {
        if name == "root" {
            continue;
        }
        out.push_str(&format!("## `[{name}]`\n\n"));
        if !section.description.trim().is_empty() {
            out.push_str(&format!("{}\n\n", section.description.trim()));
        }
        let keys = render_section_keys(section);
        if keys.is_empty() {
            out.push_str("_No sub-keys (presence of the section toggles the feature)._\n\n");
        } else {
            out.push_str(&keys);
        }
    }
    out
}

fn render_section_keys(section: &crate::core::config::schema::SectionSchema) -> String {
    let mut out = String::new();
    for (key, ks) in &section.keys {
        let mut ty = ks.ty.clone();
        if let Some(values) = &ks.values {
            ty = format!("{ty}: {}", values.join(" | "));
        }
        let default = value_to_inline(&ks.default);
        let env = ks
            .env_override
            .as_ref()
            .map(|e| format!(" — env `{e}`"))
            .unwrap_or_default();
        let desc = ks.description.trim();
        out.push_str(&format!(
            "- `{key}` ({ty}, default `{default}`{env}) — {desc}\n"
        ));
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// Compact inline rendering of a JSON default value for docs.
fn value_to_inline(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::String(s) if s.is_empty() => "\"\"".to_string(),
        Value::String(s) => s.clone(),
        Value::Array(a) if a.is_empty() => "[]".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_tools_doc_lists_every_registered_tool() {
        let md = mcp_tools_markdown();
        let count = crate::server::registry::tool_count();
        // Every registered tool gets its own `## ` section heading.
        let headings = md.matches("\n## `").count();
        assert!(
            headings >= count,
            "expected at least one heading per tool: {headings} headings for {count} tools"
        );
        // Spot-check a couple of always-present tools.
        assert!(md.contains("## `ctx_read`"), "ctx_read must be documented");
        assert!(
            md.contains("## `ctx_shell`"),
            "ctx_shell must be documented"
        );
    }

    #[test]
    fn config_keys_doc_covers_all_known_keys() {
        let md = config_keys_markdown();
        let schema = ConfigSchema::generate();
        for key in schema.sections.values().flat_map(|s| s.keys.keys()) {
            assert!(
                md.contains(&format!("`{key}`")),
                "config key `{key}` missing from generated doc"
            );
        }
    }

    #[test]
    fn content_matches_ignores_line_endings() {
        // A CRLF checkout (Windows) must still match LF-generated content.
        assert!(content_matches("a\r\nb\r\n", "a\nb\n"));
        assert!(content_matches("a\nb\n", "a\nb\n"));
        // Real content differences still fail.
        assert!(!content_matches("a\nb\n", "a\nc\n"));
    }

    #[test]
    fn generated_docs_are_nonempty_and_named() {
        let docs = generated_docs();
        assert_eq!(docs.len(), 2);
        for (name, body) in docs {
            assert!(
                std::path::Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md")),
                "{name} should be a .md file"
            );
            assert!(body.len() > 100, "{name} should not be trivial");
            assert!(body.contains("GENERATED FILE"), "{name} needs the banner");
        }
    }
}
