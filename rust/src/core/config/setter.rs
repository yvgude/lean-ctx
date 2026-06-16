//! Generic config setter that works for ALL schema-known keys.
//!
//! Instead of a hardcoded match arm per key, this module:
//! 1. Validates the key against `ConfigSchema`
//! 2. Parses the value according to the schema type
//! 3. Performs a TOML round-trip to set the value
//! 4. Deserializes back into `Config` for full serde validation

use super::Config;
use super::schema::{ConfigSchema, KeySchema};

/// Attempts to set a config key generically via schema-validated TOML round-trip.
///
/// Returns the updated `Config` on success, or a user-friendly error message.
pub fn set_by_key(key: &str, value: &str) -> Result<Config, String> {
    let schema = ConfigSchema::generate();
    let key_schema = schema
        .lookup(key)
        .ok_or_else(|| format!("Unknown config key: {key}"))?;

    let mut table = load_config_as_table()?;
    let toml_value = parse_value(value, key_schema)?;
    set_nested(&mut table, key, toml_value)?;

    let cfg: Config = toml::Value::Table(table)
        .try_into()
        .map_err(|e| format!("Invalid value for '{key}': {e}"))?;
    cfg.save()
        .map_err(|e| format!("Error saving config: {e}"))?;
    Ok(cfg)
}

/// Loads the current config file as a raw TOML table.
/// If no file exists, returns an empty table (fresh config).
fn load_config_as_table() -> Result<toml::Table, String> {
    let path = Config::path().ok_or("Cannot determine config path")?;
    if !path.exists() {
        return Ok(toml::Table::new());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("Cannot read config: {e}"))?;
    raw.parse::<toml::Table>()
        .map_err(|e| format!("Config parse error: {e}"))
}

/// Parses a string value into the appropriate `toml::Value` based on schema type.
fn parse_value(value: &str, schema: &KeySchema) -> Result<toml::Value, String> {
    match schema.ty.as_str() {
        "bool" | "bool?" => match value {
            "true" | "1" | "yes" => Ok(toml::Value::Boolean(true)),
            "false" | "0" | "no" => Ok(toml::Value::Boolean(false)),
            _ => Err(format!("Expected bool (true/false), got: {value}")),
        },
        "u8" | "u16" | "u32" | "u64" | "usize" | "u64?" => {
            let n: i64 = value
                .parse()
                .map_err(|_| format!("Expected integer, got: {value}"))?;
            if n < 0 {
                return Err(format!("Expected unsigned integer, got: {value}"));
            }
            Ok(toml::Value::Integer(n))
        }
        "f32" | "f64" => {
            let n: f64 = value
                .parse()
                .map_err(|_| format!("Expected number, got: {value}"))?;
            Ok(toml::Value::Float(n))
        }
        "string" | "string?" => Ok(toml::Value::String(value.to_string())),
        "enum" => {
            if let Some(ref allowed) = schema.values
                && !allowed.iter().any(|v| v == value)
            {
                return Err(format!(
                    "Invalid value '{value}'. Allowed: {}",
                    allowed.join(", ")
                ));
            }
            Ok(toml::Value::String(value.to_string()))
        }
        "string[]" | "array" => {
            let items: Vec<toml::Value> = value
                .split(',')
                .map(|s| toml::Value::String(s.trim().to_string()))
                .filter(|v| v.as_str() != Some(""))
                .collect();
            Ok(toml::Value::Array(items))
        }
        "table" => Err(format!(
            "Cannot set table '{value}' via CLI. Edit config.toml directly."
        )),
        other => {
            // Fallback: treat as string (covers unknown future types gracefully)
            tracing::debug!("Unknown schema type '{other}', treating value as string");
            Ok(toml::Value::String(value.to_string()))
        }
    }
}

/// Sets a value in a nested TOML table using a dot-separated key path.
/// Creates intermediate tables as needed. Returns an error (rather than
/// panicking) if an intermediate key already holds a non-table value in the
/// user's `config.toml` — e.g. `proxy = "x"` then `config set proxy.port 1`.
fn set_nested(table: &mut toml::Table, key: &str, value: toml::Value) -> Result<(), String> {
    let parts: Vec<&str> = key.split('.').collect();
    let (parents, leaf) = parts.split_at(parts.len() - 1);

    let mut current = table;
    for part in parents {
        current = current
            .entry(*part)
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or_else(|| {
                format!(
                    "Cannot set '{key}': '{part}' already holds a non-table value in config.toml. \
                     Fix or remove that key first."
                )
            })?;
    }
    current.insert(leaf[0].to_string(), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bool_values() {
        let schema = KeySchema {
            ty: "bool".to_string(),
            default: serde_json::json!(false),
            description: String::new(),
            values: None,
            env_override: None,
        };
        assert_eq!(
            parse_value("true", &schema).unwrap(),
            toml::Value::Boolean(true)
        );
        assert_eq!(
            parse_value("false", &schema).unwrap(),
            toml::Value::Boolean(false)
        );
        assert!(parse_value("maybe", &schema).is_err());
    }

    #[test]
    fn parse_integer_values() {
        let schema = KeySchema {
            ty: "u32".to_string(),
            default: serde_json::json!(0),
            description: String::new(),
            values: None,
            env_override: None,
        };
        assert_eq!(
            parse_value("42", &schema).unwrap(),
            toml::Value::Integer(42)
        );
        assert!(parse_value("-1", &schema).is_err());
        assert!(parse_value("abc", &schema).is_err());
    }

    #[test]
    fn parse_enum_validates_allowed() {
        let schema = KeySchema {
            ty: "enum".to_string(),
            default: serde_json::json!("off"),
            description: String::new(),
            values: Some(vec!["off".into(), "lite".into(), "full".into()]),
            env_override: None,
        };
        assert_eq!(
            parse_value("lite", &schema).unwrap(),
            toml::Value::String("lite".into())
        );
        assert!(parse_value("invalid", &schema).is_err());
    }

    #[test]
    fn parse_string_array() {
        let schema = KeySchema {
            ty: "string[]".to_string(),
            default: serde_json::json!([]),
            description: String::new(),
            values: None,
            env_override: None,
        };
        let result = parse_value("a, b, c", &schema).unwrap();
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0].as_str().unwrap(), "a");
        assert_eq!(arr[2].as_str().unwrap(), "c");
    }

    #[test]
    fn set_nested_creates_intermediate_tables() {
        let mut table = toml::Table::new();
        set_nested(
            &mut table,
            "proxy.anthropic_upstream",
            toml::Value::String("https://example.com".into()),
        )
        .unwrap();
        let proxy = table["proxy"].as_table().unwrap();
        assert_eq!(
            proxy["anthropic_upstream"].as_str().unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn set_nested_flat_key() {
        let mut table = toml::Table::new();
        set_nested(&mut table, "ultra_compact", toml::Value::Boolean(true)).unwrap();
        assert!(table["ultra_compact"].as_bool().unwrap());
    }

    #[test]
    fn set_nested_rejects_non_table_intermediate() {
        let mut table = toml::Table::new();
        table.insert("proxy".into(), toml::Value::String("oops".into()));
        let err = set_nested(&mut table, "proxy.port", toml::Value::Integer(8080)).unwrap_err();
        assert!(err.contains("non-table"), "got: {err}");
    }
}
