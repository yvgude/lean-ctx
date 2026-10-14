#![allow(clippy::too_many_lines, clippy::collapsible_if)]
//! Deterministic HTML content extractor — extracts article/main content from
//! web pages, converts to clean markdown, discards boilerplate (#1124).
//!
//! Web pages fetched by agents (documentation, issue trackers, Stack Overflow)
//! contain ~90% non-informational tokens (navigation, ads, scripts, footers).
//! This module extracts only the meaningful article content and converts it to
//! markdown — the format agents work best with.
//!
//! Determinism (#498): output is a pure function of the input HTML — no
//! timestamps, counters, or randomness. Same HTML always produces same markdown.

use std::collections::VecDeque;

pub const KEEP_DATA_DIVISOR: usize = 2;
const MIN_HTML_BYTES: usize = 5000;
const MAX_EXTRACTED_TOKENS: usize = 8000;
const CHARS_PER_TOKEN_ESTIMATE: usize = 4;
const TRACKING_QUERY_KEYS: &[&str] = &["fbclid", "gclid", "mc_cid", "mc_eid", "_ga", "ref_src"];

/// A code block extracted from the selected article body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlock {
    /// Language hint from `class="language-*"`/`class="lang-*"`, or empty.
    pub language: String,
    /// Verbatim text inside the `<pre>`/`<code>` element.
    pub content: String,
}

/// Article metadata found in document `<meta>`, `<link>`, and `<time>` nodes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArticleMeta {
    pub author: Option<String>,
    pub date: Option<String>,
    pub url: Option<String>,
}

/// Deterministic article extraction result.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractionResult {
    pub title: Option<String>,
    pub content: String,
    pub code_blocks: Vec<CodeBlock>,
    pub metadata: ArticleMeta,
    /// Fraction of input bytes removed from the rendered article, in `[0, 1]`.
    pub token_reduction: f64,
}

impl ExtractionResult {
    /// Render the compact one-line source marker used by shell/proxy callers.
    #[must_use]
    pub fn metadata_line(&self) -> Option<String> {
        let mut fields = Vec::new();
        if let Some(url) = self.metadata.url.as_deref() {
            fields.push(url);
        }
        if let Some(title) = self.title.as_deref() {
            fields.push(title);
        }
        if let Some(date) = self.metadata.date.as_deref() {
            fields.push(date);
        }
        (!fields.is_empty()).then(|| format!("Source: {}", fields.join(" | ")))
    }
}

#[derive(Debug, Clone)]
pub struct CrushResult {
    pub text: String,
    pub lossless: bool,
    pub original_bytes: usize,
    pub extracted_tokens: usize,
}

pub fn is_html_content(content: &str) -> bool {
    let trimmed = content.trim_start().to_ascii_lowercase();
    trimmed.starts_with("<!doctype")
        || trimmed.starts_with("<html")
        || trimmed.starts_with("<?xml")
        || (trimmed.contains("<head") && trimmed.contains("<body"))
}

pub fn crush_if_beneficial(html: &str) -> Option<CrushResult> {
    if html.len() < MIN_HTML_BYTES {
        return None;
    }
    if !is_html_content(html) {
        return None;
    }

    let extraction = extract_article_content(html);
    if extraction.content.is_empty() {
        return None;
    }
    let extracted = match extraction.metadata_line() {
        Some(line) => format!("{line}\n\n{}", extraction.content),
        None => extraction.content,
    };

    let original_tokens = html.len() / CHARS_PER_TOKEN_ESTIMATE;
    let extracted_tokens = extracted.len() / CHARS_PER_TOKEN_ESTIMATE;

    if extracted_tokens * KEEP_DATA_DIVISOR >= original_tokens {
        return None;
    }
    if extracted_tokens > MAX_EXTRACTED_TOKENS {
        let char_budget = MAX_EXTRACTED_TOKENS * CHARS_PER_TOKEN_ESTIMATE;
        let mut end = char_budget.min(extracted.len());
        while !extracted.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        let truncated = format!(
            "{}\n\n[… truncated, {} more tokens in original]",
            &extracted[..end],
            original_tokens - MAX_EXTRACTED_TOKENS
        );
        return Some(CrushResult {
            text: truncated,
            lossless: false,
            original_bytes: html.len(),
            extracted_tokens: MAX_EXTRACTED_TOKENS,
        });
    }

    Some(CrushResult {
        text: extracted,
        lossless: false,
        original_bytes: html.len(),
        extracted_tokens,
    })
}

