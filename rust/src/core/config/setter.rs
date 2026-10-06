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
pub fn set_by_key(key: &str, value: &str) -> Result<Config, crate::core::error::ConfigError> {
    set_many_by_key(&[(key, value)])
}

/// Atomically validates and persists several schema-known keys.
///
/// The config file is written only after every key and value has passed schema
/// validation and the resulting document deserializes into [`Config`].
///
/// The read, the merge, and the write all happen under the one config write
/// lock (LR-TEL-01). Previously the on-disk table was read unlocked and saved
/// later, so a concurrent writer could commit between the two and have its
/// value silently overwritten by this document.
pub fn set_many_by_key(
    updates: &[(&str, &str)],
) -> Result<Config, crate::core::error::ConfigError> {
    let schema = ConfigSchema::generate();
    let path = Config::path().ok_or(crate::core::error::ConfigError::MissingPath)?;
    let guard = crate::config_io::ConfigWriteGuard::acquire(&path).map_err(|message| {
        crate::core::error::ConfigError::Save {
            source: Box::new(crate::core::error::LeanCtxError::Config(message)),
        }
    })?;
    // Read the same resolved file the guard excludes writes to.
    let mut table = load_config_as_table_at(guard.path())?;
    for (key, value) in updates {
        let key_schema =
            schema
                .lookup(key)
                .ok_or_else(|| crate::core::error::ConfigError::UnknownKey {
                    key: (*key).to_string(),
                })?;
        let toml_value = parse_value(value, key_schema)?;
        set_nested(&mut table, key, toml_value)?;
    }

    let cfg: Config = toml::Value::Table(table)
        .try_into()
        .map_err(
            |e: toml::de::Error| crate::core::error::ConfigError::InvalidValue {
                key: updates
                    .iter()
                    .map(|(key, _)| *key)
                    .collect::<Vec<_>>()
                    .join(","),
                message: e.to_string(),
            },
        )?;
    // Already holding the guard: write through it so the non-reentrant lock is
    // not acquired a second time on this path.
    cfg.save_to_locked(&guard)
        .map_err(|e| crate::core::error::ConfigError::Save {
            source: Box::new(e),
        })?;
    Ok(cfg)
}

/// Returns the current value of `key` from the on-disk `config.toml`, rendered
/// as the string a user would type (no TOML quoting), or `None` if the key is
/// unset (still at its schema default).
///
/// Powers the before→after review for consequential `config set` writes (#852).
/// Reads only the user's file, so an unset key is reported as `None` rather than
/// the default — the review then shows `(default) → <new>`.
#[must_use]
pub fn current_value(key: &str) -> Option<String> {
    let table = load_config_as_table().ok()?;
    let parts: Vec<&str> = key.split('.').collect();
    let (parents, leaf) = parts.split_at(parts.len() - 1);

    let mut current = &table;
    for part in parents {
        current = current.get(*part)?.as_table()?;
    }
    current.get(leaf[0]).map(display_toml_value)
}

/// Renders a scalar `toml::Value` the way a user types it on the CLI: strings
/// without quotes, arrays comma-joined, everything else via its TOML form.
fn display_toml_value(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Array(items) => items
            .iter()
            .map(display_toml_value)
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

fn load_config_as_table() -> Result<toml::Table, crate::core::error::ConfigError> {
    let path = Config::path().ok_or(crate::core::error::ConfigError::MissingPath)?;
    load_config_as_table_at(&path)
}

/// Path-parameterized core of [`load_config_as_table`], so a caller that has
/// already resolved and locked the config path reads exactly that file.
fn load_config_as_table_at(
    path: &std::path::Path,
) -> Result<toml::Table, crate::core::error::ConfigError> {
    if !path.exists() {
        return Ok(toml::Table::new());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|source| crate::core::error::ConfigError::Read { source })?;
    raw.parse::<toml::Table>()
        .map_err(|source| crate::core::error::ConfigError::ParseToml { source })
}
fn parse_value(
    value: &str,
    schema: &KeySchema,
) -> Result<toml::Value, crate::core::error::ConfigError> {
    match schema.ty.as_str() {
        "bool" | "bool?" => match value {
            "true" | "1" | "yes" => Ok(toml::Value::Boolean(true)),
            "false" | "0" | "no" => Ok(toml::Value::Boolean(false)),
            _ => Err(crate::core::error::ConfigError::ExpectedBool {
                value: value.to_string(),
            }),
        },
        "u8" | "u16" | "u32" | "u64" | "usize" | "u64?" => {
            let n: i64 =
                value
                    .parse()
                    .map_err(|_| crate::core::error::ConfigError::ExpectedInteger {
                        value: value.to_string(),
                    })?;
            if n < 0 {
                return Err(crate::core::error::ConfigError::ExpectedUnsignedInteger {
                    value: value.to_string(),
                });
            }
            Ok(toml::Value::Integer(n))
        }
        "f32" | "f64" => {
            let n: f64 =
                value
                    .parse()
                    .map_err(|_| crate::core::error::ConfigError::ExpectedNumber {
                        value: value.to_string(),
                    })?;
            Ok(toml::Value::Float(n))
        }
        "string" | "string?" => Ok(toml::Value::String(value.to_string())),
        "enum" => {
            if let Some(ref allowed) = schema.values
                && !allowed.iter().any(|v| v == value)
            {
                return Err(crate::core::error::ConfigError::InvalidEnumValue {
                    value: value.to_string(),
                    allowed: allowed.join(", "),
                });
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
        "table" => Err(crate::core::error::ConfigError::CannotSetTable {
            value: value.to_string(),
        }),
        other => {
            // Fallback: treat as string (covers unknown future types gracefully)
            tracing::debug!("Unknown schema type '{other}', treating value as string");
            Ok(toml::Value::String(value.to_string()))
        }
    }
}

fn set_nested(
    table: &mut toml::Table,
    key: &str,
    value: toml::Value,
) -> Result<(), crate::core::error::ConfigError> {
    let parts: Vec<&str> = key.split('.').collect();
    let (parents, leaf) = parts.split_at(parts.len() - 1);

    let mut current = table;
    for part in parents {
        current = current
            .entry(*part)
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or_else(|| crate::core::error::ConfigError::NonTableParent {
                key: key.to_string(),
                part: (*part).to_string(),
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
    fn decision_loop_keys_round_trip_through_cli_setter() {
        let schema = ConfigSchema::generate();
        let mut table = toml::Table::new();
        for (key, value) in [
            ("decision_loop.enabled", "false"),
            ("decision_loop.max_filter_level", "0"),
        ] {
            let key_schema = schema.lookup(key).expect("decision-loop key in schema");
            set_nested(&mut table, key, parse_value(value, key_schema).unwrap()).unwrap();
        }

        let cfg: Config = toml::Value::Table(table).try_into().unwrap();
        assert!(!cfg.decision_loop.enabled);
        assert_eq!(cfg.decision_loop.max_filter_level, 0);
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
        assert!(err.to_string().contains("non-table"), "got: {err}");
    }

    #[test]
    fn display_toml_value_renders_user_facing_form() {
        // Strings drop their quotes (what a user would type on the CLI).
        assert_eq!(
            display_toml_value(&toml::Value::String("enforce".into())),
            "enforce"
        );
        assert_eq!(display_toml_value(&toml::Value::Boolean(false)), "false");
        assert_eq!(display_toml_value(&toml::Value::Integer(8080)), "8080");
        assert_eq!(
            display_toml_value(&toml::Value::Array(vec![
                toml::Value::String("a".into()),
                toml::Value::String("b".into()),
            ])),
            "a, b"
        );
    }
}
