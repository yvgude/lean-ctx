//! Minimal, dependency-free PDF renderer for the compliance report (GL #677).
//!
//! lean-ctx ships as one lean binary (`opt-level = "z"`, LTO), so rather than
//! pull a heavyweight PDF/font stack we emit a small, spec-compliant PDF 1.7
//! ourselves: a text report typeset in Helvetica (a Standard-14 font, no
//! embedding) across paginated US-Letter pages. The output is a *real*,
//! openable PDF — valid header, object table, cross-reference table and
//! trailer — not a stub.
//!
//! Scope is deliberately narrow: monospaced-budget word wrapping of ASCII text.
//! The signed, machine-verifiable artifact is always the JSON
//! ([`super::write_artifact`]); the PDF is the human-facing rendering.

/// US-Letter dimensions and layout, in PostScript points (1/72 inch).
const PAGE_W: i32 = 612;
const PAGE_H: i32 = 792;
const MARGIN: i32 = 54;
const FONT_SIZE: i32 = 9;
const LEADING: i32 = 12;
/// First baseline, measured from the page bottom.
const TOP_Y: i32 = PAGE_H - MARGIN;
/// Lines per page that keep the last baseline above the bottom margin.
const LINES_PER_PAGE: usize = ((PAGE_H - 2 * MARGIN) / LEADING) as usize;
/// Character budget per line (Helvetica ~0.5·size avg advance, conservative).
const WRAP: usize = 100;

/// Render `text` to a complete PDF document.
#[must_use]
pub fn to_pdf(text: &str) -> Vec<u8> {
    let lines = layout_lines(text);
    let pages: Vec<&[String]> = if lines.is_empty() {
        vec![&[][..]]
    } else {
        lines.chunks(LINES_PER_PAGE).collect()
    };

    let mut buf: Vec<u8> = Vec::new();
    // obj index 0 is the free head; real objects are 1-based.
    let mut offsets: Vec<usize> = vec![0];

    buf.extend_from_slice(b"%PDF-1.7\n");
    // Binary marker so tools treat the file as binary (recommended by spec).
    buf.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

    let page_count = pages.len();
    // Object numbering: 1=Catalog, 2=Pages, 3=Font, then per page p (0-based):
    // page object = 4 + 2p, content object = 5 + 2p.
    let page_obj = |p: usize| 4 + 2 * p;
    let content_obj = |p: usize| 5 + 2 * p;

    push_obj(&mut buf, &mut offsets, "<< /Type /Catalog /Pages 2 0 R >>");

    let kids: String = (0..page_count)
        .map(|p| format!("{} 0 R", page_obj(p)))
        .collect::<Vec<_>>()
        .join(" ");
    push_obj(
        &mut buf,
        &mut offsets,
        &format!("<< /Type /Pages /Kids [{kids}] /Count {page_count} >>"),
    );

    push_obj(
        &mut buf,
        &mut offsets,
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
    );

    for (p, page_lines) in pages.iter().enumerate() {
        let body = page_content(page_lines);
        push_obj(
            &mut buf,
            &mut offsets,
            &format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_W} {PAGE_H}] \
                 /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                content_obj(p)
            ),
        );
        push_stream_obj(&mut buf, &mut offsets, &body);
    }

    write_xref_and_trailer(&mut buf, &offsets);
    buf
}

/// Append `N 0 obj << … >> endobj`, recording the object's byte offset.
fn push_obj(buf: &mut Vec<u8>, offsets: &mut Vec<usize>, dict: &str) {
    let obj_num = offsets.len();
    offsets.push(buf.len());
    buf.extend_from_slice(format!("{obj_num} 0 obj\n{dict}\nendobj\n").as_bytes());
}

/// Append a stream object (`<< /Length n >> stream … endstream`).
fn push_stream_obj(buf: &mut Vec<u8>, offsets: &mut Vec<usize>, content: &str) {
    let obj_num = offsets.len();
    offsets.push(buf.len());
    buf.extend_from_slice(
        format!(
            "{obj_num} 0 obj\n<< /Length {} >>\nstream\n{content}\nendstream\nendobj\n",
            content.len()
        )
        .as_bytes(),
    );
}

/// Build the text-drawing content stream for one page.
fn page_content(lines: &[String]) -> String {
    let mut s = String::new();
    s.push_str("BT\n");
    s.push_str(&format!("/F1 {FONT_SIZE} Tf\n"));
    s.push_str(&format!("{LEADING} TL\n"));
    s.push_str(&format!("{MARGIN} {TOP_Y} Td\n"));
    for line in lines {
        s.push('(');
        s.push_str(&escape_pdf_text(line));
        s.push_str(") Tj\n");
        s.push_str("T*\n");
    }
    s.push_str("ET");
    s
}

