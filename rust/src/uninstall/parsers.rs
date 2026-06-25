// ---------------------------------------------------------------------------
// Shell block removal
// ---------------------------------------------------------------------------

pub(super) fn remove_lean_ctx_block(content: &str) -> String {
    if content.contains("# lean-ctx shell hook — end") {
        return remove_lean_ctx_block_by_marker(content);
    }
    remove_lean_ctx_block_legacy(content)
}

/// Removes the login-shell snippet `init_posix` adds so bash login shells source `~/.bashrc`
/// (see `cli/shell_init.rs::ensure_bash_login_sources_bashrc`). Marker-delimited, so user content
/// in `~/.bash_profile` / `~/.profile` is preserved.
pub(super) fn remove_lean_ctx_login_block(content: &str) -> String {
    const BEGIN: &str = "# lean-ctx: load ~/.bashrc in login shells";
    const END: &str = "# lean-ctx: load ~/.bashrc in login shells (e.g. macOS Terminal) — end";
    if !content.contains(BEGIN) {
        return content.to_string();
    }
    let mut result = String::new();
    let mut in_block = false;
    for line in content.lines() {
        if !in_block && line.contains(BEGIN) && !line.trim_end().ends_with("— end") {
            in_block = true;
            continue;
        }
        if in_block {
            if line.trim() == END {
                in_block = false;
            }
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

fn remove_lean_ctx_block_by_marker(content: &str) -> String {
    let mut result = String::new();
    let mut in_block = false;

    for line in content.lines() {
        if !in_block && line.contains("lean-ctx shell hook") && !line.contains("end") {
            in_block = true;
            continue;
        }
        if in_block {
            if line.trim() == "# lean-ctx shell hook — end" {
                in_block = false;
            }
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

fn remove_lean_ctx_block_legacy(content: &str) -> String {
    let mut result = String::new();
    let mut in_block = false;

    for line in content.lines() {
        if line.contains("lean-ctx shell hook") {
            in_block = true;
            continue;
        }
        if in_block {
            if line.trim() == "fi" || line.trim() == "end" || line.trim().is_empty() {
                if line.trim() == "fi" || line.trim() == "end" {
                    in_block = false;
                }
                continue;
            }
            if !line.starts_with("alias ") && !line.starts_with('\t') && !line.starts_with("if ") {
                in_block = false;
                result.push_str(line);
                result.push('\n');
            }
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

// ---------------------------------------------------------------------------
// JSON removal — textual approach preserving comments and formatting
// ---------------------------------------------------------------------------

pub(super) fn remove_lean_ctx_from_json(content: &str) -> Option<String> {
    // Try textual removal first (preserves comments, formatting, key order)
    if let Some(result) = remove_lean_ctx_from_json_textual(content) {
        return Some(result);
    }

    // Fallback to serde-based approach for edge cases
    remove_lean_ctx_from_json_serde(content)
}

/// Textual JSON key removal: finds `"lean-ctx"` key-value pairs and removes
/// them from the raw text without re-serializing. Preserves JSONC comments,
/// formatting, trailing commas, and key ordering.
fn remove_lean_ctx_from_json_textual(content: &str) -> Option<String> {
    let mut result = content.to_string();
    let mut modified = false;

    // Repeatedly find and remove "lean-ctx" entries until none remain.
    // Each iteration rescans because positions shift after removal.
    while let Some(key_start) = find_json_key_position(result.as_bytes(), "lean-ctx") {
        let Some(new_result) = remove_json_entry_at(&result, key_start) else {
            break;
        };

        result = new_result;
        modified = true;
    }

    // Also handle array-style entries: {"name": "lean-ctx", ...}
    loop {
        let bytes = result.as_bytes();
        let Some(pos) = find_named_array_entry(bytes, "lean-ctx") else {
            break;
        };
        let Some(new_result) = remove_array_entry_at(&result, pos) else {
            break;
        };
        result = new_result;
        modified = true;
    }

    if modified {
        // Validate the result is still valid JSON(C) if the input was valid
        if crate::core::jsonc::parse_jsonc(&result).is_ok() {
            Some(result)
        } else if crate::core::jsonc::parse_jsonc(content).is_ok() {
            // Input was valid but our textual removal broke it — don't use this result
            None
        } else {
            // Input was already invalid, return our best effort
            Some(result)
        }
    } else {
        None
    }
}

/// Remove a key whose value is an EMPTY object (`"key": {}`) from JSON text.
/// Used for `OpenClaw` (GitHub #390): after the lean-ctx entry is removed, a
/// leftover empty `mcpServers` object would still trip the strict 2026.6.1
/// validator ("Unrecognized key"). Returns None when the key is absent or its
/// object is non-empty.
pub(super) fn remove_empty_json_object_key(content: &str, key_name: &str) -> Option<String> {
    let bytes = content.as_bytes();
    let key_start = find_json_key_position(bytes, key_name)?;

    // Locate the value and confirm it is an empty object.
    let key_name_end = content[key_start + 1..].find('"')? + key_start + 2;
    let mut colon_pos = key_name_end;
    while colon_pos < bytes.len() && bytes[colon_pos] != b':' {
        colon_pos += 1;
    }
    let mut v = colon_pos + 1;
    while v < bytes.len() && bytes[v].is_ascii_whitespace() {
        v += 1;
    }
    if v >= bytes.len() || bytes[v] != b'{' {
        return None;
    }
    let value_end = skip_json_value(bytes, v)?;
    let inner = &content[v + 1..value_end - 1];
    if !inner.trim().is_empty() {
        return None;
    }

    let result = remove_json_entry_at(content, key_start)?;
    crate::core::jsonc::parse_jsonc(&result)
        .is_ok()
        .then_some(result)
}

/// Find the byte position of a JSON key `"key_name"` that is followed by `:`.
fn find_json_key_position(bytes: &[u8], key_name: &str) -> Option<usize> {
    let needle = format!("\"{key_name}\"");
    let needle_bytes = needle.as_bytes();
    let mut i = 0;

    while i + needle_bytes.len() <= bytes.len() {
        if &bytes[i..i + needle_bytes.len()] == needle_bytes {
            // Check it's followed by `:` (after optional whitespace)
            let after = i + needle_bytes.len();
            let mut j = after;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b':' {
                // Make sure we're not inside a string by checking if we have
                // an even number of unescaped quotes before this position
                if !is_inside_string(bytes, i) {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

/// Check if position `pos` is inside a JSON string literal.
fn is_inside_string(bytes: &[u8], pos: usize) -> bool {
    let mut in_string = false;
    let mut i = 0;
    while i < pos {
        match bytes[i] {
            b'"' if !in_string => in_string = true,
            b'"' if in_string => in_string = false,
            b'\\' if in_string => {
                i += 1; // skip escaped char
            }
            b'/' if !in_string && i + 1 < bytes.len() => {
                if bytes[i + 1] == b'/' {
                    // Line comment — skip to end of line
                    while i < pos && i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                } else if bytes[i + 1] == b'*' {
                    // Block comment — skip to */
                    i += 2;
                    while i + 1 < bytes.len() {
                        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    in_string
}

/// Remove a JSON key-value entry starting at `key_start` position.
/// Handles surrounding commas and whitespace.
fn remove_json_entry_at(content: &str, key_start: usize) -> Option<String> {
    let bytes = content.as_bytes();

    // Find the colon after the key
    let key_name_end = content[key_start + 1..].find('"')? + key_start + 2;
    let mut colon_pos = key_name_end;
    while colon_pos < bytes.len() && bytes[colon_pos] != b':' {
        colon_pos += 1;
    }
    if colon_pos >= bytes.len() {
        return None;
    }

    // Skip the value
    let value_start = colon_pos + 1;
    let value_end = skip_json_value(bytes, value_start)?;

    // Determine the range to remove, including surrounding comma and whitespace.
    // Scan backwards from key_start to find leading comma or whitespace.
    let mut remove_start = key_start;

    // Look backwards for a comma (we might be after a comma)
    let mut scan_back = key_start;
    while scan_back > 0 {
        scan_back -= 1;
        let ch = bytes[scan_back];
        if ch == b',' {
            remove_start = scan_back;
            break;
        }
        if ch == b'{' || ch == b'[' {
            break;
        }
        if !ch.is_ascii_whitespace() {
            break;
        }
    }

    // Extend remove_start back to include the newline before the comma/key
    if remove_start > 0 && remove_start == key_start {
        let mut ns = remove_start;
        while ns > 0 && bytes[ns - 1].is_ascii_whitespace() && bytes[ns - 1] != b'\n' {
            ns -= 1;
        }
        if ns > 0 && bytes[ns - 1] == b'\n' {
            remove_start = ns;
        }
    }

    let mut remove_end = value_end;

    // Look forward for a trailing comma
    let mut scan_fwd = value_end;
    while scan_fwd < bytes.len() && bytes[scan_fwd].is_ascii_whitespace() {
        scan_fwd += 1;
    }
    if scan_fwd < bytes.len() && bytes[scan_fwd] == b',' {
        // If we already consumed a leading comma, don't consume trailing too
        if remove_start < key_start && remove_start < bytes.len() && bytes[remove_start] == b',' {
            // Already have leading comma removed, skip trailing
        } else {
            remove_end = scan_fwd + 1;
        }
    }

    // Skip trailing whitespace/newline after the removed entry
    while remove_end < bytes.len()
        && (bytes[remove_end] == b' ' || bytes[remove_end] == b'\t' || bytes[remove_end] == b'\r')
    {
        remove_end += 1;
    }
    if remove_end < bytes.len() && bytes[remove_end] == b'\n' {
        remove_end += 1;
    }

    let mut result = String::with_capacity(content.len());
    result.push_str(&content[..remove_start]);
    result.push_str(&content[remove_end..]);
    Some(result)
}

/// Find an array entry like `{"name": "lean-ctx", ...}` and return its start position.
fn find_named_array_entry(bytes: &[u8], name: &str) -> Option<usize> {
    let needle = format!("\"{name}\"");
    let needle_bytes = needle.as_bytes();
    let mut i = 0;

    while i + needle_bytes.len() <= bytes.len() {
        if &bytes[i..i + needle_bytes.len()] == needle_bytes && !is_inside_string(bytes, i) {
            // Check this is a value (preceded by `:` after `"name"`)
            // Scan backwards to check if the key is "name"
            let mut j = i;
            while j > 0 && bytes[j - 1].is_ascii_whitespace() {
                j -= 1;
            }
            if j > 0 && bytes[j - 1] == b':' {
                j -= 1;
                while j > 0 && bytes[j - 1].is_ascii_whitespace() {
                    j -= 1;
                }
                if j >= 6 && &bytes[j - 6..j] == b"\"name\"" {
                    // Found "name": "lean-ctx" — now find the enclosing object `{`
                    let mut obj_start = j - 6;
                    while obj_start > 0 {
                        if bytes[obj_start] == b'{' && !is_inside_string(bytes, obj_start) {
                            return Some(obj_start);
                        }
                        obj_start -= 1;
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Remove an array entry (object) starting at `entry_start`, handling commas.
fn remove_array_entry_at(content: &str, entry_start: usize) -> Option<String> {
    let bytes = content.as_bytes();
    if bytes[entry_start] != b'{' {
        return None;
    }
    let entry_end = skip_json_value(bytes, entry_start)?;

    let mut remove_start = entry_start;
    let mut remove_end = entry_end;

    // Handle leading whitespace
    while remove_start > 0 && (bytes[remove_start - 1] == b' ' || bytes[remove_start - 1] == b'\t')
    {
        remove_start -= 1;
    }

    // Handle trailing comma
    let mut fwd = entry_end;
    while fwd < bytes.len() && bytes[fwd].is_ascii_whitespace() {
        fwd += 1;
    }
    if fwd < bytes.len() && bytes[fwd] == b',' {
        remove_end = fwd + 1;
    } else {
        // No trailing comma — check for leading comma
        let mut back = remove_start;
        while back > 0 && bytes[back - 1].is_ascii_whitespace() {
            back -= 1;
        }
        if back > 0 && bytes[back - 1] == b',' {
            remove_start = back - 1;
        }
    }

    // Skip trailing newline
    while remove_end < bytes.len()
        && (bytes[remove_end] == b' ' || bytes[remove_end] == b'\t' || bytes[remove_end] == b'\r')
    {
        remove_end += 1;
    }
    if remove_end < bytes.len() && bytes[remove_end] == b'\n' {
        remove_end += 1;
    }

    let mut result = String::with_capacity(content.len());
    result.push_str(&content[..remove_start]);
    result.push_str(&content[remove_end..]);
    Some(result)
}

/// Skip over a JSON value (object, array, string, number, boolean, null)
/// starting from `start`. Returns the position after the value.
fn skip_json_value(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;

    // Skip whitespace
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }

    match bytes[i] {
        b'{' | b'[' => {
            let open = bytes[i];
            let close = if open == b'{' { b'}' } else { b']' };
            let mut depth = 1;
            i += 1;
            while i < bytes.len() && depth > 0 {
                match bytes[i] {
                    c if c == open => depth += 1,
                    c if c == close => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    b'"' => {
                        i += 1;
                        while i < bytes.len() {
                            if bytes[i] == b'\\' {
                                i += 1;
                            } else if bytes[i] == b'"' {
                                break;
                            }
                            i += 1;
                        }
                    }
                    b'/' if i + 1 < bytes.len() => {
                        if bytes[i + 1] == b'/' {
                            while i < bytes.len() && bytes[i] != b'\n' {
                                i += 1;
                            }
                            continue;
                        } else if bytes[i + 1] == b'*' {
                            i += 2;
                            while i + 1 < bytes.len() {
                                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                                    i += 1;
                                    break;
                                }
                                i += 1;
                            }
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            Some(i)
        }
        b'"' => {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 1;
                } else if bytes[i] == b'"' {
                    return Some(i + 1);
                }
                i += 1;
            }
            None
        }
        _ => {
            // Number, boolean, null
            while i < bytes.len() && !matches!(bytes[i], b',' | b'}' | b']' | b'\n' | b'\r') {
                i += 1;
            }
            Some(i)
        }
    }
}

/// Fallback: serde-based JSON removal (destroys comments/formatting).
fn remove_lean_ctx_from_json_serde(content: &str) -> Option<String> {
    let mut parsed: serde_json::Value = crate::core::jsonc::parse_jsonc(content).ok()?;
    let mut modified = false;

    if let Some(servers) = parsed.get_mut("mcpServers").and_then(|s| s.as_object_mut()) {
        modified |= servers.remove("lean-ctx").is_some();
    }

    if let Some(servers) = parsed.get_mut("servers").and_then(|s| s.as_object_mut()) {
        modified |= servers.remove("lean-ctx").is_some();
    }

    if let Some(servers) = parsed.get_mut("servers").and_then(|s| s.as_array_mut()) {
        let before = servers.len();
        servers.retain(|entry| entry.get("name").and_then(|n| n.as_str()) != Some("lean-ctx"));
        modified |= servers.len() < before;
    }

    if let Some(mcp) = parsed.get_mut("mcp").and_then(|s| s.as_object_mut()) {
        modified |= mcp.remove("lean-ctx").is_some();
    }

    // Zed uses `context_servers` instead of `mcpServers`
    if let Some(ctx) = parsed
        .get_mut("context_servers")
        .and_then(|s| s.as_object_mut())
    {
        modified |= ctx.remove("lean-ctx").is_some();
    }

    if let Some(amp) = parsed
        .get_mut("amp.mcpServers")
        .and_then(|s| s.as_object_mut())
    {
        modified |= amp.remove("lean-ctx").is_some();
    }

    if modified {
        Some(serde_json::to_string_pretty(&parsed).ok()? + "\n")
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// YAML removal
// ---------------------------------------------------------------------------

pub(super) fn remove_lean_ctx_from_yaml(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut skip_depth: Option<usize> = None;

    for line in content.lines() {
        if let Some(depth) = skip_depth {
            let indent = line.len() - line.trim_start().len();
            if indent > depth || line.trim().is_empty() {
                continue;
            }
            skip_depth = None;
        }

        let trimmed = line.trim();
        if trimmed == "lean-ctx:" || trimmed.starts_with("lean-ctx:") {
            let indent = line.len() - line.trim_start().len();
            skip_depth = Some(indent);
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }

    out
}

// ---------------------------------------------------------------------------
// TOML removal
// ---------------------------------------------------------------------------

pub(super) fn remove_lean_ctx_from_toml(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut skip = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let section = trimmed.trim_start_matches('[').trim_end_matches(']').trim();
            if section == "mcp_servers.lean-ctx"
                || section == "mcp_servers.\"lean-ctx\""
                || section.starts_with("mcp_servers.lean-ctx.")
                || section.starts_with("mcp_servers.\"lean-ctx\".")
            {
                skip = true;
                continue;
            }
            skip = false;
        }

        if skip {
            continue;
        }

        let without_comment = trimmed.split('#').next().unwrap_or("").trim();
        if (without_comment.contains("codex_hooks")
            || without_comment
                .strip_prefix("hooks")
                .is_some_and(|rest| rest.trim_start().starts_with('=') && !rest.starts_with('_')))
            && without_comment.contains("true")
        {
            out.push_str(&line.replace("true", "false"));
            out.push('\n');
            continue;
        }

        out.push_str(line);
        out.push('\n');
    }

    let cleaned: String = out
        .lines()
        .filter(|l| l.trim() != "[]")
        .collect::<Vec<_>>()
        .join("\n");
    if cleaned.is_empty() {
        cleaned
    } else {
        cleaned + "\n"
    }
}

// moved to core/editor_registry/paths.rs

#[cfg(test)]
mod tests {
    use super::super::agents::{
        HookCleanupResult, remove_lean_ctx_from_hooks_json, remove_lean_ctx_section_from_rules,
    };
    use super::super::{backup_before_modify, bak_path_for, remove_marked_block};
    use super::*;

    // --- TOML tests ---

    #[test]
    fn remove_toml_mcp_server_section() {
        let input = "\
[features]
codex_hooks = true

[mcp_servers.lean-ctx]
command = \"/usr/local/bin/lean-ctx\"
args = []

[mcp_servers.other-tool]
command = \"/usr/bin/other\"
";
        let result = remove_lean_ctx_from_toml(input);
        assert!(
            !result.contains("lean-ctx"),
            "lean-ctx section should be removed"
        );
        assert!(
            result.contains("[mcp_servers.other-tool]"),
            "other sections should be preserved"
        );
        assert!(
            result.contains("codex_hooks = false"),
            "codex_hooks should be set to false"
        );
    }

    #[test]
    fn remove_toml_only_lean_ctx() {
        let input = "\
[mcp_servers.lean-ctx]
command = \"lean-ctx\"
";
        let result = remove_lean_ctx_from_toml(input);
        assert!(
            result.trim().is_empty(),
            "should produce empty output: {result}"
        );
    }

    #[test]
    fn remove_toml_no_lean_ctx() {
        let input = "\
[mcp_servers.other]
command = \"other\"
";
        let result = remove_lean_ctx_from_toml(input);
        assert!(
            result.contains("[mcp_servers.other]"),
            "other content should be preserved"
        );
    }

    // --- JSON textual removal tests ---

    #[test]
    fn json_textual_removes_key_from_object() {
        let input = r#"{
  "mcpServers": {
    "other-tool": {
      "command": "other"
    },
    "lean-ctx": {
      "command": "/usr/bin/lean-ctx",
      "args": []
    }
  }
}
"#;
        let result = remove_lean_ctx_from_json(input).expect("should find lean-ctx");
        assert!(!result.contains("lean-ctx"), "lean-ctx should be removed");
        assert!(
            result.contains("other-tool"),
            "other-tool should be preserved"
        );
        // Verify valid JSON
        assert!(
            crate::core::jsonc::parse_jsonc(&result).is_ok(),
            "result should be valid JSON: {result}"
        );
    }

    #[test]
    fn json_textual_preserves_comments() {
        let input = r#"{
  // This is a user comment
  "mcpServers": {
    "lean-ctx": {
      "command": "lean-ctx"
    },
    "my-tool": {
      "command": "my-tool"
    }
  }
}
"#;
        let result = remove_lean_ctx_from_json(input).expect("should find lean-ctx");
        assert!(!result.contains("lean-ctx"), "lean-ctx should be removed");
        assert!(
            result.contains("// This is a user comment"),
            "comment should be preserved: {result}"
        );
        assert!(result.contains("my-tool"), "my-tool should be preserved");
    }

    #[test]
    fn json_textual_only_lean_ctx() {
        let input = r#"{
  "mcpServers": {
    "lean-ctx": {
      "command": "lean-ctx"
    }
  }
}
"#;
        let result = remove_lean_ctx_from_json(input).expect("should find lean-ctx");
        assert!(!result.contains("lean-ctx"), "lean-ctx should be removed");
    }

    #[test]
    fn json_no_lean_ctx_returns_none() {
        let input = r#"{"mcpServers": {"other": {"command": "other"}}}"#;
        assert!(remove_lean_ctx_from_json(input).is_none());
    }

    // --- Empty-object key removal (OpenClaw #390) ---

    #[test]
    fn empty_mcp_servers_object_is_removed() {
        let input = "{\n  \"mcpServers\": {},\n  \"mcp\": { \"servers\": {} }\n}\n";
        let result = remove_empty_json_object_key(input, "mcpServers").expect("should remove");
        assert!(
            !result.contains("mcpServers"),
            "empty legacy key must vanish: {result}"
        );
        assert!(result.contains("\"mcp\""), "nested schema key preserved");
        assert!(crate::core::jsonc::parse_jsonc(&result).is_ok());
    }

    #[test]
    fn empty_object_removal_respects_whitespace_variants() {
        let input = "{ \"mcpServers\": {  \n  }, \"gateway\": { \"port\": 1 } }";
        let result = remove_empty_json_object_key(input, "mcpServers").expect("should remove");
        assert!(!result.contains("mcpServers"));
        assert!(result.contains("gateway"));
        assert!(crate::core::jsonc::parse_jsonc(&result).is_ok());
    }

    #[test]
    fn non_empty_mcp_servers_object_is_kept() {
        let input = r#"{"mcpServers": {"github": {"command": "gh-mcp"}}}"#;
        assert!(
            remove_empty_json_object_key(input, "mcpServers").is_none(),
            "foreign servers must never be dropped"
        );
    }

    #[test]
    fn missing_key_returns_none() {
        assert!(remove_empty_json_object_key("{}", "mcpServers").is_none());
    }

    #[test]
    fn openclaw_uninstall_flow_leaves_no_unrecognized_keys() {
        // Full reporter flow: nested + legacy entry, uninstall removes the
        // lean-ctx entries, then the empty legacy container is stripped.
        let input = r#"{
  "mcpServers": {
    "lean-ctx": { "command": "/usr/bin/lean-ctx" }
  },
  "mcp": {
    "servers": {
      "lean-ctx": { "command": "/usr/bin/lean-ctx" },
      "github": { "command": "gh-mcp" }
    }
  }
}
"#;
        let cleaned = remove_lean_ctx_from_json(input).expect("entries removed");
        let stripped =
            remove_empty_json_object_key(&cleaned, "mcpServers").expect("empty container removed");
        let parsed: serde_json::Value = crate::core::jsonc::parse_jsonc(&stripped).unwrap();
        assert!(
            parsed.get("mcpServers").is_none(),
            "no unrecognized key left"
        );
        assert_eq!(parsed["mcp"]["servers"]["github"]["command"], "gh-mcp");
        assert!(parsed["mcp"]["servers"].get("lean-ctx").is_none());
    }

    // --- Shared rules (SharedMarkdown) tests ---

    #[test]
    fn shared_markdown_surgical_removal() {
        let input = "# My custom rules\n\nDo this and that.\n\n\
                      # Context Engineering Layer\n\
                      <!-- lean-ctx-rules-v9 -->\n\n\
                      Use ctx_read instead of Read.\n\
                      <!-- /lean-ctx -->\n\n\
                      # Other section\n\nMore user content.\n";

        let cleaned =
            remove_marked_block(input, "# Context Engineering Layer", "<!-- /lean-ctx -->");

        assert!(
            !cleaned.contains("lean-ctx"),
            "lean-ctx block should be removed"
        );
        assert!(
            cleaned.contains("My custom rules"),
            "user content before should be preserved"
        );
        assert!(
            cleaned.contains("Other section"),
            "user content after should be preserved"
        );
        assert!(
            cleaned.contains("More user content"),
            "user content after should be preserved"
        );
    }

    #[test]
    fn shared_markdown_only_lean_ctx() {
        let input = "# Context Engineering Layer\n\
                      <!-- lean-ctx-rules-v9 -->\n\
                      content\n\
                      <!-- /lean-ctx -->\n";

        let cleaned =
            remove_marked_block(input, "# Context Engineering Layer", "<!-- /lean-ctx -->");

        assert!(
            cleaned.trim().is_empty() || !cleaned.contains("lean-ctx"),
            "should be empty or without lean-ctx: '{cleaned}'"
        );
    }

    // --- Project files (.cursorrules) tests ---

    #[test]
    fn cursorrules_surgical_removal() {
        let input = "# My project rules\n\n\
                      Always use TypeScript.\n\n\
                      # Context Engineering Layer\n\n\
                      PREFER lean-ctx MCP tools over native equivalents.\n";

        let cleaned = remove_lean_ctx_section_from_rules(input);

        assert!(
            !cleaned.contains("lean-ctx"),
            "lean-ctx section should be removed"
        );
        assert!(
            cleaned.contains("My project rules"),
            "user rules should be preserved"
        );
        assert!(
            cleaned.contains("Always use TypeScript"),
            "user content should be preserved"
        );
    }

    #[test]
    fn cursorrules_only_lean_ctx() {
        let input = "# Context Engineering Layer\n\n\
                      PREFER lean-ctx MCP tools.\n";

        let cleaned = remove_lean_ctx_section_from_rules(input);
        assert!(
            cleaned.trim().is_empty(),
            "should be empty when only lean-ctx content: '{cleaned}'"
        );
    }

    // --- hooks.json tests ---

    #[test]
    fn hooks_json_preserves_other_hooks() {
        let input = r#"{
  "version": 1,
  "hooks": {
    "preToolUse": [
      {
        "matcher": "Shell",
        "command": "lean-ctx hook rewrite"
      },
      {
        "matcher": "Shell",
        "command": "my-other-tool hook"
      }
    ]
  }
}"#;
        let result = match remove_lean_ctx_from_hooks_json(input) {
            HookCleanupResult::Cleaned(s) => s,
            other => panic!("expected Cleaned, got {other:?}"),
        };
        assert!(!result.contains("lean-ctx"), "lean-ctx should be removed");
        assert!(
            result.contains("my-other-tool"),
            "other hooks should be preserved"
        );
    }

    #[test]
    fn hooks_json_entirely_lean_ctx_no_other_keys() {
        let input = r#"{
  "hooks": {
    "preToolUse": [
      {
        "matcher": "Shell",
        "command": "lean-ctx hook rewrite"
      }
    ]
  }
}"#;
        assert!(
            matches!(
                remove_lean_ctx_from_hooks_json(input),
                HookCleanupResult::EntirelyLeanCtx
            ),
            "should return EntirelyLeanCtx when all hooks are lean-ctx and no other keys"
        );
    }

    #[test]
    fn hooks_json_version_only_boilerplate_is_entirely_lean_ctx() {
        // `version` / `$schema` are installer boilerplate: once the lean-ctx
        // hooks are gone, `{"hooks": {}, "version": 1}` carries no user
        // content and must be deleted instead of left behind (GL #558).
        let input = r#"{
  "version": 1,
  "hooks": {
    "preToolUse": [
      {
        "matcher": "Shell",
        "command": "lean-ctx hook rewrite"
      }
    ]
  }
}"#;
        assert!(
            matches!(
                remove_lean_ctx_from_hooks_json(input),
                HookCleanupResult::EntirelyLeanCtx
            ),
            "version-only leftovers should be EntirelyLeanCtx"
        );
    }

    #[test]
    fn hooks_json_version_key_with_user_hooks_is_cleaned_not_deleted() {
        let input = r#"{
  "version": 1,
  "hooks": {
    "preToolUse": [
      {
        "matcher": "Shell",
        "command": "lean-ctx hook rewrite"
      },
      {
        "matcher": "Shell",
        "command": "my-other-tool hook"
      }
    ]
  }
}"#;
        let result = match remove_lean_ctx_from_hooks_json(input) {
            HookCleanupResult::Cleaned(s) => s,
            other => panic!("expected Cleaned, got {other:?}"),
        };
        assert!(
            result.contains("version"),
            "version key should be preserved alongside user hooks"
        );
        assert!(!result.contains("lean-ctx"), "lean-ctx should be removed");
        assert!(
            result.contains("my-other-tool"),
            "user hooks should survive"
        );
    }

    #[test]
    fn hooks_json_handles_nested_claude_format() {
        let input = r#"{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "lean-ctx hook rewrite"
          }
        ]
      },
      {
        "matcher": "Other",
        "hooks": [
          {
            "type": "command",
            "command": "my-other-tool check"
          }
        ]
      }
    ]
  }
}"#;
        let result = match remove_lean_ctx_from_hooks_json(input) {
            HookCleanupResult::Cleaned(s) => s,
            other => panic!("expected Cleaned, got {other:?}"),
        };
        assert!(
            !result.contains("lean-ctx"),
            "lean-ctx nested entry removed"
        );
        assert!(
            result.contains("my-other-tool"),
            "non-lean-ctx entries preserved"
        );
    }

    #[test]
    fn hooks_json_mixed_nested_group_preserves_user_hooks() {
        let input = r#"{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          { "type": "command", "command": "lean-ctx hook rewrite" },
          { "type": "command", "command": "my-custom-guard" }
        ]
      }
    ]
  }
}"#;
        let result = match remove_lean_ctx_from_hooks_json(input) {
            HookCleanupResult::Cleaned(s) => s,
            other => panic!("expected Cleaned, got {other:?}"),
        };
        assert!(!result.contains("lean-ctx"), "lean-ctx sub-hook removed");
        assert!(
            result.contains("my-custom-guard"),
            "user sub-hook in same group preserved"
        );
    }

    #[test]
    fn hooks_json_copilot_bash_format() {
        let input = r#"{
  "hooks": {
    "preToolUse": [
      { "bash": "lean-ctx hook rewrite" },
      { "bash": "my-other-hook" }
    ]
  }
}"#;
        let result = match remove_lean_ctx_from_hooks_json(input) {
            HookCleanupResult::Cleaned(s) => s,
            other => panic!("expected Cleaned, got {other:?}"),
        };
        assert!(!result.contains("lean-ctx"), "lean-ctx bash entry removed");
        assert!(
            result.contains("my-other-hook"),
            "other bash hook preserved"
        );
    }

    #[test]
    fn hooks_json_permissions_only_not_deleted() {
        let input = r#"{
  "permissions": {
    "allow": [
      "mcp__lean-ctx__ctx_read",
      "mcp__lean-ctx__ctx_search",
      "mcp__other-tool__do_stuff"
    ]
  }
}"#;
        let result = match remove_lean_ctx_from_hooks_json(input) {
            HookCleanupResult::Cleaned(s) => s,
            other => panic!("expected Cleaned (permissions remain), got {other:?}"),
        };
        assert!(!result.contains("lean-ctx"), "lean-ctx permissions removed");
        assert!(
            result.contains("mcp__other-tool"),
            "other permissions preserved"
        );
    }

    #[test]
    fn hooks_json_parse_error_does_not_delete() {
        let input = "{ this is not valid JSON at all !!!";
        assert!(
            matches!(
                remove_lean_ctx_from_hooks_json(input),
                HookCleanupResult::ParseError
            ),
            "parse errors should return ParseError, not delete the file"
        );
    }

    #[test]
    fn hooks_json_no_lean_ctx_returns_unchanged() {
        let input = r#"{
  "hooks": {
    "preToolUse": [
      { "command": "some-other-tool" }
    ]
  }
}"#;
        assert!(
            matches!(
                remove_lean_ctx_from_hooks_json(input),
                HookCleanupResult::Unchanged
            ),
            "should return Unchanged when no lean-ctx found"
        );
    }

    // --- Marked block tests ---

    #[test]
    fn marked_block_preserves_surrounding() {
        let content = "before\n<!-- lean-ctx -->\nhook content\n<!-- /lean-ctx -->\nafter\n";
        let cleaned = remove_marked_block(content, "<!-- lean-ctx -->", "<!-- /lean-ctx -->");
        assert!(!cleaned.contains("hook content"));
        assert!(cleaned.contains("before"));
        assert!(cleaned.contains("after"));
    }

    #[test]
    fn marked_block_preserves_when_missing() {
        let content = "no hook here\n";
        let cleaned = remove_marked_block(content, "<!-- lean-ctx -->", "<!-- /lean-ctx -->");
        assert_eq!(cleaned, content);
    }

    #[test]
    fn backup_before_modify_respects_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        std::fs::write(&path, "hello").unwrap();

        backup_before_modify(&path, true);
        assert!(
            !bak_path_for(&path).exists(),
            "dry-run must not create backups"
        );

        backup_before_modify(&path, false);
        assert!(
            bak_path_for(&path).exists(),
            "non-dry-run should create backups"
        );
    }

    #[test]
    fn removes_login_block_preserving_user_content() {
        let input = "export PATH=\"$HOME/bin:$PATH\"\n\n\
            # lean-ctx: load ~/.bashrc in login shells (e.g. macOS Terminal) — begin\n\
            if [ -f \"$HOME/.bashrc\" ]; then . \"$HOME/.bashrc\"; fi\n\
            # lean-ctx: load ~/.bashrc in login shells (e.g. macOS Terminal) — end\n\n\
            export EDITOR=vim\n";
        let out = remove_lean_ctx_login_block(input);
        assert!(!out.contains("lean-ctx"), "login block removed: {out}");
        assert!(out.contains("export PATH"), "leading content preserved");
        assert!(
            out.contains("export EDITOR=vim"),
            "trailing content preserved"
        );
    }

    #[test]
    fn login_block_noop_when_absent() {
        let input = "export PATH=\"$HOME/bin:$PATH\"\n";
        assert_eq!(remove_lean_ctx_login_block(input), input);
    }
}
