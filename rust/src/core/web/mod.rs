//! Web & research context layer.
//!
//! Turns an arbitrary URL (web page or YouTube video) into compressed,
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

/// Admission of extracted source text: returns the text that may be used, or
/// a content-free reason why none of it may. See [`read_url_admitted`].
pub type AdmitText<'a> = &'a dyn Fn(&str) -> Result<String, String>;

/// Fetch and distill a URL into citation-backed context.
///
/// No admission is applied to the extracted text; an agent-facing caller
/// uses [`read_url_admitted`].
pub fn read_url(opts: &ReadOptions) -> Result<ReadResult, String> {
    read_url_admitted(opts, &|text| Ok(text.to_string()))
}

/// [`read_url`] with admission BEFORE distillation.
///
/// `admit` is applied to the extracted source text — the page's Markdown, a
/// PDF's text, a transcript — before any mode renders, ranks or summarizes
/// it, and to every other piece of page content that reaches the result (the
/// rendered link list, the title in the citation footer). Facts, quotes,
/// summaries and plain text are therefore derived only from admitted text: a
/// marking on the page cannot be dropped by the distillation before it is
/// seen, and a masked value cannot reappear in a derived line. A refusal ends
/// the read with `admit`'s reason; nothing of the page is returned. The token
/// count of the raw payload stays a number only.
pub fn read_url_admitted(opts: &ReadOptions, admit: AdmitText<'_>) -> Result<ReadResult, String> {
    // Rewrite known page URLs (e.g. GitHub blob → raw) to their clean-content
    // equivalent before fetching, so the agent gets the file instead of chrome.
    let rewritten = rewrite::rewrite_url(opts.url);
    let url = rewritten.as_deref().unwrap_or(opts.url);

    if let Some(id) = youtube::video_id(url) {
        let transcript = youtube::fetch_transcript(&id, opts.timeout_secs)?;
        return distil_transcript(
            &transcript.full_text,
            transcript.title,
            transcript.source_url,
            opts,
            admit,
        );
    }

    let effective = ReadOptions {
        url,
        mode: opts.mode,
        query: opts.query,
        max_tokens: opts.max_tokens,
        max_items: opts.max_items,
        timeout_secs: opts.timeout_secs,
    };
    read_web(&effective, admit)
}

/// What extraction produced from one fetched document, before any admission.
struct Extracted {
    title: Option<String>,
    markdown: String,
    links: Vec<html_to_text::Link>,
    original_tokens: usize,
}

fn read_web(opts: &ReadOptions, admit: AdmitText<'_>) -> Result<ReadResult, String> {
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

    distil_page(
        Extracted {
            title,
            markdown,
            links,
            original_tokens,
        },
        doc.final_url,
        opts,
        admit,
    )
}

/// A title is page content too. One that is not admitted is left out of the
/// citation; the body's own admission has already decided about the page.
fn admitted_title(title: Option<String>, admit: AdmitText<'_>) -> Option<String> {
    title.and_then(|title| admit(&title).ok())
}

/// Admit an extracted page, then distil it. Everything a mode renders is
/// derived from the admitted Markdown; the link list, which extraction took
/// from the raw document, is admitted as rendered.
fn distil_page(
    extracted: Extracted,
    final_url: String,
    opts: &ReadOptions,
    admit: AdmitText<'_>,
) -> Result<ReadResult, String> {
    let markdown = admit(&extracted.markdown)?;

    let effective = match opts.mode {
        ReadMode::Auto => ReadMode::Markdown,
        other => other,
    };

    let body = render_mode(effective, &markdown, &extracted.links, &final_url, opts);
    let body = if effective == ReadMode::Links {
        admit(&body)?
    } else {
        body
    };
    let body = apply_persona_compressor(body, effective);
    let trimmed = enforce_budget(&body, opts.max_tokens);
    let citation = Citation::new(&final_url, admitted_title(extracted.title, admit));

    Ok(ReadResult {
        content: format!("{trimmed}{}", citation.footer()),
        mode: effective,
        original_tokens: extracted.original_tokens,
        final_url,
    })
}

