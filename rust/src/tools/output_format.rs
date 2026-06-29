//! Shared `text|json` output-format selector for analysis tools
//! (`ctx_impact`, `ctx_architecture`, `ctx_smells`), which all parsed the same
//! two-variant enum independently before.

/// How a tool should render its result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputFormat {
    Text,
    Json,
}

/// Parses the optional `format` argument, defaulting to `text`. Whitespace and
/// case are normalized; anything else is a caller-facing error.
pub(crate) fn parse_format(format: Option<&str>) -> Result<OutputFormat, String> {
    let f = format.unwrap_or("text").trim().to_lowercase();
    match f.as_str() {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        _ => Err("Error: format must be text|json".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_text() {
        assert_eq!(parse_format(None).unwrap(), OutputFormat::Text);
    }

    #[test]
    fn normalizes_case_and_whitespace() {
        assert_eq!(parse_format(Some("  JSON ")).unwrap(), OutputFormat::Json);
        assert_eq!(parse_format(Some("Text")).unwrap(), OutputFormat::Text);
    }

    #[test]
    fn rejects_unknown() {
        assert_eq!(
            parse_format(Some("yaml")).unwrap_err(),
            "Error: format must be text|json"
        );
    }
}
