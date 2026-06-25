//! Dependency-free HTML → Markdown / plain-text conversion.
//!
//! A small tag tokenizer feeds a state-machine renderer. The goal is *clean,
//! readable* content for an LLM, not a faithful DOM: noise elements (script,
//! style, nav chrome) are dropped, block structure becomes Markdown headings /
//! lists / paragraphs, links become `[text](href)`, and `<pre>` becomes fenced
//! code. Implemented without an HTML crate to stay in line with the project's
//! zero-heavy-dependency stance.

/// A hyperlink extracted from the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub text: String,
    pub href: String,
}

/// Parsed document: optional `<title>`, rendered Markdown, and extracted links.
#[derive(Debug, Clone)]
pub struct HtmlDoc {
    pub title: Option<String>,
    pub markdown: String,
    pub links: Vec<Link>,
}

/// Convert an HTML document into Markdown plus extracted metadata.
#[must_use]
pub fn parse(html: &str) -> HtmlDoc {
    let title = extract_title(html);
    let content = select_main(html);
    let mut renderer = Renderer::default();
    for token in tokenize(content) {
        renderer.consume(&token);
    }
    let markdown = normalize(&renderer.out);
    HtmlDoc {
        title,
        markdown,
        links: renderer.links,
    }
}

/// Extract just the document `<title>` without rendering the body.
#[must_use]
pub fn title(html: &str) -> Option<String> {
    extract_title(html)
}

/// Strip Markdown decorations to obtain flowing plain text.
#[must_use]
pub fn markdown_to_text(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut in_fence = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let stripped = strip_inline_markup(line);
        out.push_str(&stripped);
        out.push('\n');
    }
    out.trim().to_string()
}

fn strip_inline_markup(line: &str) -> String {
    let without_heading = line.trim_start().trim_start_matches('#').trim_start();
    replace_links_with_text(without_heading)
}

/// Replace `[text](href)` spans with their visible text only.
fn replace_links_with_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'['
            && let Some(rel_close) = s[i + 1..].find(']')
        {
            let close = i + 1 + rel_close;
            if s[close + 1..].starts_with('(')
                && let Some(rel_paren) = s[close + 2..].find(')')
            {
                out.push_str(&s[i + 1..close]);
                i = close + 2 + rel_paren + 1;
                continue;
            }
        }
        // Copy a whole UTF-8 char starting at i.
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&s[i..i + ch_len]);
        i += ch_len;
    }
    out
}

fn utf8_len(first: u8) -> usize {
    match first {
        b if b < 0x80 => 1,
        b if b >> 5 == 0b110 => 2,
        b if b >> 4 == 0b1110 => 3,
        _ => 4,
    }
}

// ── Main-content selection ────────────────────────────────────────────────

fn select_main(html: &str) -> &str {
    if let Some(inner) = first_element_inner(html, "main") {
        return inner;
    }
    if let Some(inner) = first_element_inner(html, "body") {
        return inner;
    }
    html
}

/// Return the inner slice of the first `<tag ...> ... </tag>` (case-insensitive).
fn first_element_inner<'a>(html: &'a str, tag: &str) -> Option<&'a str> {
    let lower = html.to_ascii_lowercase();
    let open_marker = format!("<{tag}");
    let open_pos = lower.find(&open_marker)?;
    // The char after the tag name must be a delimiter, not more name chars.
    let after_name = open_pos + open_marker.len();
    let delim_ok = lower[after_name..]
        .chars()
        .next()
        .is_some_and(|c| c == '>' || c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '/');
    if !delim_ok {
        return None;
    }
    let gt = lower[open_pos..].find('>')? + open_pos;
    let close_marker = format!("</{tag}");
    let close_pos = lower[gt + 1..].find(&close_marker).map(|p| gt + 1 + p)?;
    Some(&html[gt + 1..close_pos])
}