pub fn extract_article(html: &str) -> String {
    extract_article_content(html).content
}

/// Extract selected article content, metadata, and sacred code blocks.
pub fn extract_article_content(html: &str) -> ExtractionResult {
    let tokens = tokenize(html);
    let nodes = build_tree(&tokens);
    let article = select_main_content(&nodes);
    let content = nodes_to_markdown(&article);
    let (title, metadata) = extract_metadata(&nodes, &article);
    let code_blocks = collect_code_blocks(&article);
    let token_reduction = if html.is_empty() {
        0.0
    } else {
        (1.0 - content.len() as f64 / html.len() as f64).clamp(0.0, 1.0)
    };
    ExtractionResult {
        title,
        content,
        code_blocks,
        metadata,
        token_reduction,
    }
}

// --- HTML Tokenizer ---

#[derive(Debug, Clone, PartialEq)]
enum HtmlToken {
    OpenTag {
        name: String,
        attrs: Vec<(String, String)>,
        self_closing: bool,
    },
    CloseTag {
        name: String,
    },
    Text(String),
}

fn tokenize(html: &str) -> Vec<HtmlToken> {
    let mut tokens = Vec::new();
    let mut chars = html.chars().peekable();
    let mut text_buf = String::new();

    while let Some(&ch) = chars.peek() {
        if ch == '<' {
            if !text_buf.is_empty() {
                let t = std::mem::take(&mut text_buf);
                tokens.push(HtmlToken::Text(decode_entities(&t)));
            }
            chars.next();
            if chars.peek() == Some(&'!') {
                skip_comment_or_doctype(&mut chars);
                continue;
            }
            let is_close = chars.peek() == Some(&'/');
            if is_close {
                chars.next();
            }
            let tag_name = consume_tag_name(&mut chars);
            if tag_name.is_empty() {
                text_buf.push('<');
                if is_close {
                    text_buf.push('/');
                }
                continue;
            }
            if is_close {
                skip_until_gt(&mut chars);
                tokens.push(HtmlToken::CloseTag {
                    name: tag_name.to_ascii_lowercase(),
                });
            } else {
                let (attrs, self_closing) = parse_attrs(&mut chars);
                let name = tag_name.to_ascii_lowercase();
                if is_raw_text_element(&name) {
                    skip_raw_content(&mut chars, &name);
                }
                tokens.push(HtmlToken::OpenTag {
                    name,
                    attrs,
                    self_closing,
                });
            }
        } else {
            text_buf.push(ch);
            chars.next();
        }
    }
    if !text_buf.is_empty() {
        tokens.push(HtmlToken::Text(decode_entities(&text_buf)));
    }
    tokens
}

fn is_raw_text_element(name: &str) -> bool {
    matches!(name, "script" | "style" | "noscript" | "template")
}

fn skip_raw_content(chars: &mut std::iter::Peekable<std::str::Chars>, tag: &str) {
    let close_tag = format!("</{tag}");
    let mut buf = String::new();
    for ch in chars.by_ref() {
        buf.push(ch);
        if buf.ends_with(&close_tag) {
            skip_until_gt(chars);
            return;
        }
        if buf.len() > close_tag.len() + 100 {
            buf.drain(..buf.len() - close_tag.len());
        }
    }
}

fn skip_comment_or_doctype(chars: &mut std::iter::Peekable<std::str::Chars>) {
    for ch in chars.by_ref() {
        if ch == '>' {
            return;
        }
    }
}

fn consume_tag_name(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut name = String::new();
    while let Some(&ch) = chars.peek() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            name.push(ch);
            chars.next();
        } else {
            break;
        }
    }
    name
}

fn parse_attrs(chars: &mut std::iter::Peekable<std::str::Chars>) -> (Vec<(String, String)>, bool) {
    let mut attrs = Vec::new();
    let mut self_closing = false;

    loop {
        skip_whitespace(chars);
        match chars.peek() {
            None => break,
            Some(&'>') => {
                chars.next();
                break;
            }
            Some(&'/') => {
                chars.next();
                if chars.peek() == Some(&'>') {
                    chars.next();
                    self_closing = true;
                }
                break;
            }
            _ => {}
        }
        let key = consume_attr_name(chars);
        if key.is_empty() {
            chars.next();
            continue;
        }
        skip_whitespace(chars);
        let value = if chars.peek() == Some(&'=') {
            chars.next();
            skip_whitespace(chars);
            consume_attr_value(chars)
        } else {
            String::new()
        };
        attrs.push((key.to_ascii_lowercase(), value));
    }
    (attrs, self_closing)
}

