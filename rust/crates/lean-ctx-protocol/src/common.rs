//! Shared wire primitives used by the version-one protocol contracts.

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{Error as DeError, MapAccess, Visitor},
    ser::{SerializeMap, Serializer},
};
use serde_json::Value;
use std::{collections::BTreeMap, error::Error, fmt, ops::Index};

/// The schema version implemented by this crate.
pub const V1_SCHEMA_VERSION: u32 = 1;

/// Maximum size for an opaque identifier on the wire.
pub const MAX_IDENTIFIER_LENGTH: usize = 256;

/// Maximum number of references in one V1 protocol collection.
pub const MAX_PROTOCOL_ITEMS: usize = 256;

/// Maximum number of additive fields retained on one V1 object.
pub const MAX_EXTENSION_FIELDS: usize = MAX_PROTOCOL_ITEMS;
/// Maximum serialized JSON size of one additive field value.
pub const MAX_EXTENSION_VALUE_BYTES: usize = 64 * 1024;
/// Maximum nesting depth of an additive JSON value.
pub const MAX_EXTENSION_DEPTH: usize = 8;

/// Ordered, bounded storage for additive top-level V1 fields.
///
/// The map is intentionally a newtype instead of an alias so both deserialization
/// and programmatic insertion enforce the same bounds. Each owning DTO applies its
/// own reserved-field validation when it validates the decoded value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionsV1(BTreeMap<String, Value>);

impl ExtensionsV1 {
    /// Add one extension after validating its key and JSON value.
    pub fn insert(
        &mut self,
        key: impl Into<String>,
        value: Value,
    ) -> Result<Option<Value>, ValidationError> {
        let key = key.into();
        validate_extension_key(&key)?;
        validate_extension_value(&value, 0)?;
        if !self.0.contains_key(&key) && self.0.len() >= MAX_EXTENSION_FIELDS {
            return Err(ValidationError::new(format!(
                "extensions exceeds the {MAX_EXTENSION_FIELDS} field limit"
            )));
        }
        Ok(self.0.insert(key, value))
    }

    /// Return one retained additive value.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    /// Check whether one additive field is retained.
    pub fn contains_key(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// Iterate over retained additive fields in lexical order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.0.iter()
    }

    /// Borrow the validated map.
    pub fn as_map(&self) -> &BTreeMap<String, Value> {
        &self.0
    }

    /// Consume the wrapper and return the validated map.
    pub fn into_inner(self) -> BTreeMap<String, Value> {
        self.0
    }
}

impl Index<&str> for ExtensionsV1 {
    type Output = Value;

    fn index(&self, key: &str) -> &Self::Output {
        &self.0[key]
    }
}

impl Serialize for ExtensionsV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

struct ExtensionsVisitor;

impl<'de> Visitor<'de> for ExtensionsVisitor {
    type Value = ExtensionsV1;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON object of additive fields")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut extensions = ExtensionsV1::default();
        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            if extensions.contains_key(&key) {
                return Err(A::Error::custom(format!("duplicate extension key {key:?}")));
            }
            extensions.insert(key, value).map_err(A::Error::custom)?;
        }
        Ok(extensions)
    }
}

impl<'de> Deserialize<'de> for ExtensionsV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ExtensionsVisitor)
    }
}

fn validate_extension_key(key: &str) -> Result<(), ValidationError> {
    validate_bounded_opaque_identifier(key, "extension key")
}

impl ExtensionsV1 {
    /// Reject keys owned by the enclosing DTO's wire contract.
    pub(crate) fn validate_reserved(&self, reserved: &[&str]) -> Result<(), ValidationError> {
        if let Some(key) = self.0.keys().find(|key| reserved.contains(&key.as_str())) {
            return Err(ValidationError::new(format!(
                "extension key {key:?} collides with a reserved field"
            )));
        }
        Ok(())
    }
}