/// Admit a transcript, then distil it: facts, quotes and the summary are
/// derived from the admitted text only.
fn distil_transcript(
    full_text: &str,
    title: Option<String>,
    source_url: String,
    opts: &ReadOptions,
    admit: AdmitText<'_>,
) -> Result<ReadResult, String> {
    let original_tokens = crate::core::tokens::count_tokens(full_text);
    let full_text = admit(full_text)?;

    let effective = match opts.mode {
        ReadMode::Auto => ReadMode::Transcript,
        other => other,
    };

    let body = match effective {
        ReadMode::Facts => render_facts(&claims_from(
            distill::facts_scored(&full_text, opts.query, opts.max_items),
            &source_url,
        )),
        ReadMode::Quotes => render_quotes(&claims_from(
            distill::quotes_scored(&full_text, opts.query, opts.max_items),
            &source_url,
        )),
        ReadMode::Links => "Links are not available for video transcripts.".to_string(),
        _ => distill::summarize_prose(&full_text, opts.max_tokens.saturating_mul(4), opts.query),
    };

    let body = apply_persona_compressor(body, effective);
    let trimmed = enforce_budget(&body, opts.max_tokens);
    let citation = Citation::new(&source_url, admitted_title(title, admit));

    Ok(ReadResult {
        content: format!("{trimmed}{}", citation.footer()),
        mode: effective,
        original_tokens,
        final_url: source_url,
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

/// persona-spec-v1 — apply the active persona's registry compressor to
/// flowing-text modes (`research` → `markdown`, `support`/`lead-gen` →
/// `prose`). Extractive modes (facts/quotes/links) stay verbatim: their lines
/// are claims and citations whose wording must not be rewritten. `identity`
/// (the `coding` default) is a guaranteed no-op and skips the registry lookup.
fn apply_persona_compressor(body: String, mode: ReadMode) -> String {
    if !matches!(
        mode,
        ReadMode::Auto | ReadMode::Markdown | ReadMode::Text | ReadMode::Transcript
    ) {
        return body;
    }
    let name = crate::core::persona::active().compressor;
    if name == "identity" {
        return body;
    }
    let Some(compressor) = crate::core::extension_registry::global()
        .read()
        .ok()
        .and_then(|reg| reg.compressor(&name))
    else {
        tracing::debug!("persona compressor '{name}' not registered; passing through");
        return body;
    };
    compressor.compress(&body, None)
}

/// The budgeted body; a cut names the archived original (K25).
fn enforce_budget(content: &str, max_tokens: usize) -> String {
    let cut = cut_to_budget(content, max_tokens);
    if cut == content {
        return cut;
    }
    crate::core::recovery::with_original_pointer("ctx_url_read", "budget", content, cut)
}

fn cut_to_budget(content: &str, max_tokens: usize) -> String {
    let tokens = crate::core::tokens::count_tokens(content);
    if tokens <= max_tokens {
        return content.to_string();
    }
    // persona-spec-v1 — cut at the persona chunker's boundaries (paragraphs
    // for research/support/lead-gen, line windows for coding/data-analysis)
    // so the truncation lands between units of meaning, not mid-sentence.
    if let Some(trimmed) = trim_at_chunk_boundaries(content, max_tokens) {
        return trimmed;
    }
    // Fallback: proportional character cut (chunker unavailable, content is a
    // single chunk, or the first chunk alone exceeds the budget).
    let total_chars = content.chars().count();
    let ratio = max_tokens as f64 / tokens as f64;
    let keep = ((total_chars as f64 * ratio) as usize).max(1);
    let truncated: String = content.chars().take(keep).collect();
    format!("{truncated}\n\n…[truncated to fit ~{max_tokens} token budget]")
}

/// Trim `content` to whole persona-chunker chunks within `max_tokens`.
///
/// Each kept chunk is located back in the original text (chunkers may trim
/// separators), so the cut always lands on real source bytes. Returns `None`
/// when the chunker yields fewer than two chunks, a chunk cannot be located
/// verbatim (e.g. a transforming format chunker), or not even the first chunk
/// fits — callers then fall back to the proportional character cut.
fn trim_at_chunk_boundaries(content: &str, max_tokens: usize) -> Option<String> {
    use crate::core::tokens::count_tokens;
    let name = crate::core::persona::active().chunker;
    let chunker = crate::core::extension_registry::global()
        .read()
        .ok()?
        .chunker(&name)?;
    let chunks = chunker.chunk(content);
    if chunks.len() < 2 {
        return None;
    }
    let mut cut = 0usize;
    let mut used = 0usize;
    let mut search_from = 0usize;
    for chunk in &chunks {
        let chunk_tokens = count_tokens(chunk);
        if used + chunk_tokens > max_tokens {
            break;
        }
        let rel = content.get(search_from..)?.find(chunk.as_str())?;
        let end = search_from + rel + chunk.len();
        used += chunk_tokens;
        cut = end;
        search_from = end;
    }
    if cut == 0 {
        return None;
    }
    Some(format!(
        "{}\n\n…[truncated to fit ~{max_tokens} token budget]",
        content[..cut].trim_end()
    ))
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

    fn page(markdown: &str) -> Extracted {
        Extracted {
            title: Some("Launch plan".to_string()),
            markdown: markdown.to_string(),
            links: vec![html_to_text::Link {
                text: "internal roadmap".into(),
                href: "https://intra.example/roadmap".into(),
            }],
            original_tokens: 100,
        }
    }

    fn options(mode: ReadMode) -> ReadOptions<'static> {
        ReadOptions {
            mode,
            ..ReadOptions::new("https://docs.example/page")
        }
    }

    const ALL_MODES: [ReadMode; 7] = [
        ReadMode::Auto,
        ReadMode::Markdown,
        ReadMode::Text,
        ReadMode::Links,
        ReadMode::Facts,
        ReadMode::Quotes,
        ReadMode::Transcript,
    ];

    /// G01: distillation used to run on the raw extracted text, so a mode
    /// that drops lines (facts, quotes, summary, links) could drop the very
    /// marking an admission after it would have needed. Admission now runs
    /// first, in every mode, on pages and on transcripts.
    #[test]
    fn a_refused_source_yields_nothing_in_any_mode() {
        let refuse = |text: &str| -> Result<String, String> {
            if text.contains("CONFIDENTIAL") {
                Err("source content withheld by policy".to_string())
            } else {
                Ok(text.to_string())
            }
        };
        let source = "CONFIDENTIAL\n\nThe launch moves to 14 March 2027. Revenue grew 40% in Q3.";
        for mode in ALL_MODES {
            let Err(reason) = distil_page(
                page(source),
                "https://docs.example/page".to_string(),
                &options(mode),
                &refuse,
            ) else {
                panic!("mode {} delivered a refused page", mode.label());
            };
            assert_eq!(reason, "source content withheld by policy");
            let Err(reason) = distil_transcript(
                source,
                None,
                "https://youtu.be/x".to_string(),
                &options(mode),
                &refuse,
            ) else {
                panic!("mode {} delivered a refused transcript", mode.label());
            };
            assert_eq!(reason, "source content withheld by policy");
        }
    }

    /// Every derived part comes from the admitted text: a value admission
    /// masks appears in no mode's output, the link list and the title are
    /// admitted too, and an identity admission changes nothing.
    #[test]
    fn distilled_parts_derive_only_from_admitted_text() {
        let mask = |text: &str| -> Result<String, String> {
            if text == "Launch plan" {
                return Err("withheld".to_string());
            }
            Ok(text
                .replace("ORBITAL-7", "[masked]")
                .replace("intra.example", "[masked]"))
        };
        let identity = |text: &str| -> Result<String, String> { Ok(text.to_string()) };
        let source = "Project ORBITAL-7 ships on 14 March 2027. Revenue grew 40% in Q3 2026.";
        for mode in ALL_MODES {
            let masked = distil_page(
                page(source),
                "https://docs.example/page".to_string(),
                &options(mode),
                &mask,
            )
            .unwrap();
            assert!(!masked.content.contains("ORBITAL-7"), "{}", mode.label());
            assert!(
                !masked.content.contains("intra.example"),
                "{}",
                mode.label()
            );
            assert!(!masked.content.contains("Launch plan"), "{}", mode.label());
            assert_eq!(masked.original_tokens, 100);

            let masked = distil_transcript(
                source,
                Some("Launch plan".to_string()),
                "https://youtu.be/x".to_string(),
                &options(mode),
                &mask,
            )
            .unwrap();
            assert!(!masked.content.contains("ORBITAL-7"), "{}", mode.label());
            assert!(!masked.content.contains("Launch plan"), "{}", mode.label());
        }
        let plain = distil_page(
            page(source),
            "https://docs.example/page".to_string(),
            &options(ReadMode::Markdown),
            &identity,
        )
        .unwrap();
        assert!(plain.content.starts_with(source), "{}", plain.content);
    }

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

    #[test]
    fn persona_chunker_trims_at_paragraph_boundaries() {
        let _guard = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_PERSONA", "research");

        let para = "alpha beta gamma delta epsilon zeta eta theta";
        let content = format!("{para}\n\n{para}\n\n{para}\n\n{para}");
        let budget = crate::core::tokens::count_tokens(para) * 2 + 1;
        let out = enforce_budget(&content, budget);

        crate::test_env::remove_var("LEAN_CTX_PERSONA");

        assert!(out.contains("[truncated"), "{out}");
        // The cut lands on a paragraph boundary: kept paragraphs verbatim,
        // no mid-word fragment before the marker.
        let body = out.split("\n\n…[truncated").next().unwrap();
        assert_eq!(body, format!("{para}\n\n{para}"), "{out}");
    }

    #[test]
    fn persona_compressor_strips_markdown_noise_for_research() {
        let _guard = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_PERSONA", "research");

        let body = "Intro ![badge](https://img.example/b.svg)\n\n\
                    See [the docs](https://docs.example/page) for details."
            .to_string();
        let out = apply_persona_compressor(body, ReadMode::Markdown);

        crate::test_env::remove_var("LEAN_CTX_PERSONA");

        assert!(out.contains("the docs"), "{out}");
        assert!(!out.contains("https://docs.example"), "{out}");
        assert!(!out.contains("img.example"), "{out}");
    }

    #[test]
    fn persona_compressor_leaves_extractive_modes_verbatim() {
        let _guard = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_PERSONA", "research");

        let body = "- (0.90) A [cited](https://src.example) claim.".to_string();
        let out = apply_persona_compressor(body.clone(), ReadMode::Facts);

        crate::test_env::remove_var("LEAN_CTX_PERSONA");

        assert_eq!(out, body);
    }

    #[test]
    fn persona_compressor_is_identity_for_coding() {
        let _guard = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_PERSONA", "coding");

        let body = "Anything ![badge](https://img.example/b.svg) stays.".to_string();
        let out = apply_persona_compressor(body.clone(), ReadMode::Markdown);

        crate::test_env::remove_var("LEAN_CTX_PERSONA");

        assert_eq!(out, body);
    }
}
