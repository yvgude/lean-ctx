use serde_json::Value;

/// Strip `//` line comments and `/* */` block comments from JSONC,
/// then parse with serde_json. String contents are preserved verbatim.
pub fn parse_jsonc(input: &str) -> Result<Value, serde_json::Error> {
    let stripped = strip_json_comments(input);
    serde_json::from_str(&stripped)
}

fn strip_json_comments(input: &str) -> String {
    // Scan as bytes for ASCII delimiters (", /, *, \, \n) — these never appear
    // inside multi-byte UTF-8 sequences, so byte-position math stays on char
    // boundaries. Emit content via `push_str(&input[..])` slicing so multi-byte
    // characters (e.g. "ó") roundtrip verbatim instead of being decomposed into
    // Latin-1 codepoints and re-encoded (which would double their byte length
    // on every pass).
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    let mut copy_from = 0;

    while i < len {
        let b = bytes[i];

        if b == b'"' {
            // Walk past the string literal, including its contents. The bytes
            // remain part of the current copy segment and will be flushed when
            // we hit a comment or the end of input.
            i += 1;
            while i < len {
                let c = bytes[i];
                if c == b'\\' && i + 1 < len {
                    i += 2;
                } else if c == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        if b == b'/' && i + 1 < len {
            if bytes[i + 1] == b'/' {
                out.push_str(&input[copy_from..i]);
                i += 2;
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
                copy_from = i;
                continue;
            }
            if bytes[i + 1] == b'*' {
                out.push_str(&input[copy_from..i]);
                i += 2;
                while i + 1 < len {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                copy_from = i;
                continue;
            }
        }

        i += 1;
    }

    out.push_str(&input[copy_from..len]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_line_comments() {
        let input = r#"{
  // this is a comment
  "key": "value"
}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn strips_block_comments() {
        let input = r#"{
  /* block
     comment */
  "key": "value"
}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn preserves_slashes_in_strings() {
        let input = r#"{"url": "https://example.com/path"}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["url"], "https://example.com/path");
    }

    #[test]
    fn preserves_comment_like_content_in_strings() {
        let input = r#"{"note": "see // inline", "code": "/* not a comment */"}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["note"], "see // inline");
        assert_eq!(v["code"], "/* not a comment */");
    }

    #[test]
    fn handles_escaped_quotes_in_strings() {
        let input = r#"{"msg": "say \"hello\" // world"}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["msg"], r#"say "hello" // world"#);
    }

    #[test]
    fn handles_trailing_comma_free_json() {
        let input = r#"{
  "a": 1,
  // comment between entries
  "b": 2
}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], 2);
    }

    #[test]
    fn empty_input() {
        assert!(parse_jsonc("").is_err());
    }

    #[test]
    fn pure_json_passthrough() {
        let input = r#"{"key": "value", "num": 42}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["key"], "value");
        assert_eq!(v["num"], 42);
    }

    #[test]
    fn preserves_non_ascii_strings_byte_for_byte() {
        // Regression: strip_json_comments used to do `out.push(byte as char)`,
        // which decomposed multi-byte UTF-8 sequences into Latin-1 codepoints
        // and silently doubled their byte length on every roundtrip. Repeated
        // install/repair cycles drove ~/.claude/settings.json past 2 GiB and
        // segfaulted Claude Code.
        let input = r#"{"msg": "Verificando optimización para iPhone/Safari..."}"#;
        let original_len = input.len();
        let mut current = input.to_string();
        for _ in 0..5 {
            let parsed = parse_jsonc(&current).unwrap();
            current = serde_json::to_string(&parsed).unwrap();
        }
        // After 5 roundtrips the size must be stable, not exponentially growing.
        assert!(
            current.len() <= original_len + 4,
            "roundtrip should be size-stable, got {} bytes (started at {})",
            current.len(),
            original_len
        );
        let v: Value = serde_json::from_str(&current).unwrap();
        assert_eq!(
            v["msg"], "Verificando optimización para iPhone/Safari...",
            "non-ASCII string content must roundtrip unchanged"
        );
    }

    #[test]
    fn real_opencode_config_with_comments() {
        let input = r#"{
  // OpenCode configuration
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    /* existing tool */
    "my-tool": {
      "type": "local",
      "command": ["my-tool"],
      "enabled": true
    }
  }
}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["$schema"], "https://opencode.ai/config.json");
        assert!(v["mcp"]["my-tool"]["enabled"].as_bool().unwrap());
    }
}
