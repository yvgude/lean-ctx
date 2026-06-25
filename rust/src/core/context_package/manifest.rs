use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::graph_model::{GraphSummary, MarketplaceMeta};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageManifest {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conformance_level: Option<u32>,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    pub layers: Vec<PackageLayer>,
    #[serde(default)]
    pub dependencies: Vec<PackageDependency>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Registry visibility: `private` hides the package from catalog/search;
    /// installs then need a namespace token (GL #524). `None` = public.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    pub integrity: PackageIntegrity,
    pub provenance: PackageProvenance,
    #[serde(default)]
    pub compatibility: CompatibilitySpec,
    #[serde(default)]
    pub stats: PackageStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<PackageSignature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_summary: Option<GraphSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketplace: Option<MarketplaceMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSignature {
    pub algorithm: String,
    pub public_key: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageLayer {
    Knowledge,
    Graph,
    Session,
    Patterns,
    Gotchas,
}

impl PackageLayer {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Knowledge => "knowledge",
            Self::Graph => "graph",
            Self::Session => "session",
            Self::Patterns => "patterns",
            Self::Gotchas => "gotchas",
        }
    }

    #[must_use]
    pub fn filename(&self) -> &'static str {
        match self {
            Self::Knowledge => "knowledge.json",
            Self::Graph => "graph.json",
            Self::Session => "session.json",
            Self::Patterns => "patterns.json",
            Self::Gotchas => "gotchas.json",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageDependency {
    pub name: String,
    pub version_req: String,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageIntegrity {
    pub sha256: String,
    pub content_hash: String,
    pub byte_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageProvenance {
    pub tool: String,
    pub tool_version: String,
    pub project_hash: Option<String>,
    #[serde(default)]
    pub source_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CompatibilitySpec {
    #[serde(default)]
    pub min_lean_ctx_version: Option<String>,
    #[serde(default)]
    pub target_languages: Vec<String>,
    #[serde(default)]
    pub target_frameworks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackageStats {
    pub knowledge_facts: u32,
    pub graph_nodes: u32,
    pub graph_edges: u32,
    pub pattern_count: u32,
    pub gotcha_count: u32,
    pub compression_ratio: f64,
}

impl PackageManifest {
    #[must_use]
    pub fn is_v2(&self) -> bool {
        self.schema_version >= crate::core::contracts::CONTEXT_PACKAGE_V2_SCHEMA_VERSION
    }

    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        let v1 = crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION;
        let v2 = crate::core::contracts::CONTEXT_PACKAGE_V2_SCHEMA_VERSION;
        if self.schema_version != v1 && self.schema_version != v2 {
            errors.push(format!(
                "unsupported schema_version {} (expected {v1} or {v2})",
                self.schema_version,
            ));
        }
        if self.name.is_empty() {
            errors.push("name must not be empty".into());
        }
        if self.name.len() > 128 {
            errors.push("name must be <= 128 characters".into());
        }
        if !self.name.chars().all(|c| {
            c.is_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '@' || c == '/'
        }) {
            errors.push("name must only contain [a-zA-Z0-9._@/-]".into());
        }
        if self.version.is_empty() {
            errors.push("version must not be empty".into());
        }
        if self.version.len() > 64 {
            errors.push("version must be <= 64 characters".into());
        }
        if !self
            .version
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '+')
        {
            errors.push("version must only contain [a-zA-Z0-9._+-]".into());
        }
        if self.version.starts_with('.') {
            errors.push("version must not start with '.'".into());
        }
        if self.layers.is_empty() && !self.is_v2() {
            errors.push("at least one layer is required".into());
        }
        let mut seen_layers = std::collections::HashSet::new();
        for layer in &self.layers {
            if !seen_layers.insert(layer.as_str()) {
                errors.push(format!("duplicate layer: {}", layer.as_str()));
            }
        }
        if self.integrity.sha256.len() != 64
            || !self.integrity.sha256.chars().all(|c| c.is_ascii_hexdigit())
        {
            errors.push("integrity.sha256 must be a 64-char hex string".into());
        }
        if self.integrity.content_hash.len() != 64
            || !self
                .integrity
                .content_hash
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        {
            errors.push("integrity.content_hash must be a 64-char hex string".into());
        }
        if self.integrity.byte_size == 0 {
            errors.push("integrity.byte_size must be > 0".into());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    #[must_use]
    pub fn has_layer(&self, layer: PackageLayer) -> bool {
        self.layers.contains(&layer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_manifest() -> PackageManifest {
        PackageManifest {
            schema_version: crate::core::contracts::CONTEXT_PACKAGE_V1_SCHEMA_VERSION,
            conformance_level: None,
            name: "test-pkg".into(),
            version: "0.1.0".into(),
            description: "A test package".into(),
            author: None,
            scope: None,
            created_at: Utc::now(),
            updated_at: None,
            layers: vec![PackageLayer::Knowledge],
            dependencies: vec![],
            tags: vec![],
            visibility: None,
            integrity: PackageIntegrity {
                sha256: "a".repeat(64),
                content_hash: "b".repeat(64),
                byte_size: 100,
            },
            provenance: PackageProvenance {
                tool: "lean-ctx".into(),
                tool_version: env!("CARGO_PKG_VERSION").into(),
                project_hash: None,
                source_session_id: None,
            },
            compatibility: CompatibilitySpec::default(),
            stats: PackageStats::default(),
            signature: None,
            graph_summary: None,
            marketplace: None,
        }
    }

    #[test]
    fn valid_manifest_passes() {
        assert!(minimal_manifest().validate().is_ok());
    }

    #[test]
    fn empty_name_fails() {
        let mut m = minimal_manifest();
        assert!(m.validate().is_ok());
        m.name = String::new();
        assert!(m.validate().is_err());
    }

    #[test]
    fn duplicate_layers_fails() {
        let mut m = minimal_manifest();
        m.layers = vec![PackageLayer::Knowledge, PackageLayer::Knowledge];
        assert!(m.validate().is_err());
    }

    #[test]
    fn non_hex_sha256_fails() {
        let mut m = minimal_manifest();
        m.integrity.sha256 = "z".repeat(64);
        assert!(m.validate().is_err());
    }

    #[test]
    fn invalid_name_chars_fails() {
        let mut m = minimal_manifest();
        m.name = "my package!".into();
        let errs = m.validate().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("only contain")));
    }

    #[test]
    fn v2_schema_version_validates() {
        let mut m = minimal_manifest();
        m.schema_version = crate::core::contracts::CONTEXT_PACKAGE_V2_SCHEMA_VERSION;
        assert!(m.validate().is_ok());
    }

    #[test]
    fn scoped_name_validates() {
        let mut m = minimal_manifest();
        m.name = "@company/auth-service".into();
        assert!(m.validate().is_ok());
    }

    #[test]
    fn is_v2_flag() {
        let mut m = minimal_manifest();
        assert!(!m.is_v2());
        m.schema_version = crate::core::contracts::CONTEXT_PACKAGE_V2_SCHEMA_VERSION;
        assert!(m.is_v2());
    }

    #[test]
    fn unsupported_schema_version_fails() {
        let mut m = minimal_manifest();
        m.schema_version = 99;
        let errs = m.validate().unwrap_err();
        assert!(
            errs.iter()
                .any(|e| e.contains("unsupported schema_version"))
        );
    }

    #[test]
    fn v2_manifest_serde_roundtrip() {
        use super::super::graph_model::{GraphSummary, MarketplaceMeta};

        let mut m = minimal_manifest();
        m.schema_version = 2;
        m.conformance_level = Some(2);
        m.scope = Some("@company".into());
        m.graph_summary = Some(GraphSummary {
            node_count: 42,
            edge_count: 100,
            node_types: vec!["fact".into(), "gotcha".into()],
            activation_mean: Some(0.75),
            freshness: Some(Utc::now()),
        });
        m.marketplace = Some(MarketplaceMeta {
            categories: vec!["security".into()],
            badges: vec!["verified".into()],
            license: Some("MIT".into()),
        });

        let json = serde_json::to_string(&m).unwrap();
        let decoded: PackageManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.schema_version, 2);
        assert_eq!(decoded.conformance_level, Some(2));
        assert_eq!(decoded.scope.as_deref(), Some("@company"));
        let gs = decoded.graph_summary.unwrap();
        assert_eq!(gs.node_count, 42);
        assert_eq!(gs.edge_count, 100);
        let mp = decoded.marketplace.unwrap();
        assert_eq!(mp.categories, vec!["security"]);
        assert_eq!(mp.license.as_deref(), Some("MIT"));
    }

    #[test]
    fn v1_manifest_missing_v2_fields_deserializes() {
        let json = serde_json::to_string(&minimal_manifest()).unwrap();
        let decoded: PackageManifest = serde_json::from_str(&json).unwrap();
        assert!(decoded.conformance_level.is_none());
        assert!(decoded.scope.is_none());
        assert!(decoded.graph_summary.is_none());
        assert!(decoded.marketplace.is_none());
    }

    #[test]
    fn nested_scope_validates() {
        let mut m = minimal_manifest();
        m.name = "@org/team/service".into();
        assert!(m.validate().is_ok());
    }
}