fn consume_attr_name(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut name = String::new();
    while let Some(&ch) = chars.peek() {
        if ch == '=' || ch == '>' || ch == '/' || ch.is_ascii_whitespace() {
            break;
        }
        name.push(ch);
        chars.next();
    }
    name
}

fn consume_attr_value(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut value = String::new();
    match chars.peek() {
        Some(&'"') => {
            chars.next();
            for ch in chars.by_ref() {
                if ch == '"' {
                    break;
                }
                value.push(ch);
            }
        }
        Some(&'\'') => {
            chars.next();
            for ch in chars.by_ref() {
                if ch == '\'' {
                    break;
                }
                value.push(ch);
            }
        }
        _ => {
            while let Some(&ch) = chars.peek() {
                if ch.is_ascii_whitespace() || ch == '>' {
                    break;
                }
                value.push(ch);
                chars.next();
            }
        }
    }
    value
}

fn skip_whitespace(chars: &mut std::iter::Peekable<std::str::Chars>) {
    while chars.peek().is_some_and(char::is_ascii_whitespace) {
        chars.next();
    }
}

fn skip_until_gt(chars: &mut std::iter::Peekable<std::str::Chars>) {
    for ch in chars.by_ref() {
        if ch == '>' {
            return;
        }
    }
}

fn decode_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&#x27;", "'")
        .replace("&#x2F;", "/")
}

// --- Tree Builder ---

#[derive(Debug, Clone)]
struct HtmlNode {
    tag: String,
    attrs: Vec<(String, String)>,
    children: Vec<HtmlNodeChild>,
}

#[derive(Debug, Clone)]
enum HtmlNodeChild {
    Element(HtmlNode),
    Text(String),
}

const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

fn build_tree(tokens: &[HtmlToken]) -> Vec<HtmlNodeChild> {
    let mut stack: VecDeque<HtmlNode> = VecDeque::new();
    stack.push_back(HtmlNode {
        tag: "root".into(),
        attrs: vec![],
        children: vec![],
    });

    for token in tokens {
        match token {
            HtmlToken::OpenTag {
                name,
                attrs,
                self_closing,
            } => {
                if *self_closing || VOID_ELEMENTS.contains(&name.as_str()) {
                    let node = HtmlNode {
                        tag: name.clone(),
                        attrs: attrs.clone(),
                        children: vec![],
                    };
                    if let Some(parent) = stack.back_mut() {
                        parent.children.push(HtmlNodeChild::Element(node));
                    }
                } else {
                    stack.push_back(HtmlNode {
                        tag: name.clone(),
                        attrs: attrs.clone(),
                        children: vec![],
                    });
                }
            }
            HtmlToken::CloseTag { name } => {
                if let Some(pos) = stack.iter().rposition(|n| n.tag == *name) {
                    while stack.len() > pos + 1 {
                        let child = stack
                            .pop_back()
                            .expect("stack.len() > pos + 1 guarantees an element");
                        if let Some(parent) = stack.back_mut() {
                            parent.children.push(HtmlNodeChild::Element(child));
                        }
                    }
                    let child = stack
                        .pop_back()
                        .expect("matching open tag guarantees a closable element");
                    if let Some(parent) = stack.back_mut() {
                        parent.children.push(HtmlNodeChild::Element(child));
                    }
                }
            }
            HtmlToken::Text(text) => {
                let trimmed = text.trim();
                if !trimmed.is_empty()
                    && let Some(parent) = stack.back_mut()
                {
                    parent.children.push(HtmlNodeChild::Text(text.clone()));
                }
            }
        }
    }

    while stack.len() > 1 {
        let child = stack
            .pop_back()
            .expect("stack.len() > 1 guarantees an element");
        if let Some(parent) = stack.back_mut() {
            parent.children.push(HtmlNodeChild::Element(child));
        }
    }

    stack.pop_back().map(|r| r.children).unwrap_or_default()
}

// --- Content Selection ---

const BOILERPLATE_TAGS: &[&str] = &["nav", "footer", "header", "aside", "menu", "menuitem"];

const BOILERPLATE_ROLES: &[&str] = &[
    "navigation",
    "banner",
    "contentinfo",
    "complementary",
    "menu",
];

const BOILERPLATE_CLASSES: &[&str] = &[
    "nav",
    "navbar",
    "footer",
    "sidebar",
    "menu",
    "cookie",
    "banner",
    "advertisement",
    "ad",
    "social",
    "share",
    "comment",
    "related",
];

