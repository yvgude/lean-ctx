//! Web & research context layer.
//!
//! Turns an arbitrary URL (web page or `YouTube` video) into compressed,
//! citation-backed context for an agent. The flow is:
//!
//! 1. [`url_guard`] validates the URL and blocks SSRF targets.
//! 2. [`fetch`] downloads it (bounded, manual-redirect, SSRF-revalidated) — or
//!    [`youtube`] pulls a transcript for video URLs.
//! 3. [`html_to_text`] renders HTML to clean Markdown.
//! 4. [`distill`] applies the requested research-compression mode.
//! 5. [`citation`] attaches source attribution.
//!
//! The single entry point is [`read_url`]; the [`crate::tools::registered::ctx_url_read`]
//! MCP tool is a thin wrapper over it.

pub mod citation;
pub mod distill;
pub mod feed;
pub mod fetch;
pub mod html_to_text;
pub mod pdf;
pub mod rewrite;
pub mod url_guard;
pub mod youtube;

use crate::core::evidence::Claim;

use citation::Citation;

/// Default token budget for returned content.
pub const DEFAULT_MAX_TOKENS: usize = 6000;
/// Default number of items for `facts` / `quotes` modes.
pub const DEFAULT_MAX_ITEMS: usize = 12;
const MAX_LINKS: usize = 100;

/// How fetched content should be distilled before returning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadMode {
    /// Pick a sensible mode from the content type (Markdown for pages,
    /// transcript summary for videos).
    Auto,
    /// Clean Markdown of the main content.
    Markdown,
    /// Plain text (Markdown decorations stripped).
    Text,
    /// Extracted hyperlinks.
    Links,
    /// Sentences carrying factual signals.
    Facts,
    /// Central / query-relevant sentences as evidence.
    Quotes,
    /// De-duplicated, filler-stripped transcript (best for videos).
    Transcript,
}

impl ReadMode {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "markdown" | "md" => Some(Self::Markdown),
            "text" | "plain" => Some(Self::Text),
            "links" => Some(Self::Links),
            "facts" => Some(Self::Facts),
            "quotes" => Some(Self::Quotes),
            "transcript" | "summary" => Some(Self::Transcript),
            _ => None,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Markdown => "markdown",
            Self::Text => "text",
            Self::Links => "links",
            Self::Facts => "facts",
            Self::Quotes => "quotes",
            Self::Transcript => "transcript",
        }
    }
}

/// Parameters for [`read_url`].
pub struct ReadOptions<'a> {
    pub url: &'a str,
    pub mode: ReadMode,
    pub query: Option<&'a str>,
    pub max_tokens: usize,
    pub max_items: usize,
    pub timeout_secs: u64,
}

impl<'a> ReadOptions<'a> {
    #[must_use]
    pub fn new(url: &'a str) -> Self {
        Self {
            url,
            mode: ReadMode::Auto,
            query: None,
            max_tokens: DEFAULT_MAX_TOKENS,
            max_items: DEFAULT_MAX_ITEMS,
            timeout_secs: fetch::DEFAULT_TIMEOUT_SECS,
        }
    }
}

/// Result of a successful [`read_url`].
pub struct ReadResult {
    /// Distilled content with the citation footer already appended.
    pub content: String,
    /// Effective mode after `Auto` resolution.
    pub mode: ReadMode,
    /// Token count of the raw fetched payload (for savings accounting).
    pub original_tokens: usize,
    pub final_url: String,
}

/// Fetch and distill a URL into citation-backed context.
pub fn read_url(opts: &ReadOptions) -> Result<ReadResult, String> {
    // Rewrite known page URLs (e.g. GitHub blob → raw) to their clean-content
    // equivalent before fetching, so the agent gets the file instead of chrome.
    let rewritten = rewrite::rewrite_url(opts.url);
    let url = rewritten.as_deref().unwrap_or(opts.url);

    if let Some(id) = youtube::video_id(url) {
        return read_youtube(&id, opts);
    }

    let effective = ReadOptions {
        url,
        mode: opts.mode,
        query: opts.query,
        max_tokens: opts.max_tokens,
        max_items: opts.max_items,
        timeout_secs: opts.timeout_secs,
    };
    read_web(&effective)
}