fn extract_title(html: &str) -> Option<String> {
    let inner = first_element_inner(html, "title")?;
    let decoded = decode_entities(inner);
    let collapsed = collapse_ws(&decoded);
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// ── Tokenizer ─────────────────────────────────────────────────────────────

enum Token<'a> {
    Open {
        name: String,
        attrs: &'a str,
        self_closing: bool,
    },
    Close {
        name: String,
    },
    Text(&'a str),
}

fn tokenize(html: &str) -> Vec<Token<'_>> {
    let bytes = html.as_bytes();
    let n = bytes.len();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < n {
        if bytes[i] == b'<' {
            if html[i..].starts_with("<!--") {
                match html[i + 4..].find("-->") {
                    Some(end) => i = i + 4 + end + 3,
                    None => break,
                }
                continue;
            }
            if i + 1 < n && bytes[i + 1] == b'!' {
                match html[i..].find('>') {
                    Some(end) => i += end + 1,
                    None => break,
                }
                continue;
            }
            if let Some(end) = tag_end(bytes, i) {
                parse_tag(&html[i + 1..end], &mut tokens);
                i = end + 1;
            } else {
                tokens.push(Token::Text(&html[i..]));
                break;
            }
        } else {
            let start = i;
            while i < n && bytes[i] != b'<' {
                i += 1;
            }
            tokens.push(Token::Text(&html[start..i]));
        }
    }
    tokens
}

/// Index of the `>` that closes a tag opened at `start`, honoring quotes.
fn tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    let mut quote = 0u8;
    while i < bytes.len() {
        let b = bytes[i];
        if quote != 0 {
            if b == quote {
                quote = 0;
            }
        } else if b == b'"' || b == b'\'' {
            quote = b;
        } else if b == b'>' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn parse_tag<'a>(inner: &'a str, tokens: &mut Vec<Token<'a>>) {
    let trimmed = inner.trim_start();
    if let Some(rest) = trimmed.strip_prefix('/') {
        let name = take_name(rest);
        if !name.is_empty() {
            tokens.push(Token::Close { name });
        }
        return;
    }
    let name = take_name(trimmed);
    if name.is_empty() {
        return;
    }
    let attrs = &trimmed[name.len()..];
    let self_closing = trimmed.trim_end().ends_with('/');
    tokens.push(Token::Open {
        name,
        attrs,
        self_closing,
    });
}

fn take_name(s: &str) -> String {
    s.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == ':')
        .collect::<String>()
        .to_ascii_lowercase()
}

fn get_attr(attrs: &str, key: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(key) {
        let idx = from + pos;
        let boundary = idx == 0 || lower.as_bytes()[idx - 1].is_ascii_whitespace();
        let after = idx + key.len();
        let rest = attrs[after..].trim_start();
        if boundary && rest.starts_with('=') {
            return Some(parse_attr_value(rest[1..].trim_start()));
        }
        from = after;
    }
    None
}

fn parse_attr_value(s: &str) -> String {
    let bytes = s.as_bytes();
    if let Some(&q) = bytes.first()
        && (q == b'"' || q == b'\'')
    {
        let quote = q as char;
        return match s[1..].find(quote) {
            Some(end) => s[1..=end].to_string(),
            None => s[1..].to_string(),
        };
    }
    s.split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string()
}

// ── Renderer ──────────────────────────────────────────────────────────────

struct ListCtx {
    ordered: bool,
    index: usize,
}

/// Accumulates a `<table>` so it can be flushed as a GitHub-Flavored-Markdown
/// table (leading `|`, header separator row) instead of the ambiguous
/// `cell | cell |` lines the streaming renderer produced before.
#[derive(Default)]
struct TableCtx {
    /// Completed rows, each a list of finalized (escaped) cell strings.
    rows: Vec<Vec<String>>,
    /// Cells of the row currently being built.
    cur_row: Vec<String>,
    /// Active cell buffer; text/inline content is routed here while open.
    cell: Option<String>,
    /// Index into `rows` of the row to use as the header, if known.
    header_idx: Option<usize>,
    /// Inside a `<thead>` section.
    in_thead: bool,
    /// The in-progress row contains at least one `<th>`.
    cur_row_has_th: bool,
}

#[derive(Default)]
struct Renderer {
    out: String,
    links: Vec<Link>,
    skip_depth: usize,
    pre_depth: usize,
    anchor: Option<(String, String)>,
    list_stack: Vec<ListCtx>,
    table_stack: Vec<TableCtx>,
}

