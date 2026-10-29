use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::core::protocol::append_savings;
use crate::core::tokens::count_tokens;
use crate::core::web::{self, ReadMode, ReadOptions};
use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_int, get_str};
use crate::tool_defs::tool_def;

/// `ctx_url_read` — fetch a web page, PDF, or YouTube video and return
/// compressed, citation-backed context (HTML/PDF→text, transcript flattening,
/// extractive research-compression modes).
pub struct CtxUrlReadTool;

impl McpTool for CtxUrlReadTool {
    fn name(&self) -> &'static str {
        "ctx_url_read"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_url_read",
            "Fetch URL: pages→Markdown; PDF→text; YouTube→transcript; mode=auto best per type.\n\
             mode=facts|quotes for research (claims+confidence). query='topic' focuses extraction.\n\
             GitHub blob/raw URLs auto-resolve to raw file. SSRF-guarded (no private IPs).\n\
             max_tokens=6000; timeout_secs=20 (max 60).\n",
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "http(s) URL (page, PDF, YouTube)" },
                    "mode": {
                        "type": "string",
                        "enum": ["auto", "markdown", "text", "links", "facts", "quotes", "transcript"],
                        "description": "auto|markdown|text|links|facts|quotes|transcript"
                    },
                    "query": { "type": "string", "description": "Focus query (boosts facts/quotes)" },
                    "max_tokens": { "type": "integer", "description": "Token budget" },
                    "max_items": { "type": "integer", "description": "Max items for facts/quotes" },
                    "timeout_secs": { "type": "integer", "description": "Timeout seconds" }
                },
                "required": ["url"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let url = get_str(args, "url")
            .ok_or_else(|| ErrorData::invalid_params("url is required", None))?;

        let mode = match get_str(args, "mode") {
            Some(m) => ReadMode::parse(&m).ok_or_else(|| {
                ErrorData::invalid_params(
                    format!("invalid mode '{m}' (use: auto, markdown, text, links, facts, quotes, transcript)"),
                    None,
                )
            })?,
            None => ReadMode::Auto,
        };

        let query = get_str(args, "query");
        let max_tokens = get_int(args, "max_tokens")
            .map_or(web::DEFAULT_MAX_TOKENS, |n| n.clamp(200, 50_000) as usize);
        let max_items =
            get_int(args, "max_items").map_or(web::DEFAULT_MAX_ITEMS, |n| n.clamp(1, 100) as usize);
        let timeout_secs = get_int(args, "timeout_secs")
            .map_or(web::fetch::DEFAULT_TIMEOUT_SECS, |n| n.clamp(1, 60) as u64);

        let opts = ReadOptions {
            url: &url,
            mode,
            query: query.as_deref(),
            max_tokens,
            max_items,
            timeout_secs,
        };

        // Admission runs twice, on purpose: on the extracted source text
        // BEFORE any mode distils it (`read_url_admitted`), and on the text
        // that is finally delivered, footer included (`admitted`).
        let result = tokio::task::block_in_place(|| {
            web::read_url_admitted(&opts, &admit_extracted).and_then(admitted)
        });

        result.map_err(|e| ErrorData::invalid_params(format!("ctx_url_read failed: {e}"), None))
    }
}

/// Admission of the extracted source text (page Markdown, PDF text,
/// transcript, rendered link list, title), applied by `core::web` before it
/// renders, ranks or summarizes anything (G01). A fetched body is a source
/// that is not a file: project policy on the text, then the gateway.
fn admit_extracted(text: &str) -> Result<String, String> {
    crate::tools::admitted_sources::admit_remote_text(text, "ctx_url_read")
        .map(|admission| admission.text)
}

/// What a receipt trace names as the source of a fetched text: scheme, host
/// and path. Query, fragment and user information can carry a credential no
/// detector knows, so they are never part of the reference.
fn source_reference(url: &str) -> String {
    let end = url.find(['?', '#']).unwrap_or(url.len());
    let plain = &url[..end];
    let Some((scheme, rest)) = plain.split_once("://") else {
        return plain.to_owned();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let host = rest[..authority_end]
        .rsplit_once('@')
        .map_or(&rest[..authority_end], |(_, host)| host);
    format!("{scheme}://{host}{}", &rest[authority_end..])
}

/// The last check before a fetched text becomes a tool result: the text this
/// call delivers, citation footer included, is admitted once more, and only
/// the admitted text is counted, footed and returned. A text that is withheld
/// yields the content-free reason as the tool's error; it never becomes an
/// empty or partial success. A body `core::web` cannot extract as text (an
/// unsupported content type) is refused there, in every mode.
fn admitted(read: web::ReadResult) -> Result<ToolOutput, String> {
    let content =
        crate::tools::admitted_sources::admit_remote_text(&read.content, "ctx_url_read")?.text;
    crate::core::context_admission::capture::note_source_id(&source_reference(&read.final_url));
    let sent = count_tokens(&content);
    let saved = read.original_tokens.saturating_sub(sent);
    let text = append_savings(&content, read.original_tokens, sent);
    Ok(ToolOutput {
        text,
        original_tokens: read.original_tokens,
        saved_tokens: saved,
        mode: Some(read.mode.label().to_string()),
        path: Some(read.final_url),
        changed: false,
        shell_outcome: None,
        content_blocks: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    fn fetched(content: &str) -> web::ReadResult {
        web::ReadResult {
            content: content.to_string(),
            mode: ReadMode::Markdown,
            original_tokens: 400,
            final_url: "https://docs.example/page".to_string(),
        }
    }

    /// G01: the old handler returned the fetched text as it came — a key on
    /// a page reached the model unless the release point caught it, and a
    /// page the project policy blocks was delivered. A clean page keeps its
    /// bytes.
    #[test]
    fn a_fetched_body_is_admitted_before_it_is_returned() {
        let _data = crate::core::data_dir::isolated_data_dir();
        {
            let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
            let clean = "# Guide\n\nInstall the widget, then run it.\n\n[source](https://docs.example/page)";
            let Ok(output) = admitted(fetched(clean)) else {
                panic!("a clean page is delivered");
            };
            let sent = count_tokens(clean);
            assert_eq!(output.text, append_savings(clean, 400, sent));
            assert_eq!(output.path.as_deref(), Some("https://docs.example/page"));

            let keyed = format!("# Setup\n\nexport AWS_ACCESS_KEY_ID={AWS_KEY}\n");
            let Ok(output) = admitted(fetched(&keyed)) else {
                panic!("a page with a maskable value is delivered masked");
            };
            assert!(!output.text.contains(AWS_KEY), "{}", output.text);
            assert!(output.text.contains("# Setup"), "{}", output.text);
        }
        let policy = crate::core::policy::load(
            "name = \"ctx-url-read-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let Err(reason) = admitted(fetched("CONFIDENTIAL\nroadmap-marker for the launch\n")) else {
            panic!("a blocked page must not be delivered");
        };
        assert!(reason.contains("withheld"), "{reason}");
        assert!(!reason.contains("roadmap-marker"), "{reason}");
        // The same decision is taken on the extracted text, before any mode
        // distils it (the hook `core::web::read_url_admitted` applies).
        let reason = admit_extracted("CONFIDENTIAL\nroadmap-marker for the launch\n")
            .expect_err("blocked before distillation");
        assert!(!reason.contains("roadmap-marker"), "{reason}");
        assert_eq!(
            admit_extracted("plain extracted text\n").as_deref(),
            Ok("plain extracted text\n")
        );
    }
}
