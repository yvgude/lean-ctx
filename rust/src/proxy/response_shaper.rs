//! Response shaping (#1125): reduces LLM output verbosity at the proxy layer
//! to save future context tokens.
//!
//! LLM responses contain "ceremony" tokens (preambles, code repetition,
//! post-action narration, trailing confirmations) that provide no value for
//! future turns but accumulate in conversation history. This module shapes
//! responses before returning them to the client.
//!
//! Determinism (#498): same response bytes → same shaped output. All pattern
//! matching is deterministic (static regex set, no randomness).
use regex::Regex;
use std::sync::LazyLock;

/// Result of shaping a response.
pub(crate) struct ShapingResult {
    pub bytes: Vec<u8>,
    pub tokens_saved: usize,
}

/// Shaping mode controls aggressiveness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShapingMode {
    Off,
    Gentle,
    Aggressive,
}

impl ShapingMode {
    pub(crate) fn from_str_config(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "gentle" => Self::Gentle,
            "aggressive" => Self::Aggressive,
            _ => Self::Off,
        }
    }
}

/// Shape a non-streaming LLM response. Returns `None` if no shaping applied
/// (response unchanged). Only operates on JSON responses with a text content
/// field (Anthropic `content[].text`, OpenAI `choices[].message.content`).
pub(super) fn shape_response(resp_bytes: &[u8], mode: ShapingMode) -> Option<ShapingResult> {
    if mode == ShapingMode::Off {
        return None;
    }

    let text = std::str::from_utf8(resp_bytes).ok()?;
    let mut parsed: serde_json::Value = serde_json::from_str(text).ok()?;

    let mut total_saved = 0usize;

    if let Some(shaped) = shape_anthropic(&mut parsed, mode) {
        total_saved += shaped;
    } else {
        let shaped = shape_openai(&mut parsed, mode)?;
        total_saved += shaped;
    }

    if total_saved == 0 {
        return None;
    }

    let output = serde_json::to_vec(&parsed).ok()?;
    Some(ShapingResult {
        bytes: output,
        tokens_saved: total_saved,
    })
}

/// Shape Anthropic response format: `content[].text`
fn shape_anthropic(parsed: &mut serde_json::Value, mode: ShapingMode) -> Option<usize> {
    let content = parsed.get_mut("content")?.as_array_mut()?;
    let mut saved = 0;

    for block in content.iter_mut() {
        if block.get("type").and_then(|t| t.as_str()) != Some("text") {
            continue;
        }
        if let Some(text_val) = block.get("text").and_then(|t| t.as_str()) {
            let original_len = text_val.len();
            let shaped = shape_text_content(text_val, mode);
            if shaped.len() < original_len {
                saved += (original_len - shaped.len()) / 4;
                block["text"] = serde_json::Value::String(shaped);
            }
        }
    }

    if saved > 0 { Some(saved) } else { None }
}

/// Shape OpenAI response format: `choices[].message.content`
fn shape_openai(parsed: &mut serde_json::Value, mode: ShapingMode) -> Option<usize> {
    let choices = parsed.get_mut("choices")?.as_array_mut()?;
    let mut saved = 0;

    for choice in choices.iter_mut() {
        let content = choice
            .get_mut("message")
            .and_then(|m| m.get_mut("content"))
            .and_then(|c| c.as_str())
            .map(std::string::ToString::to_string)?;

        let original_len = content.len();
        let shaped = shape_text_content(&content, mode);
        if shaped.len() < original_len {
            saved += (original_len - shaped.len()) / 4;
            choice["message"]["content"] = serde_json::Value::String(shaped);
        }
    }

    if saved > 0 { Some(saved) } else { None }
}

/// Core text shaping: applies preamble removal, narration compression, and
/// trailing confirmation stripping. Never touches code blocks, error messages,
/// or technical decisions.
fn shape_text_content(text: &str, mode: ShapingMode) -> String {
    if is_protected_content(text) {
        return text.to_string();
    }

    let mut result = text.to_string();

    // Stage 1: Strip preambles (gentle + aggressive)
    result = strip_preamble(&result);

    // Stage 2: Strip trailing confirmations (gentle + aggressive)
    result = strip_trailing_confirmation(&result);

    // Stage 3: Compress post-action narration (aggressive only)
    if mode == ShapingMode::Aggressive {
        result = compress_narration(&result);
    }

    result
}