impl Renderer {
    fn consume(&mut self, token: &Token<'_>) {
        match token {
            Token::Text(t) => self.text(t),
            Token::Open {
                name,
                attrs,
                self_closing,
            } => self.open(name, attrs, *self_closing),
            Token::Close { name } => self.close(name),
        }
    }

    fn text(&mut self, raw: &str) {
        if self.skip_depth > 0 {
            return;
        }
        let decoded = decode_entities(raw);
        if self.pre_depth > 0 {
            self.active_sink().push_str(&decoded);
            return;
        }
        let collapsed = collapse_ws(&decoded);
        if collapsed.is_empty() {
            return;
        }
        self.active_sink().push_str(&collapsed);
    }

    /// The string buffer inline content should currently be written to:
    /// an open `<a>` anchor first, then the innermost open table cell, else the
    /// main document body. Keeps link/cell capture working when nested.
    fn active_sink(&mut self) -> &mut String {
        if let Some((_, buf)) = self.anchor.as_mut() {
            return buf;
        }
        // Probe immutably first so that borrow is released before the mutable
        // re-borrow returns (keeps the borrow checker happy across fields).
        let cell_active = matches!(self.table_stack.last(), Some(tc) if tc.cell.is_some());
        if cell_active {
            return self
                .table_stack
                .last_mut()
                .and_then(|tc| tc.cell.as_mut())
                .expect("cell active by the check above");
        }
        &mut self.out
    }

    fn open(&mut self, name: &str, attrs: &str, self_closing: bool) {
        if self.skip_depth > 0 {
            if is_skip(name) && !self_closing && !is_void(name) {
                self.skip_depth += 1;
            }
            return;
        }
        if is_skip(name) {
            if !self_closing && !is_void(name) {
                self.skip_depth += 1;
            }
            return;
        }
        if self_closing || is_void(name) {
            self.open_void(name);
            return;
        }

        match name {
            "a" => self.open_anchor(attrs),
            "pre" => {
                self.block_break();
                self.out.push_str("```");
                self.newline();
                self.pre_depth += 1;
            }
            "code" if self.pre_depth == 0 => {
                self.active_sink().push('`');
            }
            "table" => {
                self.block_break();
                self.table_stack.push(TableCtx::default());
            }
            "thead" => {
                if let Some(tc) = self.table_stack.last_mut() {
                    tc.in_thead = true;
                }
            }
            "tr" => {
                if let Some(tc) = self.table_stack.last_mut() {
                    tc.cur_row = Vec::new();
                    tc.cur_row_has_th = false;
                } else {
                    // A stray <tr> outside any table: keep the legacy line break.
                    self.newline();
                }
            }
            "td" => {
                if let Some(tc) = self.table_stack.last_mut() {
                    tc.cell = Some(String::new());
                }
            }
            "th" => {
                if let Some(tc) = self.table_stack.last_mut() {
                    tc.cell = Some(String::new());
                    tc.cur_row_has_th = true;
                }
            }
            "ul" => {
                self.list_stack.push(ListCtx {
                    ordered: false,
                    index: 0,
                });
                self.block_break();
            }
            "ol" => {
                self.list_stack.push(ListCtx {
                    ordered: true,
                    index: 0,
                });
                self.block_break();
            }
            "li" => {
                self.newline();
                let marker = match self.list_stack.last_mut() {
                    Some(ctx) if ctx.ordered => {
                        ctx.index += 1;
                        format!("{}. ", ctx.index)
                    }
                    _ => "- ".to_string(),
                };
                self.out.push_str(&marker);
            }
            "blockquote" => {
                self.block_break();
                self.out.push_str("> ");
            }
            h if is_heading(h) => {
                self.block_break();
                for _ in 0..heading_level(h) {
                    self.out.push('#');
                }
                self.out.push(' ');
            }
            b if is_block(b) => self.block_break(),
            _ => {}
        }
    }

    fn open_void(&mut self, name: &str) {
        match name {
            "br" => self.newline(),
            "hr" => {
                self.block_break();
                self.out.push_str("---");
                self.block_break();
            }
            _ => {}
        }
    }