fn select_main_content(nodes: &[HtmlNodeChild]) -> Vec<HtmlNodeChild> {
    if let Some(main) = find_element_by_tag_or_role(nodes, "main", "main") {
        return main.children.clone();
    }
    if let Some(article) = find_element_by_tag_or_role(nodes, "article", "") {
        return article.children.clone();
    }
    if let Some(content) = find_element_by_id_class(
        nodes,
        &[
            "content",
            "main-content",
            "post-content",
            "entry-content",
            "article-body",
        ],
    ) {
        return content.children.clone();
    }
    if let Some(body) = find_element_by_tag(nodes, "body") {
        return filter_boilerplate(&body.children);
    }
    filter_boilerplate(nodes)
}

fn find_element_by_tag_or_role<'a>(
    nodes: &'a [HtmlNodeChild],
    tag: &str,
    role: &str,
) -> Option<&'a HtmlNode> {
    for child in nodes {
        if let HtmlNodeChild::Element(el) = child {
            if el.tag == tag {
                return Some(el);
            }
            if !role.is_empty() && el.attrs.iter().any(|(k, v)| k == "role" && v == role) {
                return Some(el);
            }
            if let Some(found) = find_element_by_tag_or_role(&el.children, tag, role) {
                return Some(found);
            }
        }
    }
    None
}

fn find_element_by_tag<'a>(nodes: &'a [HtmlNodeChild], tag: &str) -> Option<&'a HtmlNode> {
    for child in nodes {
        if let HtmlNodeChild::Element(el) = child {
            if el.tag == tag {
                return Some(el);
            }
            if let Some(found) = find_element_by_tag(&el.children, tag) {
                return Some(found);
            }
        }
    }
    None
}

fn find_element_by_id_class<'a>(
    nodes: &'a [HtmlNodeChild],
    candidates: &[&str],
) -> Option<&'a HtmlNode> {
    for child in nodes {
        if let HtmlNodeChild::Element(el) = child {
            let id = el
                .attrs
                .iter()
                .find(|(k, _)| k == "id")
                .map_or("", |(_, v)| v.as_str());
            let class = el
                .attrs
                .iter()
                .find(|(k, _)| k == "class")
                .map_or("", |(_, v)| v.as_str());
            if candidates
                .iter()
                .any(|c| id == *c || class.split_whitespace().any(|cls| cls == *c))
            {
                return Some(el);
            }
            if let Some(found) = find_element_by_id_class(&el.children, candidates) {
                return Some(found);
            }
        }
    }
    None
}

fn attribute<'a>(el: &'a HtmlNode, name: &str) -> Option<&'a str> {
    el.attrs
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn node_text(nodes: &[HtmlNodeChild]) -> String {
    let mut text = String::new();
    raw_text(nodes, &mut text);
    normalize_whitespace(&text).trim().to_string()
}

fn raw_text(nodes: &[HtmlNodeChild], out: &mut String) {
    for child in nodes {
        match child {
            HtmlNodeChild::Text(text) => out.push_str(text),
            HtmlNodeChild::Element(el) => raw_text(&el.children, out),
        }
    }
}

fn find_meta_value(nodes: &[HtmlNodeChild], names: &[&str]) -> Option<String> {
    for child in nodes {
        let HtmlNodeChild::Element(el) = child else {
            continue;
        };
        if el.tag == "meta"
            && let Some(value) = attribute(el, "content")
            && !value.trim().is_empty()
            && names.iter().any(|name| {
                attribute(el, "name")
                    .or_else(|| attribute(el, "property"))
                    .is_some_and(|actual| actual.eq_ignore_ascii_case(name))
            })
        {
            return Some(value.trim().to_string());
        }
        if let Some(value) = find_meta_value(&el.children, names) {
            return Some(value);
        }
    }
    None
}

fn find_canonical_url(nodes: &[HtmlNodeChild]) -> Option<String> {
    for child in nodes {
        let HtmlNodeChild::Element(el) = child else {
            continue;
        };
        if el.tag == "link"
            && attribute(el, "rel").is_some_and(|rel| {
                rel.split_whitespace()
                    .any(|v| v.eq_ignore_ascii_case("canonical"))
            })
            && let Some(href) = attribute(el, "href")
            && !href.trim().is_empty()
        {
            return Some(href.trim().to_string());
        }
        if let Some(value) = find_canonical_url(&el.children) {
            return Some(value);
        }
    }
    None
}