fn validate_extension_value(value: &Value, depth: usize) -> Result<(), ValidationError> {
    if depth > MAX_EXTENSION_DEPTH {
        return Err(ValidationError::new(format!(
            "extension value exceeds nesting depth {MAX_EXTENSION_DEPTH}"
        )));
    }
    match value {
        Value::Array(values) => {
            if values.len() > MAX_PROTOCOL_ITEMS {
                return Err(ValidationError::new("extension array exceeds item limit"));
            }
            for value in values {
                validate_extension_value(value, depth + 1)?;
            }
        }
        Value::Object(values) => {
            if values.len() > MAX_PROTOCOL_ITEMS {
                return Err(ValidationError::new("extension object exceeds field limit"));
            }
            for (key, value) in values {
                validate_bounded_opaque_identifier(key, "extension object key")?;
                validate_extension_value(value, depth + 1)?;
            }
        }
        Value::String(value) if value.len() > MAX_EXTENSION_VALUE_BYTES => {
            return Err(ValidationError::new("extension string exceeds byte limit"));
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    let encoded = serde_json::to_vec(value)
        .map_err(|error| ValidationError::new(format!("invalid extension value: {error}")))?;
    if encoded.len() > MAX_EXTENSION_VALUE_BYTES {
        return Err(ValidationError::new(
            "extension value exceeds serialized byte limit",
        ));
    }
    Ok(())
}

/// Validate an opaque wire identifier before it crosses a protocol boundary.
pub(crate) fn validate_bounded_opaque_identifier(
    value: &str,
    type_name: &str,
) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::new(format!(
            "{type_name} must not be empty"
        )));
    }
    if value.len() > MAX_IDENTIFIER_LENGTH {
        return Err(ValidationError::new(format!(
            "{type_name} exceeds the {MAX_IDENTIFIER_LENGTH} byte limit"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(ValidationError::new(format!(
            "{type_name} must not contain control characters"
        )));
    }
    Ok(())
}

/// Error returned when a contract value cannot satisfy a wire invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError(pub(crate) String);

impl ValidationError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ValidationError {}

macro_rules! bounded_identifier {
    ($name:ident) => {
        /// Opaque, bounded identifier used by a protocol contract.
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Construct an identifier after applying the wire bounds.
            pub fn new(value: impl Into<String>) -> Result<Self, ValidationError> {
                let value = value.into();
                $crate::common::validate_bounded_opaque_identifier(&value, stringify!($name))?;
                Ok(Self(value))
            }

            /// Borrow the identifier's opaque wire value.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the identifier and return its wire value.
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl std::str::FromStr for $name {
            type Err = ValidationError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = ValidationError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = ValidationError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(DeError::custom)
            }
        }
    };
}

bounded_identifier!(TaskId);
bounded_identifier!(TraceId);
bounded_identifier!(ProjectId);
bounded_identifier!(SessionId);
bounded_identifier!(AgentId);
bounded_identifier!(TenantId);
bounded_identifier!(PlanId);
bounded_identifier!(ReceiptId);
bounded_identifier!(OutcomeId);
bounded_identifier!(CapabilityId);
bounded_identifier!(DecisionId);
bounded_identifier!(ContextPlanId);
bounded_identifier!(AttemptId);

/// Validate a bounded non-empty wire string.
pub fn validate_bounded_string(value: &str, field: &str) -> Result<(), ValidationError> {
    validate_bounded_opaque_identifier(value, field)
}

/// Validate a bounded collection and reject duplicates.
pub fn validate_unique_strings<T>(values: &[T], field: &str) -> Result<(), ValidationError>
where
    T: AsRef<str>,
{
    if values.len() > MAX_PROTOCOL_ITEMS {
        return Err(ValidationError::new(format!(
            "{field} exceeds the {MAX_PROTOCOL_ITEMS} item limit"
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        validate_bounded_string(value.as_ref(), field)?;
        if !seen.insert(value.as_ref()) {
            return Err(ValidationError::new(format!(
                "{field} contains duplicate values"
            )));
        }
    }
    Ok(())
}

/// Require the one schema version implemented by this crate during decoding.
pub fn deserialize_schema_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version == V1_SCHEMA_VERSION {
        Ok(version)
    } else {
        Err(DeError::custom(format!(
            "unsupported schema_version {version}; expected {V1_SCHEMA_VERSION}"
        )))
    }
}

/// Decode a required milliunit constrained to the inclusive 0..=1000 range.
pub fn deserialize_milliunit<'de, D>(deserializer: D) -> Result<u16, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u16::deserialize(deserializer)?;
    if value <= 1000 {
        Ok(value)
    } else {
        Err(DeError::custom("milliunit must be between 0 and 1000"))
    }
}

/// Decode an optional milliunit constrained to the inclusive 0..=1000 range.
pub fn deserialize_optional_milliunit<'de, D>(deserializer: D) -> Result<Option<u16>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<u16>::deserialize(deserializer)?;
    value.map_or(Ok(None), |value| {
        if value <= 1000 {
            Ok(Some(value))
        } else {
            Err(DeError::custom("milliunit must be between 0 and 1000"))
        }
    })
}

/// Validate a schema version on values built directly in Rust.
pub fn validate_schema_version(version: u32) -> Result<(), ValidationError> {
    if version == V1_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(ValidationError::new(format!(
            "unsupported schema_version {version}; expected {V1_SCHEMA_VERSION}"
        )))
    }
}

/// Validate a 0..=1000 milliunit value on values built directly in Rust.
pub fn validate_milliunit(value: u16, field: &str) -> Result<(), ValidationError> {
    if value <= 1000 {
        Ok(())
    } else {
        Err(ValidationError::new(format!(
            "{field} must be between 0 and 1000"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extensions_retain_unknown_fields_and_reject_reserved_names() {
        let value = json!({
            "future_flag": true,
            "future_nested": {"stable": ["value", 7]}
        });
        let extensions: ExtensionsV1 =
            serde_json::from_value(value.clone()).expect("extensions should deserialize");
        assert_eq!(
            serde_json::to_value(&extensions).expect("extensions should serialize"),
            value
        );
        let mut direct = ExtensionsV1::default();
        assert!(direct.insert("extensions", Value::from(true)).is_ok());
        assert!(direct.insert("schema_version", Value::from(1)).is_ok());
        assert!(direct.insert("future_flag", Value::from(true)).is_ok());
        assert!(direct.validate_reserved(&["schema_version"]).is_err());
        assert!(direct.validate_reserved(&["provider"]).is_ok());
    }

    #[test]
    fn extensions_are_bounded_by_count_and_value_shape() {
        let mut extensions = ExtensionsV1::default();
        for index in 0..MAX_EXTENSION_FIELDS {
            extensions
                .insert(format!("future_{index}"), Value::from(index))
                .expect("entry should fit");
        }
        assert!(
            extensions
                .insert("future_overflow", Value::from(true))
                .is_err()
        );
        assert!(
            extensions
                .insert(
                    "future_large",
                    Value::String("x".repeat(MAX_EXTENSION_VALUE_BYTES + 1))
                )
                .is_err()
        );
    }

    #[test]
    fn extensions_reject_duplicate_wire_keys() {
        let json = r#"{"future_key":1,"future_key":2}"#;
        assert!(serde_json::from_str::<ExtensionsV1>(json).is_err());
    }
}
