// SPDX-License-Identifier: Apache-2.0

//! Delivery namespace contract. Parsing never establishes caller authority.
use serde::{Deserialize, Serialize};

/// Both identifiers must come from authenticated host/transport state at use.
/// Private fields prevent construction of an unvalidated namespace.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawScope")]
pub struct DeliveryScopeV1 {
    account_id: String,
    project_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScope {
    account_id: String,
    project_id: String,
}

impl TryFrom<RawScope> for DeliveryScopeV1 {
    type Error = String;
    fn try_from(raw: RawScope) -> Result<Self, Self::Error> {
        Self::new(raw.account_id, raw.project_id)
    }
}

impl DeliveryScopeV1 {
    pub fn new(account_id: String, project_id: String) -> Result<Self, String> {
        for (name, value) in [("account_id", &account_id), ("project_id", &project_id)] {
            if !valid_id(value) {
                return Err(format!("invalid delivery {name}"));
            }
        }
        Ok(Self {
            account_id,
            project_id,
        })
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }
    pub fn project_id(&self) -> &str {
        &self.project_id
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

/// No default: absent or unknown privacy must never silently broaden access.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryPrivacyV1 {
    Private,
    Project,
}

/// Explicit access metadata for a scoped delivery; neither field has a default.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryAccessV1 {
    pub scope: DeliveryScopeV1,
    pub privacy: DeliveryPrivacyV1,
}

impl DeliveryPrivacyV1 {
    /// The caller must authenticate the requester scope and agent before use.
    /// Even project-shared content never crosses an account or project boundary.
    pub fn permits(
        self,
        recorded_scope: &DeliveryScopeV1,
        authenticated_scope: &DeliveryScopeV1,
        original_agent: &str,
        authenticated_agent: &str,
    ) -> bool {
        recorded_scope == authenticated_scope
            && valid_id(original_agent)
            && valid_id(authenticated_agent)
            && (self == Self::Project || original_agent == authenticated_agent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_requires_both_bounded_identifiers() {
        for value in [
            serde_json::json!({"account_id":"a"}),
            serde_json::json!({"account_id":"a","project_id":null}),
            serde_json::json!({"account_id":"a","project_id":""}),
            serde_json::json!({"account_id":"a","project_id":"p","extra":true}),
            serde_json::json!({"account_id":"a","project_id":"x".repeat(257)}),
            serde_json::json!({"account_id":"a\n","project_id":"p"}),
        ] {
            assert!(serde_json::from_value::<DeliveryScopeV1>(value).is_err());
        }
        let scope = DeliveryScopeV1::new("a".into(), "p".into()).unwrap();
        assert_eq!(
            serde_json::from_str::<DeliveryScopeV1>(&serde_json::to_string(&scope).unwrap())
                .unwrap(),
            scope
        );
        assert!(serde_json::from_str::<DeliveryPrivacyV1>("\"unknown\"").is_err());
        assert!(serde_json::from_str::<DeliveryPrivacyV1>("null").is_err());
        for value in [
            serde_json::json!({"scope":{"account_id":"a","project_id":"p"}}),
            serde_json::json!({"scope":{"account_id":"a","project_id":"p"},"privacy":"public"}),
            serde_json::json!({"scope":{"account_id":"a","project_id":"p"},"privacy":"project","extra":true}),
        ] {
            assert!(serde_json::from_value::<DeliveryAccessV1>(value).is_err());
        }
    }

    #[test]
    fn privacy_never_crosses_account_or_project() {
        let scope = DeliveryScopeV1::new("a".into(), "p".into()).unwrap();
        for other in [
            DeliveryScopeV1::new("other".into(), "p".into()).unwrap(),
            DeliveryScopeV1::new("a".into(), "other".into()).unwrap(),
        ] {
            for privacy in [DeliveryPrivacyV1::Private, DeliveryPrivacyV1::Project] {
                assert!(!privacy.permits(&scope, &other, "worker", "worker"));
            }
        }
        assert!(DeliveryPrivacyV1::Project.permits(&scope, &scope, "a", "b"));
        assert!(!DeliveryPrivacyV1::Private.permits(&scope, &scope, "a", "b"));
        assert!(DeliveryPrivacyV1::Private.permits(&scope, &scope, "a", "a"));
        assert!(!DeliveryPrivacyV1::Project.permits(&scope, &scope, "a", ""));
    }
}
// SPDX-License-Identifier: Apache-2.0