/// Content that must NEVER be shaped — contains critical information.
fn is_protected_content(text: &str) -> bool {
    // Primarily code-only responses
    let trimmed = text.trim();
    if trimmed.starts_with("```") && trimmed.ends_with("```") {
        return true;
    }
    // Error diagnostics
    if contains_error_indicators(text) {
        return true;
    }
    // Very short responses (overhead > savings)
    if text.len() < 50 {
        return true;
    }
    false
}

fn contains_error_indicators(text: &str) -> bool {
    const ERROR_PATTERNS: &[&str] = &[
        "error[",
        "Error:",
        "ERROR:",
        "FAILED",
        "panicked at",
        "stack trace",
        "Traceback",
        "Exception",
        "fatal:",
        "FATAL:",
        "segfault",
        "core dumped",
    ];
    ERROR_PATTERNS.iter().any(|p| text.contains(p))
}

// --- Preamble Removal ---

static PREAMBLE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)^(Great|Sure|Absolutely|Of course|Certainly|Perfect|Alright)[,!.]?\s*(I('ll| will| can)|Let me|I understand|I see)",
        r"^(Based on|Looking at|After reviewing|After reading|Having reviewed)\s+(my analysis|the code|your request|the file|the output|the error)",
        r"^I('d be happy to|'ll now|'m going to|'ll go ahead and|'ll start by|'ll take a look)\s+",
        r"^(Let me|Allow me to|I'll|I will)\s+(check|look|read|examine|inspect|analyze|review|investigate)\s+",
        r"^(Okay|OK|Alright|Right)[,.]?\s+(I('ll| see| can)|let me|so)\s+",
    ]
    .iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
});

fn strip_preamble(text: &str) -> String {
    let first_line_end = text.find('\n').unwrap_or(text.len());
    let first_line = &text[..first_line_end];

    for pattern in PREAMBLE_PATTERNS.iter() {
        if pattern.find(first_line).is_some() {
            let remainder = text[first_line_end..].trim_start();
            if !remainder.is_empty() {
                let mut chars = remainder.chars();
                if let Some(first) = chars.next() {
                    return format!("{}{}", first.to_uppercase(), chars.as_str());
                }
            }
        }
    }
    text.to_string()
}

// --- Trailing Confirmation Removal ---

static CONFIRMATION_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\n+(Let me know if you('d like| need| want| have).*$)",
        r"\n+(Is there anything else.*$)",
        r"\n+(Feel free to (ask|reach|let me know).*$)",
        r"\n+(Would you like me to.*$)",
        r"\n+(I hope (this|that) helps.*$)",
        r"\n+(Don't hesitate to.*$)",
        r"\n+(Happy to help.*$)",
    ]
    .iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
});

fn strip_trailing_confirmation(text: &str) -> String {
    let mut result = text.to_string();
    for pattern in CONFIRMATION_PATTERNS.iter() {
        if let Some(m) = pattern.find(&result) {
            // Only strip if it's at the end (last 200 chars)
            if m.start() > result.len().saturating_sub(200) {
                result = result[..m.start()].trim_end().to_string();
            }
        }
    }
    result
}

// --- Narration Compression (Aggressive) ---

static NARRATION_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?m)^(I have|I've) (successfully |now )?(updated|modified|changed|fixed|added|removed|created|deleted|implemented|refactored) (the |this |that )?",
        r"(?m)^(The changes? (include|ensure|will|should|make)s?:?\s*\n)",
        r"(?m)^(This (ensures?|means?|allows?|enables?|makes?) (that |the )?.*\n)",
    ]
    .iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
});

