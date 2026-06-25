//! PDF → text extraction for the research context layer.
//!
//! Delegates to the `pdf-extract` crate. Because PDF parsers can panic on
//! malformed or unusual input — and `ctx_url_read` accepts arbitrary
//! agent-supplied URLs — extraction is wrapped in [`std::panic::catch_unwind`]
//! so a bad document yields an error instead of taking down the handler.

/// Extract and normalize the text content of a PDF byte buffer.
pub fn extract_text(bytes: &[u8]) -> Result<String, String> {
    if !looks_like_pdf(bytes) {
        return Err("response is not a PDF (missing %PDF header)".to_string());
    }

    let outcome = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes));
    match outcome {
        Ok(Ok(text)) => {
            let normalized = normalize(&text);
            if normalized.trim().is_empty() {
                Err("PDF contained no extractable text (likely scanned/image-only)".to_string())
            } else {
                Ok(normalized)
            }
        }
        Ok(Err(e)) => Err(format!("PDF text extraction failed: {e}")),
        Err(_) => Err("PDF text extraction panicked (malformed or unsupported PDF)".to_string()),
    }
}

/// PDFs start with `%PDF-` (optionally after a small BOM/whitespace preamble).
#[must_use]
pub fn looks_like_pdf(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    head.windows(5).any(|w| w == b"%PDF-")
}

/// Collapse the runs of blank lines `pdf-extract` tends to emit and trim
/// trailing whitespace per line.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0usize;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
            out.push('\n');
        } else {
            blank_run = 0;
            out.push_str(trimmed);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_pdf_bytes() {
        assert!(extract_text(b"<html>not a pdf</html>").is_err());
    }

    #[test]
    fn detects_pdf_header() {
        assert!(looks_like_pdf(b"%PDF-1.7\n..."));
        assert!(!looks_like_pdf(b"plain text"));
    }

    #[test]
    fn normalize_collapses_blank_runs() {
        assert_eq!(normalize("a\n\n\n\nb\n\n"), "a\n\nb");
        assert_eq!(normalize("  x  \n   \n y "), "x\n\ny");
    }
}