    fn open_anchor(&mut self, attrs: &str) {
        if self.anchor.is_some() {
            return;
        }
        if let Some(href) = get_attr(attrs, "href") {
            let href = href.trim();
            if !href.is_empty() && !href.starts_with("javascript:") && !href.starts_with('#') {
                self.anchor = Some((href.to_string(), String::new()));
            }
        }
    }

    fn close(&mut self, name: &str) {
        if self.skip_depth > 0 {
            if is_skip(name) {
                self.skip_depth -= 1;
            }
            return;
        }
        match name {
            "a" => {
                if let Some((href, text)) = self.anchor.take() {
                    let text = text.trim();
                    if !text.is_empty() {
                        self.out.push_str(&format!("[{text}]({href})"));
                        self.links.push(Link {
                            text: text.to_string(),
                            href,
                        });
                    }
                }
            }
            "pre" => {
                self.pre_depth = self.pre_depth.saturating_sub(1);
                self.newline();
                self.out.push_str("```");
                self.block_break();
            }
            "code" if self.pre_depth == 0 => {
                self.active_sink().push('`');
            }
            "ul" | "ol" => {
                self.list_stack.pop();
                self.block_break();
            }
            "td" | "th" => {
                if let Some(tc) = self.table_stack.last_mut()
                    && let Some(cell) = tc.cell.take()
                {
                    tc.cur_row.push(finalize_cell(&cell));
                }
            }
            "tr" => {
                if let Some(tc) = self.table_stack.last_mut() {
                    let row = std::mem::take(&mut tc.cur_row);
                    if !row.is_empty() {
                        if tc.header_idx.is_none() && (tc.in_thead || tc.cur_row_has_th) {
                            tc.header_idx = Some(tc.rows.len());
                        }
                        tc.rows.push(row);
                    }
                }
            }
            "thead" => {
                if let Some(tc) = self.table_stack.last_mut() {
                    tc.in_thead = false;
                }
            }
            "table" => {
                if let Some(tc) = self.table_stack.pop() {
                    let rendered = render_gfm_table(&tc);
                    if rendered.is_empty() {
                        return;
                    }
                    // A nested table flushes into its parent cell; a top-level
                    // one becomes its own block in the document body.
                    if self.table_stack.last().is_some_and(|p| p.cell.is_some()) {
                        self.active_sink().push_str(&rendered);
                    } else {
                        self.block_break();
                        self.out.push_str(&rendered);
                        self.block_break();
                    }
                }
            }
            h if is_heading(h) => self.block_break(),
            b if is_block(b) => self.block_break(),
            _ => {}
        }
    }

    fn newline(&mut self) {
        if !self.out.ends_with('\n') {
            self.out.push('\n');
        }
    }

    fn block_break(&mut self) {
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        if self.out.is_empty() {
            return;
        }
        if self.out.ends_with("\n\n") {
            return;
        }
        if self.out.ends_with('\n') {
            self.out.push('\n');
        } else {
            self.out.push_str("\n\n");
        }
    }
}

/// Clean a raw cell buffer into a single-line, pipe-escaped GFM cell.
fn finalize_cell(raw: &str) -> String {
    let single_line = raw.replace(['\n', '\r'], " ");
    collapse_ws(&single_line).trim().replace('|', "\\|")
}

/// Render an accumulated [`TableCtx`] as a GitHub-Flavored-Markdown table.
///
/// Guarantees a header row + separator (GFM requires both): an explicit
/// `<thead>`/`<th>` row is used when present, otherwise the first row becomes
/// the header. Rows are padded to a uniform column count. Returns an empty
/// string for an empty table so the caller can skip it.
fn render_gfm_table(tc: &TableCtx) -> String {
    if tc.rows.is_empty() {
        return String::new();
    }
    let cols = tc.rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return String::new();
    }

    let header_idx = tc.header_idx.unwrap_or(0);
    let header = tc.rows.get(header_idx).cloned().unwrap_or_default();

    let mut out = String::new();
    push_table_row(&mut out, &header, cols);
    push_separator_row(&mut out, cols);
    for (i, row) in tc.rows.iter().enumerate() {
        if i == header_idx {
            continue;
        }
        push_table_row(&mut out, row, cols);
    }
    out.trim_end().to_string()
}

