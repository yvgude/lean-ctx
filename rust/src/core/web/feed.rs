//! Dependency-free RSS / Atom / RDF feed → Markdown rendering.
//!
//! Feeds are XML, so the generic [`super::html_to_text`] renderer flattens them
//! into an unreadable mush (every `<title>`/`<link>`/`<description>` mashed
//! together). This module instead understands feed structure and emits a clean,
//! token-lean item list: a feed title heading followed by one linked entry per
//! item with its date and a short, tag-stripped summary (GH feedback: "add RSS
//! feed support to `url_read`").
//!
//! Supports RSS 2.0 (`<item>`), Atom (`<entry>`), and RSS 1.0 / RDF (`<item>`),
//! including `<![CDATA[…]]>` payloads and namespaced date fields (`dc:date`).

use super::html_to_text::decode_entities;

/// Cap on rendered items so a 200-entry feed stays token-bounded; the overall
/// token budget in `mod.rs` trims further if needed.
const MAX_ITEMS: usize = 50;
/// Per-item summary length cap (characters) — enough to triage, lean on tokens.
const SUMMARY_CHARS: usize = 280;

/// A parsed feed ready to hand back through the normal `read_web` pipeline.
pub struct FeedDoc {
    pub title: Option<String>,
    pub markdown: String,
}

/// True when the response is an RSS/Atom/RDF feed, by MIME type or a sniff of
/// the document root. Deliberately stricter than "is XML" so XHTML pages still
/// go to the HTML renderer.
#[must_use]
pub fn looks_like_feed(content_type: &str, body: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("rss") || ct.contains("atom") {
        return true;
    }
    // An explicit HTML type is a page, never a feed — don't let a stray "<rss"
    // mention in the markup misroute it to the feed renderer.
    if ct.contains("html") {
        return false;
    }
    // Otherwise (generic xml / text / empty) sniff the root for a feed element.
    let head: String = body
        .chars()
        .take(1024)
        .collect::<String>()
        .to_ascii_lowercase();
    head.contains("<rss") || head.contains("<feed") || head.contains("<rdf:rdf")
}

/// Parse a feed document into a clean Markdown item list.
#[must_use]
pub fn parse(xml: &str, _source_url: &str) -> FeedDoc {
    let doc = Xml::new(xml);
    let title = feed_title(&doc);

    let mut out = String::new();
    if let Some(t) = &title {
        out.push_str("# ");
        out.push_str(t);
        out.push_str("\n\n");
    }

    let blocks = doc.all_inner("item");
    let blocks = if blocks.is_empty() {
        doc.all_inner("entry")
    } else {
        blocks
    };

    let total = blocks.len();
    let mut rendered = 0;
    for block in blocks.into_iter().take(MAX_ITEMS) {
        let item = Xml::new(block);
        let it = parse_item(&item);
        if it.title.is_empty() && it.link.is_none() {
            continue;
        }
        out.push_str(&it.render());
        out.push_str("\n\n");
        rendered += 1;
    }

    if total > rendered {
        out.push_str(&format!("_…and {} more item(s)._", total - rendered));
    }
    if rendered == 0 && title.is_none() {
        out.push_str("No feed items found.");
    }

    FeedDoc {
        title,
        markdown: out.trim_end().to_string(),
    }
}

struct Item {
    title: String,
    link: Option<String>,
    date: Option<String>,
    summary: Option<String>,
}

impl Item {
    fn render(&self) -> String {
        let heading = match (&self.link, self.title.is_empty()) {
            (Some(link), false) => format!("## [{}]({link})", self.title),
            (Some(link), true) => format!("## [{link}]({link})"),
            (None, false) => format!("## {}", self.title),
            (None, true) => "## (untitled)".to_string(),
        };
        let mut meta = Vec::new();
        if let Some(d) = &self.date {
            meta.push(d.clone());
        }
        if let Some(s) = &self.summary {
            meta.push(s.clone());
        }
        if meta.is_empty() {
            heading
        } else {
            format!("{heading}\n{}", meta.join(" · "))
        }
    }
}

fn parse_item(item: &Xml) -> Item {
    let title = item
        .inner_text("title")
        .map(|t| clean_inline(&t))
        .unwrap_or_default();

    // RSS: <link>url</link>. Atom: <link href="url" rel="alternate"/>.
    let link = item
        .inner_text("link")
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .or_else(|| atom_link(item));

    let date = ["pubdate", "published", "updated", "dc:date", "date"]
        .into_iter()
        .find_map(|tag| item.inner_text(tag))
        .map(|d| clean_inline(&d))
        .filter(|d| !d.is_empty());

    let summary = ["description", "summary", "content"]
        .into_iter()
        .find_map(|tag| item.inner_text(tag))
        .map(|s| summarize(&s))
        .filter(|s| !s.is_empty());

    Item {
        title,
        link,
        date,
        summary,
    }
}