fn compress_narration(text: &str) -> String {
    let mut result = text.to_string();
    for pattern in NARRATION_PATTERNS.iter() {
        result = pattern.replace_all(&result, "").to_string();
    }
    // Clean up resulting double-newlines
    while result.contains("\n\n\n") {
        result = result.replace("\n\n\n", "\n\n");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{
        ShapingMode, is_protected_content, shape_response, shape_text_content, strip_preamble,
        strip_trailing_confirmation,
    };

    #[test]
    fn strips_common_preambles() {
        let cases = [
            (
                "Great, I'll look at the file now.\n\nThe issue is on line 42.",
                "The issue is on line 42.",
            ),
            (
                "Sure, let me check that for you.\n\nThe function returns None.",
                "The function returns None.",
            ),
            (
                "I'd be happy to help with that.\n\nHere's what I found:",
                "Here's what I found:",
            ),
        ];
        for (input, expected) in &cases {
            let result = strip_preamble(input);
            assert_eq!(result.trim(), *expected, "Failed for input: {input}");
        }
    }

    #[test]
    fn preserves_non_preamble_content() {
        let technical =
            "The function signature needs to change from `fn foo()` to `fn foo() -> Result<()>`.";
        assert_eq!(strip_preamble(technical), technical);
    }

    #[test]
    fn strips_trailing_confirmations() {
        let input = "The fix is applied.\n\nLet me know if you need anything else!";
        let result = strip_trailing_confirmation(input);
        assert_eq!(result, "The fix is applied.");
    }

    #[test]
    fn preserves_short_confirmations_in_middle() {
        let input =
            "Let me know if this works.\n\nThe next step is to run tests.\n\nThen we deploy.";
        let result = strip_trailing_confirmation(input);
        // Should not strip since it's not at the end
        assert!(result.contains("The next step"));
    }

    #[test]
    fn protects_error_content() {
        let error_msg = "error[E0308]: mismatched types\n  --> src/main.rs:5:5";
        assert!(is_protected_content(error_msg));
    }

    #[test]
    fn protects_code_only_responses() {
        let code = "```rust\nfn main() {\n    println!(\"hello\");\n}\n```";
        assert!(is_protected_content(code));
    }

    #[test]
    fn full_shaping_gentle_mode() {
        let input = "Great, I'll fix that for you.\n\nChanged line 42 from `x` to `y`.\n\nLet me know if you need anything else!";
        let result = shape_text_content(input, ShapingMode::Gentle);
        assert!(result.starts_with("Changed line 42"));
        assert!(!result.contains("Let me know"));
        assert!(!result.contains("Great"));
    }

    #[test]
    fn shaping_is_deterministic() {
        let input = "Sure, let me look at that.\n\nThe issue is a missing semicolon.\n\nWould you like me to fix it?";
        let r1 = shape_text_content(input, ShapingMode::Gentle);
        let r2 = shape_text_content(input, ShapingMode::Gentle);
        assert_eq!(r1, r2);
    }

    #[test]
    fn shape_openai_response_format() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Great, I'll check that.\n\nThe answer is 42.\n\nLet me know if you need more help!"
                }
            }]
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        let result = shape_response(&bytes, ShapingMode::Gentle);
        assert!(result.is_some());
        let shaped: serde_json::Value = serde_json::from_slice(&result.unwrap().bytes).unwrap();
        let content = shaped["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(content.starts_with("The answer is 42."));
        assert!(!content.contains("Great"));
        assert!(!content.contains("Let me know"));
    }

    #[test]
    fn no_shaping_when_off() {
        let json = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Great, I'll do that.\n\nDone."
                }
            }]
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        assert!(shape_response(&bytes, ShapingMode::Off).is_none());
    }
}

#[cfg(test)]
mod edge_tests {
    use super::{ShapingMode, shape_response};

    #[test]
    fn handles_empty_response() {
        assert!(shape_response(b"{}", ShapingMode::Gentle).is_none());
    }

    #[test]
    fn handles_invalid_json() {
        assert!(shape_response(b"not json", ShapingMode::Gentle).is_none());
    }

    #[test]
    fn never_modifies_error_responses() {
        let json = serde_json::json!({
            "choices": [{"message": {"role": "assistant",
                "content": "Sure, I'll check.\n\nerror[E0308]: mismatched types\n\nLet me know!"
            }}]
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        assert!(shape_response(&bytes, ShapingMode::Aggressive).is_none());
    }

    #[test]
    fn aggressive_saves_more_than_gentle() {
        let json = serde_json::json!({
            "choices": [{"message": {"role": "assistant",
                "content": "Sure, I'll fix that for you.\n\nI have successfully updated the function.\nThe changes ensure correctness.\nThis means it works now.\n\nWould you like me to do more?"
            }}]
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        let gentle = shape_response(&bytes, ShapingMode::Gentle);
        let aggressive = shape_response(&bytes, ShapingMode::Aggressive);
        assert!(aggressive.unwrap().tokens_saved >= gentle.unwrap().tokens_saved);
    }
}
