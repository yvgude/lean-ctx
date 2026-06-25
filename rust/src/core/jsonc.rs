use serde_json::Value;

/// Strip `//` line comments, `/* */` block comments and trailing commas from
/// JSONC, then parse with `serde_json`. String contents are preserved verbatim.
///
/// This makes lean-ctx tolerant of the JSONC dialect that editors like VS Code
/// use for `settings.json` / `mcp.json` (comments + trailing commas are valid
/// there but rejected by strict JSON). See issue #311.
pub fn parse_jsonc(input: &str) -> Result<Value, serde_json::Error> {
    let stripped = strip_json_comments(input);
    let cleaned = strip_trailing_commas(&stripped);
    serde_json::from_str(&cleaned)
}

fn strip_json_comments(input: &str) -> String {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    let mut seg = 0;

    while i < len {
        let b = bytes[i];

        if b == b'"' {
            i += 1;
            while i < len {
                let c = bytes[i];
                i += 1;
                if c == b'\\' && i < len {
                    i += 1;
                } else if c == b'"' {
                    break;
                }
            }
            continue;
        }

        if b == b'/' && i + 1 < len {
            if bytes[i + 1] == b'/' {
                out.push_str(&input[seg..i]);
                i += 2;
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
                seg = i;
                continue;
            }
            if bytes[i + 1] == b'*' {
                out.push_str(&input[seg..i]);
                i += 2;
                while i + 1 < len {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                seg = i;
                continue;
            }
        }

        i += 1;
    }

    out.push_str(&input[seg..]);
    out
}

/// Remove trailing commas that appear before a closing `}` or `]`.
/// String contents are preserved verbatim (commas inside strings are ignored).
///
/// Operates on already comment-stripped input. Uses byte-segment copying so
/// multi-byte UTF-8 sequences are never split (all decision bytes are ASCII).
fn strip_trailing_commas(input: &str) -> String {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    let mut seg = 0;

    while i < len {
        let b = bytes[i];

        if b == b'"' {
            i += 1;
            while i < len {
                let c = bytes[i];
                i += 1;
                if c == b'\\' && i < len {
                    i += 1;
                } else if c == b'"' {
                    break;
                }
            }
            continue;
        }

        if b == b',' {
            let mut j = i + 1;
            while j < len && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < len && (bytes[j] == b'}' || bytes[j] == b']') {
                out.push_str(&input[seg..i]);
                i += 1;
                seg = i;
                continue;
            }
        }

        i += 1;
    }

    out.push_str(&input[seg..]);
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

    // --- #311: trailing comma support (VS Code / JSONC dialect) ---

    #[test]
    fn strips_trailing_comma_in_object() {
        let input = r#"{
  "a": 1,
  "b": 2,
}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], 2);
    }

    #[test]
    fn strips_trailing_comma_in_array() {
        let input = r#"{"list": [1, 2, 3,]}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["list"][2], 3);
    }

    #[test]
    fn strips_trailing_comma_with_whitespace_and_newlines() {
        let input = "{\n  \"a\": 1  ,\n\n}";
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn strips_nested_trailing_commas() {
        let input = r#"{
  "outer": {
    "inner": [
      "x",
      "y",
    ],
  },
}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["outer"]["inner"][1], "y");
    }

    #[test]
    fn preserves_comma_inside_string_before_brace() {
        // A comma inside a string value must not be treated as trailing.
        let input = r#"{"msg": "hello, world"}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["msg"], "hello, world");
    }

    #[test]
    fn issue_311_vscode_settings_with_trailing_comma_and_comments() {
        // Mirrors the real VS Code user settings.json that triggered #311:
        // "trailing comma at line 4 column 5" plus JSONC comments.
        let input = r#"{
  // editor settings
  "editor.fontSize": 14,
  "editor.tabSize": 2,
  "chat.mcp.enabled": true,
}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["editor.fontSize"], 14);
        assert!(v["chat.mcp.enabled"].as_bool().unwrap());
    }

    #[test]
    fn pure_json_passthrough() {
        let input = r#"{"key": "value", "num": 42}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["key"], "value");
        assert_eq!(v["num"], 42);
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

    #[test]
    fn utf8_umlauts_preserved() {
        let input = "{\n  // German names\n  \"name\": \"Müller\",\n  \"city\": \"Zürich\"\n}";
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["name"], "Müller");
        assert_eq!(v["city"], "Zürich");
    }

    #[test]
    fn utf8_cjk_with_block_comment() {
        let input = "{\n  /* 日本語コメント */\n  \"desc\": \"日本語テスト\"\n}";
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["desc"], "日本語テスト");
    }

    #[test]
    fn utf8_emoji_between_comments() {
        let input = "{\n  // before\n  \"icon\": \"🚀🔥\",\n  /* after */\n  \"ok\": true\n}";
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["icon"], "🚀🔥");
        assert!(v["ok"].as_bool().unwrap());
    }

    #[test]
    fn utf8_in_comment_stripped_cleanly() {
        let input = "{\n  // Achtung: ä ö ü ß\n  \"key\": \"value\"\n}";
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn utf8_in_key() {
        let input = "{\"straße\": \"Hauptstraße 42\"}";
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["straße"], "Hauptstraße 42");
    }

    #[test]
    fn mixed_ascii_and_utf8_values() {
        let input = "{\n  // config\n  \"en\": \"hello\",\n  \"ru\": \"привет\",\n  \"jp\": \"こんにちは\"\n}";
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["en"], "hello");
        assert_eq!(v["ru"], "привет");
        assert_eq!(v["jp"], "こんにちは");
    }

    #[test]
    fn escaped_unicode_unchanged() {
        let input = r#"{"test": "\u00e4\u00f6\u00fc"}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["test"], "\u{00e4}\u{00f6}\u{00fc}");
    }

    #[test]
    fn utf8_at_comment_boundary() {
        let input = "{\n  \"before\": \"текст\"// комментарий\n, \"after\": 1\n}";
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["before"], "текст");
        assert_eq!(v["after"], 1);
    }

    #[test]
    fn empty_string_after_utf8_comment() {
        let input = "{\n  // Ü\n  \"key\": \"\"\n}";
        let v = parse_jsonc(input).unwrap();
        assert_eq!(v["key"], "");
    }

    #[test]
    fn real_claude_settings_with_german_paths() {
        let input = r#"{
  // Claude Code Einstellungen
  "mcpServers": {
    /* Lean-CTX Konfiguration für /Users/müller/Projekte */
    "lean-ctx": {
      "command": "/Users/müller/.local/bin/lean-ctx",
      "args": ["--project", "/Users/müller/Projekte/größtes-projekt"]
    }
  }
}"#;
        let v = parse_jsonc(input).unwrap();
        assert_eq!(
            v["mcpServers"]["lean-ctx"]["command"],
            "/Users/müller/.local/bin/lean-ctx"
        );
        let args = v["mcpServers"]["lean-ctx"]["args"].as_array().unwrap();
        assert_eq!(args[1], "/Users/müller/Projekte/größtes-projekt");
    }
}
