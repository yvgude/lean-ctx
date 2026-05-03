use serde_json::Value;

pub fn get_str_array(
    args: Option<&serde_json::Map<String, Value>>,
    key: &str,
) -> Option<Vec<String>> {
    let val = args?.get(key)?;

    // Normal path: native JSON array.
    if let Some(arr) = val.as_array() {
        let mut out = Vec::with_capacity(arr.len());
        for v in arr {
            out.push(v.as_str()?.to_string());
        }
        return Some(out);
    }

    // Fallback: some MCP bridges serialize arrays as JSON-encoded strings.
    // Example: { "paths": "[\"src/main.rs\",\"src/lib.rs\"]" }
    if let Some(s) = val.as_str() {
        if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(s) {
            let mut out = Vec::with_capacity(arr.len());
            for v in &arr {
                out.push(v.as_str()?.to_string());
            }
            return Some(out);
        }
    }

    None
}

pub fn get_str(args: Option<&serde_json::Map<String, Value>>, key: &str) -> Option<String> {
    args?
        .get(key)?
        .as_str()
        .map(std::string::ToString::to_string)
}

pub fn get_int(args: Option<&serde_json::Map<String, Value>>, key: &str) -> Option<i64> {
    args?.get(key)?.as_i64()
}

pub fn get_bool(args: Option<&serde_json::Map<String, Value>>, key: &str) -> Option<bool> {
    args?.get(key)?.as_bool()
}

pub fn md5_hex(s: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Fast MD5 fingerprint for dedup purposes.
/// Hashes prefix + suffix + length for strings larger than 16 KB to avoid
/// O(n) hashing on multi-megabyte tool outputs.
pub fn md5_hex_fast(s: &str) -> String {
    use md5::{Digest, Md5};
    const THRESHOLD: usize = 16 * 1024;
    let mut hasher = Md5::new();
    if s.len() <= THRESHOLD {
        hasher.update(s.as_bytes());
    } else {
        hasher.update(&s.as_bytes()[..8192]);
        hasher.update(&s.as_bytes()[s.len() - 8192..]);
        hasher.update(s.len().to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub fn canonicalize_json(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                if let Some(val) = map.get(k) {
                    out.insert(k.clone(), canonicalize_json(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(canonicalize_json).collect()),
        other => other.clone(),
    }
}

pub fn canonical_args_string(args: Option<&serde_json::Map<String, Value>>) -> String {
    let v = args.map_or(Value::Null, |m| Value::Object(m.clone()));
    let canon = canonicalize_json(&v);
    serde_json::to_string(&canon).unwrap_or_default()
}

pub fn extract_search_pattern_from_command(command: &str) -> Option<String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }
    let cmd = parts[0];
    if cmd == "grep" || cmd == "rg" || cmd == "ag" || cmd == "ack" {
        for (i, part) in parts.iter().enumerate().skip(1) {
            if !part.starts_with('-') {
                return Some(part.to_string());
            }
            if (*part == "-e" || *part == "--regexp" || *part == "-m") && i + 1 < parts.len() {
                return Some(parts[i + 1].to_string());
            }
        }
    }
    if cmd == "find" || cmd == "fd" {
        for (i, part) in parts.iter().enumerate() {
            if (*part == "-name" || *part == "-iname") && i + 1 < parts.len() {
                return Some(
                    parts[i + 1]
                        .trim_matches('\'')
                        .trim_matches('"')
                        .to_string(),
                );
            }
        }
        if cmd == "fd" && parts.len() >= 2 && !parts[1].starts_with('-') {
            return Some(parts[1].to_string());
        }
    }
    None
}