/// Atom `<link>` carries the URL in an `href` attribute; prefer `rel="alternate"`
/// (or no `rel`) over `self`/`edit` link relations.
fn atom_link(item: &Xml) -> Option<String> {
    let mut fallback = None;
    let mut from = 0;
    while let Some((open, content_or_end)) = item.open_tag("link", from) {
        from = content_or_end;
        let href = attr(open, "href")?;
        if href.is_empty() {
            continue;
        }
        match attr(open, "rel").as_deref() {
            None | Some("alternate") => return Some(href),
            Some(_) => {
                if fallback.is_none() {
                    fallback = Some(href);
                }
            }
        }
    }
    fallback
}

fn feed_title(doc: &Xml) -> Option<String> {
    // The channel/feed title is the first <title> before any item/entry.
    let cut = [doc.find("<item"), doc.find("<entry")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(doc.raw.len());
    let head = Xml::new(&doc.raw[..cut]);
    head.inner_text("title")
        .map(|t| clean_inline(&t))
        .filter(|s| !s.is_empty())
}

/// Strip HTML tags + entities from a feed summary and truncate to a lean length.
///
/// Tags are stripped twice around entity decoding: Atom `type="html"` payloads
/// arrive entity-*encoded* (`&lt;p&gt;`), so a single pre-decode strip would
/// leave visible `<p>` once decoded. RSS CDATA payloads carry literal tags, so
/// the first strip catches those.
fn summarize(raw: &str) -> String {
    let unwrapped = strip_cdata(raw);
    let once = strip_tags(&unwrapped);
    let decoded = decode_entities(&once);
    let twice = strip_tags(&decoded);
    let text = collapse_ws(&twice);
    let text = text.trim();
    if text.chars().count() > SUMMARY_CHARS {
        let truncated: String = text.chars().take(SUMMARY_CHARS).collect();
        format!("{}…", truncated.trim_end())
    } else {
        text.to_string()
    }
}

/// Decode entities + collapse whitespace for a short inline value (title/date).
fn clean_inline(raw: &str) -> String {
    collapse_ws(&decode_entities(&strip_cdata(raw)))
        .trim()
        .to_string()
}

fn strip_cdata(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("<![CDATA[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "<![CDATA[".len()..];
        let Some(end) = after.find("]]>") else {
            out.push_str(after);
            return out;
        };
        out.push_str(&after[..end]);
        rest = &after[end + 3..];
    }
    out.push_str(rest);
    out
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

fn attr(open_tag: &str, key: &str) -> Option<String> {
    let lower = open_tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(key) {
        let idx = from + pos;
        let boundary = idx == 0 || lower.as_bytes()[idx - 1].is_ascii_whitespace();
        let after = idx + key.len();
        let rest = open_tag[after..].trim_start();
        if boundary && rest.starts_with('=') {
            let val = rest[1..].trim_start();
            let bytes = val.as_bytes();
            if let Some(&q) = bytes.first()
                && (q == b'"' || q == b'\'')
            {
                let quote = q as char;
                return val[1..]
                    .find(quote)
                    .map(|end| val[1..=end].to_string())
                    .or_else(|| Some(val[1..].to_string()));
            }
            return val
                .split_whitespace()
                .next()
                .map(|v| v.trim_end_matches("/>").to_string());
        }
        from = after;
    }
    None
}

/// A lower-cased index over an XML slice for case-insensitive element lookups.
struct Xml<'a> {
    raw: &'a str,
    lower: String,
}

impl<'a> Xml<'a> {
    fn new(raw: &'a str) -> Self {
        Self {
            raw,
            lower: raw.to_ascii_lowercase(),
        }
    }

    fn find(&self, needle: &str) -> Option<usize> {
        self.lower.find(needle)
    }

    /// Locate `<tag …>` at/after `from`, returning `(open_tag_str, content_start)`
    /// where `open_tag_str` is the full `<…>` (for attribute parsing) and
    /// `content_start` is the byte index just past the `>`.
    fn open_tag(&self, tag: &str, from: usize) -> Option<(&'a str, usize)> {
        let needle = format!("<{}", tag.to_ascii_lowercase());
        let mut search = from;
        loop {
            let rel = self.lower[search..].find(&needle)?;
            let pos = search + rel;
            let after = pos + needle.len();
            let delim_ok = self.lower[after..]
                .chars()
                .next()
                .is_some_and(|c| matches!(c, '>' | ' ' | '\t' | '\n' | '\r' | '/'));
            if delim_ok {
                let gt = self.lower[pos..].find('>')? + pos;
                return Some((&self.raw[pos..=gt], gt + 1));
            }
            search = after;
        }
    }

    /// Inner text of the first `<tag>…</tag>` at/after document start.
    fn inner_text(&self, tag: &str) -> Option<String> {
        let (_, content_start) = self.open_tag(tag, 0)?;
        let close = format!("</{}", tag.to_ascii_lowercase());
        let end = self.lower[content_start..].find(&close)? + content_start;
        Some(self.raw[content_start..end].to_string())
    }

    /// Inner slices of every `<tag>…</tag>` block (non-nested).
    fn all_inner(&self, tag: &str) -> Vec<&'a str> {
        let mut out = Vec::new();
        let close = format!("</{}", tag.to_ascii_lowercase());
        let mut from = 0;
        while let Some((_, content_start)) = self.open_tag(tag, from) {
            let Some(rel) = self.lower[content_start..].find(&close) else {
                break;
            };
            let end = content_start + rel;
            out.push(&self.raw[content_start..end]);
            from = end + close.len();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_feed_by_mime_and_sniff() {
        assert!(looks_like_feed("application/rss+xml", ""));
        assert!(looks_like_feed("application/atom+xml", ""));
        assert!(looks_like_feed(
            "text/xml",
            "<?xml version='1.0'?><rss version='2.0'>"
        ));
        assert!(looks_like_feed(
            "",
            "<feed xmlns='http://www.w3.org/2005/Atom'>"
        ));
        assert!(!looks_like_feed("text/html", "<!doctype html><html><body>"));
        // An HTML page that merely mentions "<rss" must not be misrouted.
        assert!(!looks_like_feed(
            "text/html",
            "<html><body>How to read <rss> feeds</body></html>"
        ));
    }

    #[test]
    fn parses_rss_items_into_markdown() {
        let xml = r#"<?xml version="1.0"?>
        <rss version="2.0"><channel>
          <title>My Feed</title>
          <link>https://feed.example/</link>
          <description>Channel desc</description>
          <item>
            <title>First post</title>
            <link>https://feed.example/1</link>
            <pubDate>Mon, 02 Jun 2026 10:00:00 GMT</pubDate>
            <description><![CDATA[<p>Hello <b>world</b> with detail.</p>]]></description>
          </item>
          <item>
            <title>Second &amp; last</title>
            <link>https://feed.example/2</link>
            <description>Plain summary</description>
          </item>
        </channel></rss>"#;
        let doc = parse(xml, "https://feed.example/feed.xml");
        assert_eq!(doc.title.as_deref(), Some("My Feed"));
        assert!(doc.markdown.starts_with("# My Feed"));
        assert!(
            doc.markdown
                .contains("## [First post](https://feed.example/1)"),
            "item must be a linked heading: {}",
            doc.markdown
        );
        assert!(doc.markdown.contains("Mon, 02 Jun 2026 10:00:00 GMT"));
        assert!(
            doc.markdown.contains("Hello world with detail."),
            "CDATA HTML summary must be stripped to text: {}",
            doc.markdown
        );
        assert!(
            doc.markdown
                .contains("## [Second & last](https://feed.example/2)"),
            "entities in titles must decode: {}",
            doc.markdown
        );
    }

    #[test]
    fn parses_atom_entries_with_href_links() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom">
          <title>Atom Feed</title>
          <entry>
            <title>Atom entry</title>
            <link href="https://a.example/self" rel="self"/>
            <link href="https://a.example/post" rel="alternate"/>
            <updated>2026-06-02T00:00:00Z</updated>
            <summary>An atom summary.</summary>
          </entry>
        </feed>"#;
        let doc = parse(xml, "https://a.example/atom");
        assert_eq!(doc.title.as_deref(), Some("Atom Feed"));
        assert!(
            doc.markdown
                .contains("## [Atom entry](https://a.example/post)"),
            "must prefer rel=alternate link: {}",
            doc.markdown
        );
        assert!(doc.markdown.contains("An atom summary."));
    }

    #[test]
    fn strips_entity_encoded_html_in_atom_summary() {
        // Atom type="html" content is entity-encoded; the rendered summary must
        // not leak visible <p>/<a> tags (regression for the live Rust-blog feed).
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom">
          <title>F</title>
          <entry>
            <title>E</title>
            <link href="https://e/1"/>
            <content type="html">&lt;p&gt;Hello &lt;a href="https://x"&gt;link&lt;/a&gt; there.&lt;/p&gt;</content>
          </entry>
        </feed>"#;
        let doc = parse(xml, "https://e");
        assert!(
            doc.markdown.contains("Hello link there."),
            "entity-encoded HTML must be stripped to text: {}",
            doc.markdown
        );
        assert!(
            !doc.markdown.contains("<p>") && !doc.markdown.contains("&lt;"),
            "no raw/encoded tags may remain: {}",
            doc.markdown
        );
    }

    #[test]
    fn truncates_long_summaries() {
        let long = "x ".repeat(400);
        let xml = format!(
            "<rss><channel><title>F</title><item><title>T</title>\
             <link>https://e/1</link><description>{long}</description></item></channel></rss>"
        );
        let doc = parse(&xml, "https://e/feed");
        assert!(
            doc.markdown.contains('…'),
            "long summary should be truncated"
        );
    }

    #[test]
    fn handles_empty_feed_gracefully() {
        let doc = parse(
            "<rss><channel><title>Empty</title></channel></rss>",
            "https://e",
        );
        assert_eq!(doc.title.as_deref(), Some("Empty"));
        assert!(doc.markdown.contains("# Empty"));
    }
}