fn extract_metadata(
    nodes: &[HtmlNodeChild],
    article: &[HtmlNodeChild],
) -> (Option<String>, ArticleMeta) {
    let title = find_element_by_tag(nodes, "title")
        .map(|el| node_text(&el.children))
        .filter(|value| !value.is_empty())
        .or_else(|| {
            find_element_by_tag(article, "h1")
                .map(|el| node_text(&el.children))
                .filter(|value| !value.is_empty())
        });
    let author = find_meta_value(nodes, &["author", "article:author"]);
    let date = find_meta_value(
        nodes,
        &[
            "date",
            "article:published_time",
            "article:modified_time",
            "pubdate",
        ],
    )
    .or_else(|| {
        find_element_by_tag(nodes, "time").and_then(|el| {
            attribute(el, "datetime").map(str::to_string).or_else(|| {
                let text = node_text(&el.children);
                (!text.is_empty()).then_some(text)
            })
        })
    });
    let url = find_meta_value(nodes, &["og:url", "twitter:url", "url"])
        .or_else(|| find_canonical_url(nodes));
    (title, ArticleMeta { author, date, url })
}

fn collect_code_blocks(nodes: &[HtmlNodeChild]) -> Vec<CodeBlock> {
    let mut blocks = Vec::new();
    collect_code_blocks_inner(nodes, &mut blocks);
    blocks
}

fn collect_code_blocks_inner(nodes: &[HtmlNodeChild], blocks: &mut Vec<CodeBlock>) {
    for child in nodes {
        let HtmlNodeChild::Element(el) = child else {
            continue;
        };
        if el.tag == "pre" || el.tag == "code" {
            let mut content = String::new();
            raw_text(&el.children, &mut content);
            blocks.push(CodeBlock {
                language: if el.tag == "pre" {
                    detect_code_language(el)
                } else {
                    code_language(el)
                },
                content,
            });
            if el.tag == "pre" {
                continue;
            }
        }
        collect_code_blocks_inner(&el.children, blocks);
    }
}

fn filter_boilerplate(nodes: &[HtmlNodeChild]) -> Vec<HtmlNodeChild> {
    nodes
        .iter()
        .filter(|child| {
            if let HtmlNodeChild::Element(el) = child {
                !is_boilerplate(el)
            } else {
                true
            }
        })
        .cloned()
        .collect()
}

fn is_boilerplate(el: &HtmlNode) -> bool {
    if BOILERPLATE_TAGS.contains(&el.tag.as_str()) {
        return true;
    }
    let role = el
        .attrs
        .iter()
        .find(|(k, _)| k == "role")
        .map_or("", |(_, v)| v.as_str());
    if BOILERPLATE_ROLES.contains(&role) {
        return true;
    }
    let class = el
        .attrs
        .iter()
        .find(|(k, _)| k == "class")
        .map_or("", |(_, v)| v.as_str());
    BOILERPLATE_CLASSES
        .iter()
        .any(|bc| class.split_whitespace().any(|cls| cls.contains(bc)))
}

// --- Markdown Converter ---

fn nodes_to_markdown(nodes: &[HtmlNodeChild]) -> String {
    let mut output = String::new();
    render_nodes(nodes, &mut output, &mut RenderState::default());
    collapse_whitespace(&output)
}

#[derive(Default)]
struct RenderState {
    list_depth: usize,
    ordered_counter: Vec<usize>,
    in_pre: bool,
}

fn render_nodes(nodes: &[HtmlNodeChild], out: &mut String, state: &mut RenderState) {
    for child in nodes {
        match child {
            HtmlNodeChild::Text(text) => {
                if state.in_pre {
                    out.push_str(text);
                } else {
                    let normalized = normalize_whitespace(text);
                    if !normalized.is_empty() {
                        out.push_str(&normalized);
                    }
                }
            }
            HtmlNodeChild::Element(el) => render_element(el, out, state),
        }
    }
}

fn collapse_tracking_parameters(href: &str) -> String {
    let Some((base, query_and_fragment)) = href.split_once('?') else {
        return href.to_string();
    };
    let (query, fragment) = query_and_fragment
        .split_once('#')
        .unwrap_or((query_and_fragment, ""));
    let kept: Vec<&str> = query
        .split('&')
        .filter(|part| {
            let key = part.split_once('=').map_or(*part, |(key, _)| key);
            let lower = key.to_ascii_lowercase();
            !lower.starts_with("utm_") && !TRACKING_QUERY_KEYS.contains(&lower.as_str())
        })
        .collect();
    let mut result = base.to_string();
    if !kept.is_empty() {
        result.push('?');
        result.push_str(&kept.join("&"));
    }
    if !fragment.is_empty() {
        result.push('#');
        result.push_str(fragment);
    }
    result
}