fn read_web(opts: &ReadOptions) -> Result<ReadResult, String> {
    let doc = fetch::fetch(opts.url, fetch::DEFAULT_MAX_BYTES, opts.timeout_secs)?;
    if doc.status >= 400 {
        return Err(format!("HTTP {} from {}", doc.status, doc.final_url));
    }

    let is_pdf = doc.content_type.contains("pdf")
        || (doc.content_type.is_empty() && pdf::looks_like_pdf(&doc.bytes));

    let (title, markdown, links, original_tokens) = if is_pdf {
        let text = pdf::extract_text(&doc.bytes)?;
        let tokens = crate::core::tokens::count_tokens(&text);
        (None, text, Vec::new(), tokens)
    } else {
        let body = doc.body_text();
        let tokens = crate::core::tokens::count_tokens(&body);
        let looks_html = body.trim_start().starts_with('<');
        // RSS/Atom feeds are XML, so check them before the HTML branch (which
        // would otherwise flatten a feed into unreadable text — GH #feedback).
        if feed::looks_like_feed(&doc.content_type, &body) {
            let parsed = feed::parse(&body, &doc.final_url);
            (parsed.title, parsed.markdown, Vec::new(), tokens)
        } else if is_html(&doc.content_type) || (doc.content_type.is_empty() && looks_html) {
            let parsed = html_to_text::parse(&body);
            (parsed.title, parsed.markdown, parsed.links, tokens)
        } else if is_textual(&doc.content_type) {
            (None, body, Vec::new(), tokens)
        } else {
            return Err(format!(
                "unsupported content type '{}' for {} (extractable: HTML, PDF, plain text)",
                doc.content_type, doc.final_url
            ));
        }
    };

    let effective = match opts.mode {
        ReadMode::Auto => ReadMode::Markdown,
        other => other,
    };

    let body = render_mode(effective, &markdown, &links, &doc.final_url, opts);
    let trimmed = enforce_budget(&body, opts.max_tokens);
    let citation = Citation::new(&doc.final_url, title);

    Ok(ReadResult {
        content: format!("{trimmed}{}", citation.footer()),
        mode: effective,
        original_tokens,
        final_url: doc.final_url,
    })
}

fn read_youtube(video_id: &str, opts: &ReadOptions) -> Result<ReadResult, String> {
    let transcript = youtube::fetch_transcript(video_id, opts.timeout_secs)?;
    let original_tokens = crate::core::tokens::count_tokens(&transcript.full_text);

    let effective = match opts.mode {
        ReadMode::Auto => ReadMode::Transcript,
        other => other,
    };

    let body = match effective {
        ReadMode::Facts => render_facts(&claims_from(
            distill::facts_scored(&transcript.full_text, opts.query, opts.max_items),
            &transcript.source_url,
        )),
        ReadMode::Quotes => render_quotes(&claims_from(
            distill::quotes_scored(&transcript.full_text, opts.query, opts.max_items),
            &transcript.source_url,
        )),
        ReadMode::Links => "Links are not available for video transcripts.".to_string(),
        _ => distill::summarize_prose(
            &transcript.full_text,
            opts.max_tokens.saturating_mul(4),
            opts.query,
        ),
    };

    let trimmed = enforce_budget(&body, opts.max_tokens);
    let citation = Citation::new(&transcript.source_url, transcript.title);

    Ok(ReadResult {
        content: format!("{trimmed}{}", citation.footer()),
        mode: effective,
        original_tokens,
        final_url: transcript.source_url,
    })
}

fn render_mode(
    mode: ReadMode,
    markdown: &str,
    links: &[html_to_text::Link],
    base_url: &str,
    opts: &ReadOptions,
) -> String {
    match mode {
        ReadMode::Markdown | ReadMode::Auto => markdown.to_string(),
        ReadMode::Text => html_to_text::markdown_to_text(markdown),
        ReadMode::Links => render_links(links, base_url),
        ReadMode::Facts => {
            let plain = html_to_text::markdown_to_text(markdown);
            let claims = claims_from(
                distill::facts_scored(&plain, opts.query, opts.max_items),
                base_url,
            );
            render_facts(&claims)
        }
        ReadMode::Quotes => {
            let plain = html_to_text::markdown_to_text(markdown);
            let claims = claims_from(
                distill::quotes_scored(&plain, opts.query, opts.max_items),
                base_url,
            );
            render_quotes(&claims)
        }
        ReadMode::Transcript => {
            let plain = html_to_text::markdown_to_text(markdown);
            distill::summarize_prose(&plain, opts.max_tokens.saturating_mul(4), opts.query)
        }
    }
}

