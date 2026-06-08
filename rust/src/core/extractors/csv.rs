//! CSV/TSV → record text + row-group chunks (EPIC 12.13).
//!
//! A small RFC-4180-aware parser (quoted fields, escaped `""`, embedded
//! delimiters/newlines) turns tabular data into `header: value` records so an
//! LLM sees labeled fields, not bare columns. Chunks are row groups, each
//! prefixed with the header for standalone context. Degrades gracefully: any
//! non-empty input yields at least one non-empty chunk.

/// Rows per chunk — keeps each chunk small while preserving header context.
const ROWS_PER_CHUNK: usize = 20;

/// Parse delimited text into rows of fields (RFC-4180 quoting aware).
#[must_use]
pub fn parse(input: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut field = String::new();
    let mut record: Vec<String> = Vec::new();
    let mut in_quotes = false;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(ch);
            }
        } else if ch == '"' {
            in_quotes = true;
        } else if ch == delimiter {
            record.push(std::mem::take(&mut field));
        } else if ch == '\n' || ch == '\r' {
            // Swallow a CRLF pair as one terminator.
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            record.push(std::mem::take(&mut field));
            push_record(&mut rows, std::mem::take(&mut record));
        } else {
            field.push(ch);
        }
    }
    // Trailing field/record (no final newline).
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        push_record(&mut rows, record);
    }
    rows
}

/// Append a record unless it is entirely empty (skips blank lines).
fn push_record(rows: &mut Vec<Vec<String>>, record: Vec<String>) {
    if record.iter().any(|f| !f.trim().is_empty()) {
        rows.push(record);
    }
}

/// Render all data rows as `header: value | …` records.
#[must_use]
pub fn to_text(input: &str, delimiter: char) -> String {
    let rows = parse(input, delimiter);
    record_lines(&rows).join("\n")
}

/// Row-group chunks; each chunk repeats the header line for context.
#[must_use]
pub fn chunks(input: &str, delimiter: char) -> Vec<String> {
    let rows = parse(input, delimiter);
    if rows.is_empty() {
        return Vec::new();
    }
    let lines = record_lines(&rows);
    if lines.is_empty() {
        // Header-only (no data rows): the header itself is the content.
        return vec![rows[0].join(&delimiter.to_string())];
    }
    let header_line = rows[0].join(&delimiter.to_string());
    lines
        .chunks(ROWS_PER_CHUNK)
        .map(|group| format!("{}\n{}", header_line, group.join("\n")))
        .collect()
}

/// Map data rows (everything after row 0) to `col: val | col: val` lines.
fn record_lines(rows: &[Vec<String>]) -> Vec<String> {
    if rows.len() < 2 {
        return Vec::new();
    }
    let header = &rows[0];
    rows[1..]
        .iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(i, val)| {
                    let col = header
                        .get(i)
                        .map_or_else(|| format!("col{i}"), |h| h.trim().to_string());
                    format!("{}: {}", col, val.trim())
                })
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .filter(|l| !l.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_fields_with_embedded_delimiter() {
        let rows = parse("a,b\n\"x,y\",\"he said \"\"hi\"\"\"\n", ',');
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][0], "x,y");
        assert_eq!(rows[1][1], "he said \"hi\"");
    }

    #[test]
    fn renders_labeled_records() {
        let text = to_text("name,age\nAlice,30\nBob,25", ',');
        assert!(text.contains("name: Alice | age: 30"));
        assert!(text.contains("name: Bob | age: 25"));
    }

    #[test]
    fn chunks_repeat_header() {
        let c = chunks("h1,h2\na,b\nc,d", ',');
        assert_eq!(c.len(), 1);
        assert!(c[0].starts_with("h1,h2\n"));
    }

    #[test]
    fn header_only_input_still_chunks() {
        let c = chunks("just,a,header", ',');
        assert_eq!(c.len(), 1);
        assert_eq!(c[0], "just,a,header");
    }

    #[test]
    fn skips_blank_lines() {
        let rows = parse("a\n\n\nb", ',');
        assert_eq!(rows.len(), 2);
    }
}