fn push_table_row(out: &mut String, cells: &[String], cols: usize) {
    out.push('|');
    for c in 0..cols {
        let cell = cells.get(c).map_or("", String::as_str);
        out.push(' ');
        out.push_str(cell);
        out.push_str(" |");
    }
    out.push('\n');
}

fn push_separator_row(out: &mut String, cols: usize) {
    out.push('|');
    for _ in 0..cols {
        out.push_str(" --- |");
    }
    out.push('\n');
}

fn is_skip(name: &str) -> bool {
    matches!(
        name,
        "script"
            | "style"
            | "noscript"
            | "svg"
            | "template"
            | "iframe"
            | "head"
            | "object"
            | "embed"
            | "canvas"
            | "math"
    )
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "br" | "hr"
            | "img"
            | "input"
            | "meta"
            | "link"
            | "source"
            | "col"
            | "area"
            | "base"
            | "wbr"
            | "track"
            | "param"
    )
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "section"
            | "article"
            | "main"
            | "header"
            | "footer"
            | "aside"
            | "nav"
            | "dl"
            | "dd"
            | "dt"
            | "figure"
            | "figcaption"
            | "address"
            | "form"
            | "fieldset"
            | "details"
            | "summary"
    )
}

fn is_heading(name: &str) -> bool {
    name.len() == 2 && name.starts_with('h') && matches!(name.as_bytes()[1], b'1'..=b'6')
}

fn heading_level(name: &str) -> usize {
    (name.as_bytes()[1] - b'0') as usize
}

// ── Whitespace + entities ──────────────────────────────────────────────────

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

fn normalize(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_fence = false;
    let mut blank_run = 0;

    for line in s.lines() {
        if line.trim() == "```" {
            in_fence = !in_fence;
            result.push_str("```\n");
            blank_run = 0;
            continue;
        }
        if in_fence {
            result.push_str(line);
            result.push('\n');
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                result.push('\n');
            }
            continue;
        }
        blank_run = 0;
        result.push_str(trimmed);
        result.push('\n');
    }
    result.trim().to_string()
}

/// Decode HTML/XML character entities (`&amp;`, `&#39;`, `&#x2019;`, …).
///
/// Exposed for sibling modules (e.g. the `YouTube` srv3 transcript parser) so
/// entity handling lives in exactly one place.
#[must_use]
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some(rel_end) = s[i..].find(';') {
                let end = i + rel_end;
                let entity = &s[i + 1..end];
                if let Some(decoded) = decode_one(entity) {
                    out.push_str(&decoded);
                    i = end + 1;
                    continue;
                }
            }
            out.push('&');
            i += 1;
        } else {
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&s[i..i + ch_len]);
            i += ch_len;
        }
    }
    out
}

