//! RFC-822/2822 email (`.eml`) → header summary + body text (EPIC 12.13).
//!
//! Splits the message into the header block and body at the first blank line,
//! unfolds multi-line headers, surfaces the salient headers (From/To/Subject/
//! Date), and — for `multipart/*` — extracts the `text/plain` parts so an LLM
//! sees the message, not MIME boilerplate. Degrades gracefully: input without a
//! header/body split is treated as a body so non-empty input is never dropped.

/// A parsed email: salient headers (in order) and decoded body text.
#[derive(Debug, Clone, Default)]
pub struct Email {
    pub headers: Vec<(String, String)>,
    pub body: String,
}

const SALIENT: [&str; 5] = ["from", "to", "cc", "subject", "date"];

/// Parse an `.eml` document into salient headers + a text body.
#[must_use]
pub fn parse(input: &str) -> Email {
    let normalized = input.replace("\r\n", "\n");
    let (header_block, body_block) = match normalized.split_once("\n\n") {
        Some((h, b)) if looks_like_headers(h) => (h, b),
        // No header/body split (or block isn't headers): treat all as body.
        _ => ("", normalized.as_str()),
    };

    let all_headers = parse_headers(header_block);
    let content_type = header_value(&all_headers, "content-type").unwrap_or_default();

    let body = if content_type.contains("multipart/") {
        extract_plain_parts(&content_type, body_block)
    } else {
        body_block.trim().to_string()
    };

    let headers = all_headers
        .into_iter()
        .filter(|(k, _)| SALIENT.contains(&k.to_ascii_lowercase().as_str()))
        .collect();

    Email { headers, body }
}

/// Render the email as a header summary followed by the body.
#[must_use]
pub fn to_text(input: &str) -> String {
    let email = parse(input);
    let mut out = String::new();
    for (k, v) in &email.headers {
        out.push_str(&format!("{k}: {v}\n"));
    }
    if !email.headers.is_empty() && !email.body.is_empty() {
        out.push('\n');
    }
    out.push_str(&email.body);
    out.trim().to_string()
}

/// Chunks: the header summary as one chunk, then body paragraphs.
#[must_use]
pub fn chunks(input: &str) -> Vec<String> {
    let email = parse(input);
    let mut out = Vec::new();
    if !email.headers.is_empty() {
        let header_block = email
            .headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join("\n");
        out.push(header_block);
    }
    out.extend(super::paragraph_chunks(&email.body));
    out.retain(|c| !c.trim().is_empty());
    if out.is_empty() {
        let trimmed = input.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// A header block looks like headers if its first non-empty line is `Name: …`.
fn looks_like_headers(block: &str) -> bool {
    block
        .lines()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| {
            l.split_once(':')
                .is_some_and(|(name, _)| !name.is_empty() && name.chars().all(is_header_name_char))
        })
}

fn is_header_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

/// Parse a header block, unfolding continuation lines (leading whitespace).
fn parse_headers(block: &str) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in block.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some(last) = headers.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
        } else if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    headers
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

/// Extract concatenated `text/plain` parts from a multipart body.
fn extract_plain_parts(content_type: &str, body: &str) -> String {
    let Some(boundary) = boundary_of(content_type) else {
        return body.trim().to_string();
    };
    let sep = format!("--{boundary}");
    let mut parts = Vec::new();
    for raw in body.split(&sep) {
        let part = raw.trim_start_matches('\n');
        let Some((part_headers, part_body)) = part.split_once("\n\n") else {
            continue;
        };
        let ct = part_headers.to_ascii_lowercase();
        if ct.contains("text/plain")
            || (!ct.contains("content-type") && !part_body.trim().is_empty())
        {
            let text = part_body.trim();
            if !text.is_empty() {
                parts.push(text.to_string());
            }
        }
    }
    if parts.is_empty() {
        body.trim().to_string()
    } else {
        parts.join("\n\n")
    }
}

fn boundary_of(content_type: &str) -> Option<String> {
    let lower = content_type.to_ascii_lowercase();
    let idx = lower.find("boundary=")?;
    let rest = &content_type[idx + "boundary=".len()..];
    let rest = rest.trim();
    let b = rest
        .strip_prefix('"')
        .and_then(|r| r.split('"').next())
        .unwrap_or_else(|| rest.split([';', ' ', '\n']).next().unwrap_or(rest));
    (!b.is_empty()).then(|| b.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_headers_and_body() {
        let eml = "From: a@x.com\nTo: b@y.com\nSubject: Hi\n\nHello there.\n";
        let email = parse(eml);
        assert_eq!(header_value(&email.headers, "subject").unwrap(), "Hi");
        assert_eq!(email.body, "Hello there.");
    }

    #[test]
    fn unfolds_continuation_headers() {
        let eml = "Subject: a very\n long subject\n\nbody";
        let email = parse(eml);
        assert_eq!(
            header_value(&email.headers, "subject").unwrap(),
            "a very long subject"
        );
    }

    #[test]
    fn extracts_plain_from_multipart() {
        let eml = "Content-Type: multipart/alternative; boundary=\"BB\"\n\n--BB\nContent-Type: text/plain\n\nplain body\n--BB\nContent-Type: text/html\n\n<p>html</p>\n--BB--";
        let email = parse(eml);
        assert!(email.body.contains("plain body"));
        assert!(!email.body.contains("<p>"));
    }

    #[test]
    fn plain_text_without_headers_is_body() {
        let email = parse("just a single line");
        assert!(email.headers.is_empty());
        assert_eq!(email.body, "just a single line");
        assert_eq!(chunks("just a single line"), vec!["just a single line"]);
    }
}