fn render_element(el: &HtmlNode, out: &mut String, state: &mut RenderState) {
    match el.tag.as_str() {
        "h1" => {
            ensure_newlines(out, 2);
            out.push_str("# ");
            render_nodes(&el.children, out, state);
            ensure_newlines(out, 2);
        }
        "h2" => {
            ensure_newlines(out, 2);
            out.push_str("## ");
            render_nodes(&el.children, out, state);
            ensure_newlines(out, 2);
        }
        "h3" => {
            ensure_newlines(out, 2);
            out.push_str("### ");
            render_nodes(&el.children, out, state);
            ensure_newlines(out, 2);
        }
        "h4" => {
            ensure_newlines(out, 2);
            out.push_str("#### ");
            render_nodes(&el.children, out, state);
            ensure_newlines(out, 2);
        }
        "h5" => {
            ensure_newlines(out, 2);
            out.push_str("##### ");
            render_nodes(&el.children, out, state);
            ensure_newlines(out, 2);
        }
        "h6" => {
            ensure_newlines(out, 2);
            out.push_str("###### ");
            render_nodes(&el.children, out, state);
            ensure_newlines(out, 2);
        }
        "p" | "div" | "section" | "article" => {
            ensure_newlines(out, 2);
            render_nodes(&el.children, out, state);
            ensure_newlines(out, 2);
        }
        "br" => {
            out.push('\n');
        }
        "hr" => {
            ensure_newlines(out, 2);
            out.push_str("---");
            ensure_newlines(out, 2);
        }
        "strong" | "b" => {
            out.push_str("**");
            render_nodes(&el.children, out, state);
            out.push_str("**");
        }
        "em" | "i" => {
            out.push('*');
            render_nodes(&el.children, out, state);
            out.push('*');
        }
        "code" if !state.in_pre => {
            out.push('`');
            render_nodes(&el.children, out, state);
            out.push('`');
        }
        "pre" => {
            ensure_newlines(out, 2);
            let lang = detect_code_language(el);
            out.push_str("```");
            out.push_str(&lang);
            out.push('\n');
            state.in_pre = true;
            render_nodes(&el.children, out, state);
            state.in_pre = false;
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```");
            ensure_newlines(out, 2);
        }
        "a" => {
            let href = el
                .attrs
                .iter()
                .find(|(k, _)| k == "href")
                .map_or("", |(_, v)| v.as_str());
            let mut link_text = String::new();
            render_nodes(&el.children, &mut link_text, state);
            let link_text = link_text.trim().to_string();
            if !link_text.is_empty()
                && !href.is_empty()
                && !href.starts_with('#')
                && !href.starts_with("javascript:")
            {
                out.push('[');
                out.push_str(&link_text);
                out.push_str("](");
                out.push_str(&collapse_tracking_parameters(href));
                out.push(')');
            } else if !link_text.is_empty() {
                out.push_str(&link_text);
            }
        }
        "ul" => {
            ensure_newlines(out, 2);
            state.list_depth += 1;
            render_nodes(&el.children, out, state);
            state.list_depth -= 1;
            ensure_newlines(out, 2);
        }
        "ol" => {
            ensure_newlines(out, 2);
            state.list_depth += 1;
            state.ordered_counter.push(0);
            render_nodes(&el.children, out, state);
            state.ordered_counter.pop();
            state.list_depth -= 1;
            ensure_newlines(out, 2);
        }
        "li" => {
            ensure_newlines(out, 1);
            let indent = "  ".repeat(state.list_depth.saturating_sub(1));
            out.push_str(&indent);
            if let Some(counter) = state.ordered_counter.last_mut() {
                *counter += 1;
                out.push_str(&format!("{counter}. "));
            } else {
                out.push_str("- ");
            }
            render_nodes(&el.children, out, state);
        }
        "blockquote" => {
            ensure_newlines(out, 2);
            let mut inner = String::new();
            render_nodes(&el.children, &mut inner, state);
            for line in inner.trim().lines() {
                out.push_str("> ");
                out.push_str(line);
                out.push('\n');
            }
            ensure_newlines(out, 2);
        }
        "table" => {
            ensure_newlines(out, 2);
            render_table(el, out, state);
            ensure_newlines(out, 2);
        }
        "img" => {
            let alt = el
                .attrs
                .iter()
                .find(|(k, _)| k == "alt")
                .map_or("", |(_, v)| v.as_str());
            let src = el
                .attrs
                .iter()
                .find(|(k, _)| k == "src")
                .map_or("", |(_, v)| v.as_str());
            if !alt.is_empty() && !src.is_empty() {
                out.push_str(&format!("![{alt}]({src})"));
            }
        }
        _ => {
            render_nodes(&el.children, out, state);
        }
    }
}

fn render_table(el: &HtmlNode, out: &mut String, state: &mut RenderState) {
    let rows = collect_table_rows(el);
    if rows.is_empty() {
        return;
    }

    let col_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if col_count == 0 {
        return;
    }

    for (i, row) in rows.iter().enumerate() {
        out.push('|');
        for col in 0..col_count {
            let cell = row.get(col).map_or("", String::as_str);
            out.push(' ');
            out.push_str(cell.trim());
            out.push_str(" |");
        }
        out.push('\n');
        if i == 0 {
            out.push('|');
            for _ in 0..col_count {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    let _ = state;
}

fn collect_table_rows(el: &HtmlNode) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    collect_rows_recursive(el, &mut rows);
    rows
}

fn collect_rows_recursive(el: &HtmlNode, rows: &mut Vec<Vec<String>>) {
    if el.tag == "tr" {
        let cells: Vec<String> = el
            .children
            .iter()
            .filter_map(|child| {
                if let HtmlNodeChild::Element(cell) = child {
                    if cell.tag == "td" || cell.tag == "th" {
                        let mut text = String::new();
                        render_nodes(&cell.children, &mut text, &mut RenderState::default());
                        return Some(text.trim().to_string());
                    }
                }
                None
            })
            .collect();
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    for child in &el.children {
        if let HtmlNodeChild::Element(child_el) = child {
            collect_rows_recursive(child_el, rows);
        }
    }
}

fn code_language(el: &HtmlNode) -> String {
    let class = attribute(el, "class").unwrap_or("");
    for cls in class.split_whitespace() {
        if let Some(lang) = cls.strip_prefix("language-") {
            return lang.to_string();
        }
        if let Some(lang) = cls.strip_prefix("lang-") {
            return lang.to_string();
        }
        if matches!(
            cls,
            "rust"
                | "python"
                | "javascript"
                | "typescript"
                | "go"
                | "java"
                | "c"
                | "cpp"
                | "ruby"
                | "bash"
                | "sh"
                | "json"
                | "yaml"
                | "toml"
                | "sql"
                | "html"
                | "css"
        ) {
            return cls.to_string();
        }
    }
    String::new()
}

fn detect_code_language(pre: &HtmlNode) -> String {
    for child in &pre.children {
        if let HtmlNodeChild::Element(code) = child {
            if code.tag == "code" {
                let language = code_language(code);
                if !language.is_empty() {
                    return language;
                }
            }
        }
    }
    code_language(pre)
}

fn normalize_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut last_was_space = false;
    for ch in text.chars() {
        if ch.is_ascii_whitespace() {
            if !last_was_space {
                result.push(' ');
                last_was_space = true;
            }
        } else {
            result.push(ch);
            last_was_space = false;
        }
    }
    result
}

fn ensure_newlines(out: &mut String, count: usize) {
    let trailing_newlines = out.chars().rev().take_while(|&c| c == '\n').count();
    for _ in trailing_newlines..count {
        out.push('\n');
    }
}

fn collapse_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut consecutive_newlines = 0u32;

    for ch in text.chars() {
        if ch == '\n' {
            consecutive_newlines += 1;
            if consecutive_newlines <= 2 {
                result.push('\n');
            }
        } else {
            consecutive_newlines = 0;
            result.push(ch);
        }
    }
    result.trim().to_string()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn detects_html_content() {
        assert!(is_html_content(
            "<!DOCTYPE html><html><body>hi</body></html>"
        ));
        assert!(is_html_content(
            "  <html lang='en'><head></head><body></body></html>"
        ));
        assert!(!is_html_content("just plain text"));
        assert!(!is_html_content("{\"json\": true}"));
    }

    #[test]
    fn extracts_article_element() {
        let html = r"
            <html><body>
                <nav>Navigation stuff</nav>
                <article>
                    <h1>Title</h1>
                    <p>Important content here.</p>
                </article>
                <footer>Footer junk</footer>
            </body></html>
        ";
        let result = extract_article(html);
        assert!(result.contains("# Title"));
        assert!(result.contains("Important content here."));
        assert!(!result.contains("Navigation stuff"));
        assert!(!result.contains("Footer junk"));
    }

    #[test]
    fn extracts_main_element() {
        let html = r"
            <html><body>
                <header>Header</header>
                <main>
                    <h2>Main Content</h2>
                    <p>The real stuff.</p>
                </main>
                <aside>Sidebar</aside>
            </body></html>
        ";
        let result = extract_article(html);
        assert!(result.contains("## Main Content"));
        assert!(result.contains("The real stuff."));
        assert!(!result.contains("Header"));
        assert!(!result.contains("Sidebar"));
    }

    #[test]
    fn preserves_code_blocks() {
        let html = r#"
            <html><body><article>
                <p>Example:</p>
                <pre><code class="language-rust">fn main() {
    println!("hello");
}</code></pre>
            </article></body></html>
        "#;
        let result = extract_article(html);
        assert!(result.contains("```rust"));
        assert!(result.contains("fn main()"));
        assert!(result.contains("```"));
    }

    #[test]
    fn converts_links() {
        let html = r#"<html><body><article><p>See <a href="https://example.com">docs</a></p></article></body></html>"#;
        let result = extract_article(html);
        assert!(result.contains("[docs](https://example.com)"));
    }

    #[test]
    fn converts_tables() {
        let html = r"
            <html><body><article>
                <table>
                    <tr><th>Name</th><th>Value</th></tr>
                    <tr><td>foo</td><td>42</td></tr>
                </table>
            </article></body></html>
        ";
        let result = extract_article(html);
        assert!(result.contains("| Name | Value |"));
        assert!(result.contains("| foo | 42 |"));
    }

    #[test]
    fn crush_rejects_small_input() {
        let small = "<html><body><p>hi</p></body></html>";
        assert!(crush_if_beneficial(small).is_none());
    }

    #[test]
    fn crush_is_deterministic() {
        let html = format!(
            "<html><body><nav>{}</nav><article><h1>Title</h1><p>{}</p></article><footer>{}</footer></body></html>",
            "x".repeat(3000),
            "content ".repeat(200),
            "y".repeat(3000)
        );
        let r1 = crush_if_beneficial(&html);
        let r2 = crush_if_beneficial(&html);
        assert_eq!(r1.as_ref().map(|r| &r.text), r2.as_ref().map(|r| &r.text));
    }
}

#[cfg(test)]
pub mod edge_tests {
    use super::*;

    #[test]
    fn handles_malformed_html_gracefully() {
        let broken = "<html><body><div><p>Unclosed paragraph<div>Nested wrong</p></div>";
        let result = extract_article(broken);
        assert!(result.contains("Unclosed paragraph") || result.contains("Nested wrong"));
    }

    #[test]
    fn handles_empty_html() {
        let empty = "<html><body></body></html>";
        assert!(crush_if_beneficial(empty).is_none());
    }

    #[test]
    fn handles_unicode_content() {
        let html = "<html><body><article><h1>\u{65E5}\u{672C}\u{8A9E}</h1><p>\u{00DC}nic\u{00F6}d\u{00E9} with emojis \u{1F680}</p></article></body></html>";
        let result = extract_article(html);
        assert!(result.contains("\u{65E5}\u{672C}\u{8A9E}"));
        assert!(result.contains("\u{1F680}"));
    }

    #[test]
    fn handles_deeply_nested_structures() {
        let mut html = String::from("<html><body><article>");
        for i in 0..50 {
            html.push_str(&format!("<div><p>Level {i}</p>"));
        }
        for _ in 0..50 {
            html.push_str("</div>");
        }
        html.push_str("</article></body></html>");
        let result = extract_article(&html);
        assert!(result.contains("Level 0"));
        assert!(result.contains("Level 49"));
    }

    #[test]
    fn handles_script_and_style_exclusion() {
        let html = "<html><body><article><script>var x = 'not content';</script><style>.foo{}</style><p>Real content.</p></article></body></html>";
        let result = extract_article(html);
        assert!(result.contains("Real content."));
        assert!(!result.contains("not content"));
        assert!(!result.contains(".foo"));
    }

    #[test]
    fn handles_entities_correctly() {
        let html =
            "<html><body><article><p>5 &gt; 3 &amp;&amp; 2 &lt; 4</p></article></body></html>";
        let result = extract_article(html);
        assert!(result.contains("5 > 3 && 2 < 4"));
    }

    #[test]
    fn handles_empty_article() {
        let html = "<html><body><article>   \n\t  </article></body></html>";
        let result = extract_article(html);
        {
            let found = &result.trim();
            assert!(found.is_empty(), "{found:?}");
        }
    }
}