fn decode_one(entity: &str) -> Option<String> {
    if let Some(num) = entity.strip_prefix('#') {
        let code = if let Some(hex) = num.strip_prefix(['x', 'X']) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        return char::from_u32(code).map(|c| c.to_string());
    }
    let named = match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "laquo" => "«",
        "raquo" => "»",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "bull" => "•",
        "middot" => "·",
        "euro" => "€",
        "pound" => "£",
        "deg" => "°",
        "times" => "×",
        "divide" => "÷",
        _ => return None,
    };
    Some(named.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_title_and_decodes() {
        let doc =
            parse("<html><head><title>Foo &amp; Bar</title></head><body><p>Hi</p></body></html>");
        assert_eq!(doc.title.as_deref(), Some("Foo & Bar"));
    }

    #[test]
    fn drops_script_and_style() {
        let html = "<body><script>var x=1;</script><style>.a{}</style><p>Visible</p></body>";
        let doc = parse(html);
        assert_eq!(doc.markdown, "Visible");
        assert!(!doc.markdown.contains("var x"));
    }

    #[test]
    fn renders_headings_and_paragraphs() {
        let html = "<body><h1>Title</h1><p>First.</p><p>Second.</p></body>";
        let doc = parse(html);
        assert_eq!(doc.markdown, "# Title\n\nFirst.\n\nSecond.");
    }

    #[test]
    fn renders_links_and_collects_them() {
        let html = r#"<body><p>See <a href="https://x.com/a">the site</a> now.</p></body>"#;
        let doc = parse(html);
        assert!(doc.markdown.contains("[the site](https://x.com/a)"));
        assert_eq!(doc.links.len(), 1);
        assert_eq!(doc.links[0].href, "https://x.com/a");
        assert_eq!(doc.links[0].text, "the site");
    }

    #[test]
    fn renders_unordered_and_ordered_lists() {
        let html = "<body><ul><li>one</li><li>two</li></ul><ol><li>a</li><li>b</li></ol></body>";
        let doc = parse(html);
        assert!(doc.markdown.contains("- one"));
        assert!(doc.markdown.contains("- two"));
        assert!(doc.markdown.contains("1. a"));
        assert!(doc.markdown.contains("2. b"));
    }

    #[test]
    fn prefers_main_over_chrome() {
        let html = "<body><nav><a href=/x>menu</a></nav><main><p>Core content</p></main><footer>foot</footer></body>";
        let doc = parse(html);
        assert_eq!(doc.markdown, "Core content");
    }

    #[test]
    fn preserves_pre_as_fenced_code() {
        let html = "<body><pre>line1\n  line2</pre></body>";
        let doc = parse(html);
        assert!(doc.markdown.contains("```"));
        assert!(doc.markdown.contains("line1\n  line2"));
    }

    #[test]
    fn renders_table_as_gfm_with_header_separator() {
        let html = "<body><table>\
            <thead><tr><th>A</th><th>B</th></tr></thead>\
            <tbody><tr><td>1</td><td>2</td></tr><tr><td>3</td><td>4</td></tr></tbody>\
            </table></body>";
        let doc = parse(html);
        assert_eq!(
            doc.markdown, "| A | B |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |",
            "thead row must become the GFM header with a separator row"
        );
    }

    #[test]
    fn table_without_thead_promotes_first_row_to_header() {
        let html = "<body><table><tr><td>h1</td><td>h2</td></tr>\
            <tr><td>a</td><td>b</td></tr></table></body>";
        let doc = parse(html);
        assert_eq!(doc.markdown, "| h1 | h2 |\n| --- | --- |\n| a | b |");
    }

    #[test]
    fn table_cells_escape_pipes_and_keep_links() {
        let html = r#"<body><table><tr><th>name</th><th>url</th></tr>
            <tr><td>a|b</td><td><a href="https://x.com/p">site</a></td></tr></table></body>"#;
        let doc = parse(html);
        assert!(doc.markdown.contains(r"| a\|b |"), "pipe must be escaped");
        assert!(
            doc.markdown.contains("[site](https://x.com/p)"),
            "links inside cells must render: {}",
            doc.markdown
        );
        assert_eq!(doc.links.len(), 1, "cell links are still collected");
    }

    #[test]
    fn ragged_table_rows_are_padded() {
        let html = "<body><table><tr><th>a</th><th>b</th><th>c</th></tr>\
            <tr><td>1</td></tr></table></body>";
        let doc = parse(html);
        // Short row padded to 3 columns so the GFM grid stays valid.
        assert_eq!(
            doc.markdown,
            "| a | b | c |\n| --- | --- | --- |\n| 1 |  |  |"
        );
    }

    #[test]
    fn markdown_to_text_strips_markup() {
        let md = "# Heading\n\nSee [link](https://x.com) here.";
        let text = markdown_to_text(md);
        assert_eq!(text, "Heading\n\nSee link here.");
    }

    #[test]
    fn handles_unterminated_tag_gracefully() {
        let doc = parse("<body><p>ok</p><broken");
        assert!(doc.markdown.contains("ok"));
    }

    #[test]
    fn decodes_numeric_entities() {
        assert_eq!(decode_entities("A&#38;B&#x41;"), "A&BA");
    }
}