fn render_links(links: &[html_to_text::Link], base_url: &str) -> String {
    if links.is_empty() {
        return "No links found.".to_string();
    }
    let base = url_guard::validate(base_url).ok();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for link in links {
        let abs = absolutize(&link.href, base.as_ref());
        if seen.insert(abs.clone()) {
            out.push(format!("- [{}]({abs})", link.text));
            if out.len() >= MAX_LINKS {
                break;
            }
        }
    }
    out.join("\n")
}

fn absolutize(href: &str, base: Option<&url_guard::SafeUrl>) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    match base {
        Some(b) => fetch::resolve_redirect(b, href),
        None => href.to_string(),
    }
}

/// Build attributable claims from scored sentences, tagging each with `source`.
fn claims_from(scored: Vec<(String, f32)>, source: &str) -> Vec<Claim> {
    scored
        .into_iter()
        .map(|(text, conf)| Claim::new(text, conf).with_source(source))
        .collect()
}

/// Render facts as a confidence-prefixed bullet list. The shared source lives in
/// the citation footer, so it is not repeated per line (token-lean).
fn render_facts(claims: &[Claim]) -> String {
    if claims.is_empty() {
        return "No matching content found.".to_string();
    }
    claims
        .iter()
        .map(|c| format!("- ({:.2}) {}", c.confidence, c.text))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_quotes(claims: &[Claim]) -> String {
    if claims.is_empty() {
        return "No quotable content found.".to_string();
    }
    claims
        .iter()
        .map(|c| format!("> ({:.2}) {}", c.confidence, c.text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn enforce_budget(content: &str, max_tokens: usize) -> String {
    let tokens = crate::core::tokens::count_tokens(content);
    if tokens <= max_tokens {
        return content.to_string();
    }
    let total_chars = content.chars().count();
    let ratio = max_tokens as f64 / tokens as f64;
    let keep = ((total_chars as f64 * ratio) as usize).max(1);
    let truncated: String = content.chars().take(keep).collect();
    format!("{truncated}\n\n…[truncated to fit ~{max_tokens} token budget]")
}

fn is_html(content_type: &str) -> bool {
    content_type.contains("html") || content_type.contains("xml")
}

fn is_textual(content_type: &str) -> bool {
    content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("markdown")
        || content_type.contains("plain")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_mode_parsing_is_lenient() {
        assert_eq!(ReadMode::parse("MD"), Some(ReadMode::Markdown));
        assert_eq!(ReadMode::parse(" transcript "), Some(ReadMode::Transcript));
        assert_eq!(ReadMode::parse("summary"), Some(ReadMode::Transcript));
        assert_eq!(ReadMode::parse("bogus"), None);
    }

    #[test]
    fn content_type_classification() {
        assert!(is_html("text/html"));
        assert!(is_html("application/xhtml+xml"));
        assert!(is_textual("text/plain"));
        assert!(is_textual("application/json"));
        assert!(!is_html("application/pdf"));
        assert!(!is_textual("application/pdf"));
    }

    #[test]
    fn claim_renderers_handle_empty_and_confidence() {
        assert_eq!(render_facts(&[]), "No matching content found.");
        assert_eq!(render_quotes(&[]), "No quotable content found.");

        let claims = claims_from(
            vec![("Alpha".to_string(), 0.9), ("Beta".to_string(), 0.5)],
            "https://src.example/page",
        );
        assert_eq!(render_facts(&claims), "- (0.90) Alpha\n- (0.50) Beta");
        assert_eq!(
            claims[0].source_url.as_deref(),
            Some("https://src.example/page")
        );
    }

    #[test]
    fn render_links_absolutizes_and_dedupes() {
        let links = vec![
            html_to_text::Link {
                text: "rel".into(),
                href: "/about".into(),
            },
            html_to_text::Link {
                text: "abs".into(),
                href: "https://y.com/z".into(),
            },
            html_to_text::Link {
                text: "dup".into(),
                href: "https://y.com/z".into(),
            },
        ];
        let out = render_links(&links, "https://x.com/dir/page");
        assert!(out.contains("[rel](https://x.com/about)"));
        assert!(out.contains("[abs](https://y.com/z)"));
        assert_eq!(out.matches("https://y.com/z").count(), 1);
    }

    #[test]
    fn enforce_budget_truncates_when_over() {
        let big = "word ".repeat(5000);
        let out = enforce_budget(&big, 50);
        assert!(out.contains("[truncated"));
        assert!(crate::core::tokens::count_tokens(&out) < crate::core::tokens::count_tokens(&big));
    }

    #[test]
    fn enforce_budget_keeps_small_content() {
        let small = "short content";
        assert_eq!(enforce_budget(small, 1000), small);
    }
}