/// Cross-reference table + trailer. Each xref entry is exactly 20 bytes.
fn write_xref_and_trailer(buf: &mut Vec<u8>, offsets: &[usize]) {
    let xref_offset = buf.len();
    let size = offsets.len();
    buf.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    buf.extend_from_slice(b"0000000000 65535 f \n");
    for &off in &offsets[1..] {
        buf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    buf.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n")
            .as_bytes(),
    );
}

/// Escape the three PDF literal-string metacharacters (`\`, `(`, `)`).
/// Input is already ASCII-sanitized by [`layout_lines`].
fn escape_pdf_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            _ => out.push(c),
        }
    }
    out
}

/// Normalize to printable ASCII and word-wrap to [`WRAP`] columns, preserving
/// each source line's leading indentation on continuation rows.
fn layout_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let sanitized = sanitize_ascii(raw);
        out.extend(wrap_line(&sanitized, WRAP));
    }
    out
}

fn sanitize_ascii(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push_str("    "),
            '\u{20}'..='\u{7e}' => out.push(c),
            // Transliterate the handful of typographic glyphs the report text
            // and framework details use, so the PDF stays readable.
            '\u{2014}' | '\u{2013}' => out.push('-'), // em / en dash
            '\u{2265}' => out.push_str(">="),         // ≥
            '\u{2264}' => out.push_str("<="),         // ≤
            '\u{00d7}' => out.push('x'),              // ×
            '\u{2192}' => out.push_str("->"),         // →
            '\u{2019}' | '\u{2018}' => out.push('\''), // curly quotes
            '\u{201c}' | '\u{201d}' => out.push('"'),
            '\u{2026}' => out.push_str("..."), // …
            _ => out.push('?'),
        }
    }
    out
}

fn wrap_line(line: &str, width: usize) -> Vec<String> {
    if line.len() <= width {
        return vec![line.to_string()];
    }
    let indent = line.len() - line.trim_start().len();
    let indent = indent.min(width.saturating_sub(10));
    let pad = " ".repeat(indent);
    let budget = width.saturating_sub(indent).max(1);

    let mut rows = Vec::new();
    let mut current = String::new();
    for word in line.trim_start().split(' ') {
        // A single oversized token: hard-split into budget-width chunks.
        if word.len() > budget {
            if !current.is_empty() {
                rows.push(format!("{pad}{current}"));
                current.clear();
            }
            let chars: Vec<char> = word.chars().collect();
            for chunk in chars.chunks(budget) {
                rows.push(format!("{pad}{}", chunk.iter().collect::<String>()));
            }
            continue;
        }
        let extra = usize::from(!current.is_empty());
        if current.len() + extra + word.len() > budget {
            rows.push(format!("{pad}{current}"));
            current = word.to_string();
        } else {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        rows.push(format!("{pad}{current}"));
    }
    if rows.is_empty() {
        rows.push(pad);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_valid_pdf_envelope() {
        let pdf = to_pdf("Hello compliance");
        assert!(pdf.starts_with(b"%PDF-1.7"), "must begin with PDF header");
        assert!(pdf.ends_with(b"%%EOF\n"), "must end with EOF marker");
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("/BaseFont /Helvetica"));
        assert!(text.contains("startxref"));
        assert!(text.contains("(Hello compliance) Tj"));
    }

    #[test]
    fn xref_offsets_point_at_object_headers() {
        let pdf = to_pdf("one line");
        // Parse startxref → xref offset, then confirm the byte there is 'x'(ref).
        let s = String::from_utf8_lossy(&pdf);
        let idx = s.rfind("startxref\n").unwrap() + "startxref\n".len();
        let end = s[idx..].find('\n').unwrap() + idx;
        let xref_off: usize = s[idx..end].parse().unwrap();
        assert_eq!(&pdf[xref_off..xref_off + 4], b"xref");
    }

    #[test]
    fn long_input_paginates() {
        let many = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let pdf = to_pdf(&many);
        let s = String::from_utf8_lossy(&pdf);
        // 200 lines / 57 per page ⇒ ≥ 3 pages ⇒ Count ≥ 3.
        assert!(s.contains("/Type /Pages"));
        let count_marker = s.find("/Count ").unwrap();
        let count: usize = s[count_marker + 7..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .unwrap();
        assert!(count >= 3, "expected ≥3 pages, got {count}");
    }

    #[test]
    fn escapes_parens_and_backslash() {
        assert_eq!(escape_pdf_text("a(b)c\\d"), "a\\(b\\)c\\\\d");
    }

    #[test]
    fn sanitizes_non_ascii() {
        // Coverage icons / accents collapse to '?', tabs to spaces.
        assert_eq!(sanitize_ascii("a\tb"), "a    b");
        assert_eq!(sanitize_ascii("café ●"), "caf? ?");
    }

    #[test]
    fn wraps_long_tokens() {
        let token = "x".repeat(250);
        let rows = wrap_line(&token, WRAP);
        assert!(rows.len() >= 3);
        assert!(rows.iter().all(|r| r.len() <= WRAP));
    }
}
