// SPDX-License-Identifier: Apache-2.0

//! Canonical v4 product-capability registry.
//!
//! Product access decisions are made from the embedded, validated registry.
//! Price, source visibility and commercial rights are independent axes.
//! Legacy IDs remain stable, but never determine price or source visibility.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::core::ocla::OclaCapabilityKind;

/// The plan ladder a capability's minimum is ranked on. Account plans live
/// in LeanCTX Hive; the engine only orders them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    #[serde(alias = "free", alias = "supporter", alias = "sponsor")]
    Community,
    Pro,
    #[serde(alias = "business", alias = "biz")]
    Team,
    #[serde(alias = "ent")]
    Enterprise,
}

impl Plan {
    pub const ALL: [Self; 4] = [Self::Community, Self::Pro, Self::Team, Self::Enterprise];

    #[must_use]
    pub const fn all() -> &'static [Self] {
        &Self::ALL
    }

    #[must_use]
    pub const fn rank(self) -> usize {
        match self {
            Self::Community => 0,
            Self::Pro => 1,
            Self::Team => 2,
            Self::Enterprise => 3,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Community => "community",
            Self::Pro => "pro",
            Self::Team => "team",
            Self::Enterprise => "enterprise",
        }
    }
}

/// Package-local mirror of the repository source of truth. CI requires this
/// byte-for-byte copy so source-package builds remain hermetic.
pub const REGISTRY_SOURCE: &str = include_str!("../../data/product-capabilities.toml");

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserPriceClass {
    Free,
    /// Paid right granted by a LeanCTX Hive plan; the record's
    /// [`ProductCapability::hive_plan`] names the plan.
    Hive,
    /// Withdrawn capability: still a known lookup name, never granted.
    Retired,
    /// Legacy (schema 2) class. Kept so the frozen hosted services and old
    /// envelopes keep decoding; no schema-3 record uses it.
    CloudPaid,
    /// Legacy (schema 2) Pro class of the signed private Intelligence Runtime;
    /// no schema-3 record uses it.
    ProPaid,
    /// Legacy (schema 2) Team class; no schema-3 record uses it.
    TeamPaid,
    /// Legacy (schema 2) Enterprise class; no schema-3 record uses it.
    EnterprisePaid,
    SdkOemCommercial,
}

/// The Hive plan that grants an `hive` capability (Enterprise ⊇ Team ⊇ Personal).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HivePlan {
    Personal,
    Team,
    Enterprise,
}

impl HivePlan {
    #[must_use]
    pub const fn plan(self) -> Plan {
        match self {
            Self::Personal => Plan::Pro,
            Self::Team => Plan::Team,
            Self::Enterprise => Plan::Enterprise,
        }
    }
}

/// Registry schema this Engine reads (3: `hive` and `retired` classes).
pub const REGISTRY_SCHEMA_VERSION: u16 = 3;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceVisibility {
    PublicApache,
    PublicEnterpriseSourceAvailable,
    PrivateSource,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDelivery {
    SourceBuild,
    SignedBinary,
    LocalService,
    ManagedCloudService,
    EnterprisePrivateImage,
    /// Delivered and controlled by the LeanCTX Hive service (filter packs,
    /// shared team state, governance), consumed by the local Engine.
    HiveService,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AhaCriticality {
    None,
    Supporting,
    IndividualAha,
    TeamAha,
    /// Withheld by the public projection; see [`public_projection`].
    Undisclosed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MoatSensitivity {
    Low,
    Medium,
    CrownJewel,
    /// Withheld by the public projection; see [`public_projection`].
    Undisclosed,
}

/// Marker value of the registry's top-level `projection` key.
pub const PUBLIC_PROJECTION: &str = "public";
/// `source_path` of a capability whose implementation is not public.
pub const UNDISCLOSED_SOURCE: &str = "undisclosed";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommercialUse {
    ApacheRights,
    FreeSmallTeamInternal,
    ServiceAgreement,
    EnterpriseAgreement,
    SdkOemAgreement,
    ThirdPartyTerms,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedistributionRights {
    ApacheRights,
    SeparateAgreementRequired,
    ThirdPartyTerms,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryEventClass {
    None,
    ProductUsage,
    BillingUsage,
    OperationalAggregate,
    Audit,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicStatus {
    Available,
    Preview,
    Internal,
    Deprecated,
    Research,
    Private,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseClass {
    ApacheTrustCore,
    CommercialSource,
    PrivateService,
    FreeRuntime,
    SdkOem,
    ThirdParty,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductCapability {
    pub id: String,
    pub name: String,
    pub price_class: UserPriceClass,
    /// Present exactly on `hive` records: the Hive plan that grants it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hive_plan: Option<HivePlan>,
    pub source_visibility: SourceVisibility,
    pub runtime_delivery: RuntimeDelivery,
    pub aha_criticality: AhaCriticality,
    pub moat_sensitivity: MoatSensitivity,
    pub license_class: LicenseClass,
    pub self_host_available: bool,
    pub managed_cloud_available: bool,
    /// Canonical governance key, or the explicit value `none`.
    pub enterprise_entitlement: String,
    pub account_required: bool,
    pub allowed_commercial_use: CommercialUse,
    pub redistribution_oem_rights: RedistributionRights,
    pub data_boundary: String,
    pub telemetry_event_class: TelemetryEventClass,
    pub public_maturity: PublicStatus,
    pub source_path: String,
}

impl ProductCapability {
    /// Compatibility projection for legacy clients; not entitlement authority.
    #[must_use]
    pub const fn minimum_plan(&self) -> Plan {
        match self.price_class {
            UserPriceClass::Free => Plan::Community,
            UserPriceClass::Hive => match self.hive_plan {
                Some(plan) => plan.plan(),
                // Unreachable for a validated record; fail toward the top plan.
                None => Plan::Enterprise,
            },
            UserPriceClass::CloudPaid | UserPriceClass::ProPaid => Plan::Pro,
            UserPriceClass::TeamPaid => Plan::Team,
            // A retired id is never granted (`allows` refuses it); it never
            // reads as Community either.
            UserPriceClass::EnterprisePaid
            | UserPriceClass::SdkOemCommercial
            | UserPriceClass::Retired => Plan::Enterprise,
        }
    }

    /// True for a capability whose entitlement is Enterprise authority: it
    /// must name itself as its canonical `enterprise_entitlement`.
    #[must_use]
    pub const fn is_enterprise(&self) -> bool {
        matches!(self.price_class, UserPriceClass::EnterprisePaid)
            || matches!(
                (self.price_class, self.hive_plan),
                (UserPriceClass::Hive, Some(HivePlan::Enterprise))
            )
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryDocument {
    schema_version: u16,
    /// Absent in the canonical registry; [`PUBLIC_PROJECTION`] in the public
    /// export, whose non-public records carry no strategic classification.
    #[serde(default)]
    projection: Option<String>,
    capability: Vec<ProductCapability>,
    /// Legacy wire/config lookup names, separate from the 18-field records.
    aliases: BTreeMap<String, String>,
}

#[derive(Debug)]
pub struct ProductCapabilityRegistry {
    entries: Vec<ProductCapability>,
    lookup: BTreeMap<String, usize>,
    projected: bool,
}

impl ProductCapabilityRegistry {
    pub fn parse(source: &str) -> Result<Self, String> {
        let document: RegistryDocument = toml::from_str(source)
            .map_err(|error| format!("invalid capability registry: {error}"))?;
        if document.schema_version != REGISTRY_SCHEMA_VERSION {
            return Err(format!(
                "unsupported capability registry schema {}",
                document.schema_version
            ));
        }
        if document.capability.is_empty() {
            return Err("capability registry is empty".to_string());
        }

        let projected = match document.projection.as_deref() {
            None => false,
            Some(PUBLIC_PROJECTION) => true,
            Some(other) => return Err(format!("unknown capability registry projection '{other}'")),
        };

        let mut lookup = BTreeMap::new();
        let mut ids = BTreeSet::new();
        for (index, capability) in document.capability.iter().enumerate() {
            validate_record(capability)?;
            validate_projection(capability, projected)?;
            if !ids.insert(capability.id.as_str()) {
                return Err(format!("duplicate capability id '{}'", capability.id));
            }
            lookup.insert(capability.id.clone(), index);
        }
        for (alias, target) in document.aliases {
            if alias.trim().is_empty() || !ids.contains(target.as_str()) {
                return Err(format!("invalid capability alias '{alias}' -> '{target}'"));
            }
            let index = lookup[&target];
            if lookup.insert(alias.clone(), index).is_some() {
                return Err(format!("duplicate capability lookup key '{alias}'"));
            }
        }
        Ok(Self {
            entries: document.capability,
            lookup,
            projected,
        })
    }

    /// True for the public export's registry, whose non-public records
    /// withhold their strategic classification.
    #[must_use]
    pub const fn is_public_projection(&self) -> bool {
        self.projected
    }

    #[must_use]
    pub fn entries(&self) -> &[ProductCapability] {
        &self.entries
    }

    #[must_use]
    pub fn find(&self, id_or_key: &str) -> Option<&ProductCapability> {
        self.lookup
            .get(id_or_key)
            .and_then(|index| self.entries.get(*index))
    }

    #[must_use]
    pub fn allows(&self, plan: Plan, id_or_key: &str) -> bool {
        self.find(id_or_key).is_some_and(|capability| {
            // A legacy plan can never grant separate SDK/OEM rights, and no
            // plan grants a retired capability.
            !matches!(
                capability.price_class,
                UserPriceClass::SdkOemCommercial | UserPriceClass::Retired
            ) && plan.rank() >= capability.minimum_plan().rank()
        })
    }

    #[must_use]
    pub fn render_markdown(&self) -> String {
        use std::fmt::Write as _;

        let mut entries: Vec<&ProductCapability> = self.entries.iter().collect();
        entries.sort_unstable_by_key(|capability| capability.id.as_str());
        let mut output = String::from(
            "<!-- Generated from product/capabilities.toml; do not edit by hand. -->\n\n\
             # Product capabilities\n\n\
             | ID | Name | Price | Hive plan | Source visibility | Delivery | Aha | Moat | License | Self-host | Managed cloud | Enterprise entitlement | Account | Commercial use | Redistribution/OEM | Data boundary | Telemetry | Maturity | Source path |\n\
             |---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
        );
        for capability in entries {
            writeln!(
                output,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                capability.id,
                capability.name.replace('|', "\\|"),
                enum_name(&capability.price_class),
                capability
                    .hive_plan
                    .as_ref()
                    .map_or_else(|| "-".to_string(), enum_name),
                enum_name(&capability.source_visibility),
                enum_name(&capability.runtime_delivery),
                enum_name(&capability.aha_criticality),
                enum_name(&capability.moat_sensitivity),
                enum_name(&capability.license_class),
                capability.self_host_available,
                capability.managed_cloud_available,
                capability.enterprise_entitlement,
                capability.account_required,
                enum_name(&capability.allowed_commercial_use),
                enum_name(&capability.redistribution_oem_rights),
                capability.data_boundary.replace('|', "\\|"),
                enum_name(&capability.telemetry_event_class),
                enum_name(&capability.public_maturity),
                capability.source_path.replace('|', "\\|"),
            )
            .expect("writing to String cannot fail");
        }
        output
    }
}

fn enum_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .expect("serializable registry enum")
        .as_str()
        .expect("registry enum serializes as string")
        .to_string()
}

fn validate_record(capability: &ProductCapability) -> Result<(), String> {
    for (field, value) in [
        ("id", capability.id.as_str()),
        ("name", capability.name.as_str()),
        (
            "enterprise_entitlement",
            capability.enterprise_entitlement.as_str(),
        ),
        ("source_path", capability.source_path.as_str()),
        ("data_boundary", capability.data_boundary.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(format!("capability '{}' has empty {field}", capability.id));
        }
    }
    let invalid = if matches!(
        capability.aha_criticality,
        AhaCriticality::IndividualAha | AhaCriticality::TeamAha
    ) && capability.price_class != UserPriceClass::Free
    {
        Some("Aha-critical capability has no free delivery path")
    } else if capability.moat_sensitivity == MoatSensitivity::CrownJewel
        && capability.source_visibility != SourceVisibility::PrivateSource
    {
        Some("crown-jewel implementation must not enter public source")
    } else if capability.moat_sensitivity == MoatSensitivity::CrownJewel
        && capability.redistribution_oem_rights != RedistributionRights::SeparateAgreementRequired
    {
        Some("crown-jewel redistribution requires a separate agreement")
    } else if capability.source_visibility == SourceVisibility::PublicApache
        && (capability.license_class != LicenseClass::ApacheTrustCore
            || capability.allowed_commercial_use != CommercialUse::ApacheRights
            || capability.redistribution_oem_rights != RedistributionRights::ApacheRights
            || capability.price_class != UserPriceClass::Free
            || capability.account_required)
    {
        Some("public Apache reference rights must remain free and accountless")
    } else if capability.license_class == LicenseClass::ApacheTrustCore
        && capability.source_visibility != SourceVisibility::PublicApache
    {
        Some("Apache source classification mismatch")
    } else if capability.source_visibility != SourceVisibility::PublicApache
        && (capability.allowed_commercial_use == CommercialUse::ApacheRights
            || capability.redistribution_oem_rights == RedistributionRights::ApacheRights)
    {
        Some("non-Apache implementations cannot grant Apache redistribution rights")
    } else if capability.price_class == UserPriceClass::SdkOemCommercial
        && (capability.license_class != LicenseClass::SdkOem
            || capability.allowed_commercial_use != CommercialUse::SdkOemAgreement)
    {
        Some("SDK/OEM capability requires a separate commercial license")
    } else if capability.runtime_delivery == RuntimeDelivery::SourceBuild
        && (!capability.self_host_available
            || capability.source_visibility == SourceVisibility::PrivateSource)
    {
        Some("source builds require available source and a self-host path")
    } else if capability.runtime_delivery == RuntimeDelivery::ManagedCloudService
        && !capability.managed_cloud_available
    {
        Some("managed service requires managed-cloud availability")
    } else if matches!(
        capability.runtime_delivery,
        RuntimeDelivery::SignedBinary
            | RuntimeDelivery::LocalService
            | RuntimeDelivery::EnterprisePrivateImage
    ) && !capability.self_host_available
    {
        Some("local artifact requires a self-host path")
    } else if capability.aha_criticality == AhaCriticality::IndividualAha
        && (capability.account_required || !capability.self_host_available)
    {
        Some("individual Aha must work locally without an account")
    } else if capability.source_visibility == SourceVisibility::PublicEnterpriseSourceAvailable
        && capability.license_class != LicenseClass::CommercialSource
    {
        Some("source-available Enterprise requires its own license")
    } else if capability.license_class == LicenseClass::FreeRuntime
        && (capability.source_visibility != SourceVisibility::PrivateSource
            || capability.price_class != UserPriceClass::Free
            || capability.allowed_commercial_use != CommercialUse::FreeSmallTeamInternal
            || capability.redistribution_oem_rights
                != RedistributionRights::SeparateAgreementRequired)
    {
        Some("Free Runtime rights must not grant source/OEM/redistribution rights")
    } else if capability.is_enterprise() != (capability.enterprise_entitlement == capability.id) {
        Some("Enterprise capability requires its explicit canonical entitlement")
    } else if !capability.is_enterprise() && capability.enterprise_entitlement != "none" {
        Some("non-Enterprise capability must not require governance entitlement")
    } else if !capability.self_host_available && !capability.managed_cloud_available {
        Some("capability has no delivery path")
    } else if (capability.price_class == UserPriceClass::Hive) != capability.hive_plan.is_some() {
        Some("Hive plan is required exactly on hive capabilities")
    } else if capability.price_class == UserPriceClass::Hive
        && (capability.source_visibility != SourceVisibility::PrivateSource
            || !capability.account_required
            || !matches!(
                capability.runtime_delivery,
                RuntimeDelivery::HiveService | RuntimeDelivery::SignedBinary
            ))
    {
        Some("Hive capability needs private source, an account and the Hive or signed path")
    } else if capability.runtime_delivery == RuntimeDelivery::HiveService
        && (capability.price_class != UserPriceClass::Hive || !capability.managed_cloud_available)
    {
        Some("Hive service delivery requires a Hive plan and the managed service")
    } else if matches!(
        capability.price_class,
        UserPriceClass::CloudPaid
            | UserPriceClass::ProPaid
            | UserPriceClass::TeamPaid
            | UserPriceClass::EnterprisePaid
    ) {
        Some("legacy price class is not valid in schema 3; use free, hive or retired")
    } else {
        None
    };
    if let Some(reason) = invalid {
        return Err(format!("capability '{}': {reason}", capability.id));
    }
    Ok(())
}

/// Fields the public projection withholds for capabilities whose
/// implementation is not public: they describe product strategy, not rights.
/// Access decisions use only price, source and rights, which stay intact.
const WITHHELD_FIELDS: [(&str, &str); 4] = [
    ("aha_criticality", "undisclosed"),
    ("moat_sensitivity", "undisclosed"),
    ("public_maturity", "private"),
    ("source_path", UNDISCLOSED_SOURCE),
];

/// The canonical registry discloses everything; the public projection
/// withholds all strategic fields of every non-public record and nothing of
/// a public one. Anything in between fails closed.
fn validate_projection(capability: &ProductCapability, projected: bool) -> Result<(), String> {
    let withheld = [
        capability.aha_criticality == AhaCriticality::Undisclosed,
        capability.moat_sensitivity == MoatSensitivity::Undisclosed,
        capability.public_maturity == PublicStatus::Private,
        capability.source_path == UNDISCLOSED_SOURCE,
    ];
    let undisclosed = withheld[0] || withheld[1] || withheld[3];
    let public = capability.source_visibility == SourceVisibility::PublicApache;
    let problem = if !projected && undisclosed {
        Some("undisclosed classification outside the public projection")
    } else if projected && public && undisclosed {
        Some("public capability must stay fully classified")
    } else if projected && !public && !withheld.iter().all(|field| *field) {
        Some("public projection discloses a strategic classification")
    } else {
        None
    };
    match problem {
        Some(reason) => Err(format!("capability '{}': {reason}", capability.id)),
        None => Ok(()),
    }
}

/// Render the public projection of a canonical registry source: the
/// `projection` marker plus the withheld strategy fields (Aha criticality,
/// moat sensitivity, maturity, source path) rewritten for every
/// non-public record. Layout and all other bytes are preserved; the result
/// is re-parsed, so a rewrite that misses a field fails closed. Projecting a
/// projection returns it unchanged.
pub fn public_projection(source: &str) -> Result<String, String> {
    use std::fmt::Write as _;

    let registry = ProductCapabilityRegistry::parse(source)?;
    if registry.is_public_projection() {
        return Ok(source.to_string());
    }
    let hidden: BTreeSet<&str> = registry
        .entries()
        .iter()
        .filter(|capability| capability.source_visibility != SourceVisibility::PublicApache)
        .map(|capability| capability.id.as_str())
        .collect();

    let mut output = String::with_capacity(source.len());
    let mut current_id: Option<String> = None;
    for line in source.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        let newline = &line[content.len()..];
        if content.trim() == "[[capability]]" {
            current_id = None;
        }
        let Some((key, value)) = content.split_once(" = ") else {
            output.push_str(line);
            continue;
        };
        if key == "id" {
            current_id = Some(value.trim_matches('"').to_string());
        }
        let withheld = current_id
            .as_deref()
            .filter(|id| hidden.contains(id))
            .and_then(|_| WITHHELD_FIELDS.iter().find(|(field, _)| *field == key));
        match withheld {
            Some((field, replacement)) => write!(output, "{field} = \"{replacement}\"{newline}")
                .expect("writing to String cannot fail"),
            None => output.push_str(line),
        }
        if key == "schema_version" {
            write!(output, "projection = \"{PUBLIC_PROJECTION}\"{newline}")
                .expect("writing to String cannot fail");
        }
    }

    let projected = ProductCapabilityRegistry::parse(&output)?;
    if !projected.is_public_projection() {
        return Err("capability registry has no schema_version line to mark".to_string());
    }
    Ok(output)
}

static REGISTRY: OnceLock<ProductCapabilityRegistry> = OnceLock::new();

#[must_use]
pub fn registry() -> &'static ProductCapabilityRegistry {
    REGISTRY.get_or_init(|| {
        ProductCapabilityRegistry::parse(REGISTRY_SOURCE)
            .unwrap_or_else(|error| panic!("embedded product capability registry failed: {error}"))
    })
}

/// Stable product record for each OCLA capability kind. This exhaustive match
/// makes a newly added OCLA kind fail compilation until it is classified.
#[must_use]
pub const fn ocla_product_capability_id(kind: OclaCapabilityKind) -> &'static str {
    match kind {
        OclaCapabilityKind::ObservationHook => "trust.ocla.observation_hook",
        OclaCapabilityKind::UsageSink => "enterprise.ocla.usage_sink",
        OclaCapabilityKind::MetricsExporter => "team.ocla.metrics_exporter",
        OclaCapabilityKind::SavingsLedger => "trust.ocla.savings_ledger",
        OclaCapabilityKind::IntentClassifier => "pro.ocla.intent_classifier",
        OclaCapabilityKind::OutcomeTracker => "pro.ocla.outcome_tracker",
        OclaCapabilityKind::CompressionProvider => "trust.ocla.compression_provider",
        OclaCapabilityKind::ResponseOptimizer => "pro.ocla.response_optimizer",
        OclaCapabilityKind::EfficiencyAnalyzer => "pro.ocla.efficiency_analyzer",
        OclaCapabilityKind::ConfigTuner => "pro.ocla.config_tuner",
        OclaCapabilityKind::ExperimentRunner => "pro.ocla.experiment_runner",
        OclaCapabilityKind::ConnectorScheduler => "pro.ocla.connector_scheduler",
        OclaCapabilityKind::AgentGateway => "pro.ocla.agent_gateway",
        OclaCapabilityKind::DeliveryRegistry => "pro.ocla.delivery_registry",
    }
}

pub const EXPERIMENT_EXECUTOR_CAPABILITY_ID: &str = "pro.ocla.experiment_executor";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_registry_is_valid() {
        assert!(registry().entries().len() >= 40);
        let source = include_str!("../../../product/capabilities.toml");
        if REGISTRY_SOURCE == source {
            return;
        }
        // A checkout with the canonical private registry embeds its public
        // projection (scripts/project-capabilities.py; byte equality is
        // checked by scripts/check-capabilities.py): the same capabilities in
        // the same order, with no private source path.
        assert!(
            REGISTRY_SOURCE.contains("\nprojection = \"public\"\n"),
            "packaged capability registry drifted from repository source"
        );
        let ids = |text: &str| -> Vec<String> {
            ProductCapabilityRegistry::parse(text)
                .expect("registry parses")
                .entries()
                .iter()
                .map(|entry| entry.id.clone())
                .collect()
        };
        assert_eq!(ids(REGISTRY_SOURCE), ids(source));
        assert!(!REGISTRY_SOURCE.contains("\"ee/"));
    }

    #[test]
    fn malformed_or_duplicate_records_are_rejected() {
        assert!(ProductCapabilityRegistry::parse("schema_version = 99").is_err());
        let first = &registry().entries()[0].id;
        let second = &registry().entries()[1].id;
        let duplicate = REGISTRY_SOURCE.replacen(
            &format!("id = \"{second}\""),
            &format!("id = \"{first}\""),
            1,
        );
        assert!(ProductCapabilityRegistry::parse(&duplicate).is_err());
    }

    #[test]
    fn unknown_capabilities_fail_closed() {
        for plan in Plan::all() {
            assert!(!registry().allows(*plan, "unclassified.future.feature"));
        }
    }

    fn mutated_first(field: &str, value: toml::Value) -> String {
        let mut document: toml::Value = toml::from_str(REGISTRY_SOURCE).unwrap();
        document["capability"][0][field] = value;
        toml::to_string(&document).unwrap()
    }

    #[test]
    fn all_eighteen_classifications_are_required_without_defaults() {
        let document: toml::Value = toml::from_str(REGISTRY_SOURCE).unwrap();
        let records = document["capability"].as_array().unwrap();
        for record in records {
            let hive = record["price_class"].as_str() == Some("hive");
            assert_eq!(
                record.as_table().unwrap().len(),
                18 + usize::from(hive),
                "{record}"
            );
            for field in record.as_table().unwrap().keys() {
                let mut incomplete = record.clone();
                incomplete.as_table_mut().unwrap().remove(field);
                if field == "hive_plan" {
                    // Optional in the record format, required by validation.
                    let parsed: ProductCapability = incomplete.try_into().unwrap();
                    assert!(validate_record(&parsed).is_err(), "missing {field}");
                    continue;
                }
                assert!(
                    incomplete.try_into::<ProductCapability>().is_err(),
                    "missing {field}"
                );
            }
        }
    }

    /// The registry is the open-core boundary: every public capability points at
    /// source that exists in this repository, and no private or paid capability
    /// points into it. Moving paid code into the public tree fails here.
    #[test]
    fn registry_source_paths_enforce_the_open_core_boundary() {
        let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        for entry in registry().entries() {
            for source in entry.source_path.split(';').map(str::trim) {
                let in_repository = source.starts_with("rust/");
                if entry.source_visibility == SourceVisibility::PublicApache {
                    assert!(in_repository, "{}: public source outside rust/", entry.id);
                    assert!(
                        repository.join(source).exists(),
                        "{}: public source {source} is missing",
                        entry.id
                    );
                } else {
                    assert!(
                        !in_repository,
                        "{}: private capability points into the public tree",
                        entry.id
                    );
                }
                if entry.price_class != UserPriceClass::Free {
                    assert_ne!(entry.source_visibility, SourceVisibility::PublicApache);
                }
            }
        }
    }

    #[test]
    fn shared_team_capabilities_need_team_and_enterprise_self_hosts_them() {
        let registry = registry();
        let entry = registry.find("team.context.shared").unwrap();
        assert_eq!(entry.price_class, UserPriceClass::Hive);
        assert_eq!(entry.hive_plan, Some(HivePlan::Team));
        assert!(entry.managed_cloud_available && entry.self_host_available);
        assert!(!registry.allows(Plan::Pro, &entry.id));
        assert!(registry.allows(Plan::Team, &entry.id));
        assert!(registry.allows(Plan::Enterprise, &entry.id));
        let mut public = entry.clone();
        public.source_visibility = SourceVisibility::PublicApache;
        assert!(validate_record(&public).is_err());
    }

    #[test]
    fn team_runtime_capabilities_ship_in_the_signed_self_hosted_runtime() {
        let registry = registry();
        for id in [
            "team.runtime.artifact_reader",
            "team.runtime.artifact_author",
        ] {
            let entry = registry.find(id).unwrap();
            assert_eq!(entry.price_class, UserPriceClass::Hive);
            assert_eq!(entry.hive_plan, Some(HivePlan::Team));
            assert_eq!(entry.runtime_delivery, RuntimeDelivery::SignedBinary);
            assert!(entry.self_host_available && !entry.managed_cloud_available);
            assert!(!registry.allows(Plan::Pro, id));
            assert!(registry.allows(Plan::Team, id));
            // Each condition of the Hive rule refuses on its own.
            let breaks: [fn(&mut ProductCapability); 3] = [
                |record| {
                    record.source_visibility = SourceVisibility::PublicEnterpriseSourceAvailable;
                },
                |record| record.account_required = false,
                |record| record.runtime_delivery = RuntimeDelivery::LocalService,
            ];
            for change in breaks {
                let mut broken = entry.clone();
                change(&mut broken);
                let refusal = validate_record(&broken).unwrap_err();
                assert!(refusal.contains("Hive capability"), "{refusal}");
            }
        }
    }

    #[test]
    fn hosted_seat_authority_is_classified_for_team_and_enterprise_only() {
        let registry = registry();
        let entry = registry.find("team.seat_management").unwrap();
        assert_eq!(entry.price_class, UserPriceClass::Hive);
        assert_eq!(entry.hive_plan, Some(HivePlan::Team));
        assert_eq!(entry.runtime_delivery, RuntimeDelivery::HiveService);
        assert!(entry.account_required && entry.managed_cloud_available);
        assert_eq!(entry.enterprise_entitlement, "none");
        assert!(!registry.allows(Plan::Community, &entry.id));
        assert!(!registry.allows(Plan::Pro, &entry.id));
        assert!(registry.allows(Plan::Team, &entry.id));
        assert!(registry.allows(Plan::Enterprise, &entry.id));
    }

    #[test]
    fn aha_paywalls_public_crown_jewels_and_unclassified_telemetry_fail_closed() {
        for (field, value) in [
            ("price_class", "cloud_paid"),
            ("moat_sensitivity", "crown_jewel"),
            ("license_class", "free_runtime"),
            ("source_visibility", "private_source"),
            ("telemetry_event_class", "arbitrary_runtime_text"),
            ("enterprise_entitlement", "enterprise.unrelated"),
        ] {
            let malformed = mutated_first(field, value.into());
            assert!(
                ProductCapabilityRegistry::parse(&malformed).is_err(),
                "accepted {field}={value}"
            );
        }
        assert!(
            ProductCapabilityRegistry::parse(&mutated_first("account_required", true.into()))
                .is_err()
        );
    }

    #[test]
    fn free_private_intelligence_does_not_grant_oem_or_source_rights() {
        // Exercise the free/private rights class without advertising a retired
        // capability or changing the commercial classification of a live one.
        let mut entry = registry()
            .find("pro.runtime.adaptive_context")
            .unwrap()
            .clone();
        entry.price_class = UserPriceClass::Free;
        entry.license_class = LicenseClass::FreeRuntime;
        entry.allowed_commercial_use = CommercialUse::FreeSmallTeamInternal;
        entry.aha_criticality = AhaCriticality::IndividualAha;
        entry.account_required = false;
        assert!(validate_record(&entry).is_ok());
        assert_eq!(entry.minimum_plan(), Plan::Community);
        let mut leaking = entry.clone();
        leaking.redistribution_oem_rights = RedistributionRights::ApacheRights;
        assert!(validate_record(&leaking).is_err());
        leaking = entry.clone();
        leaking.allowed_commercial_use = CommercialUse::ApacheRights;
        assert!(validate_record(&leaking).is_err());
        // The public projection withholds moat and Aha classifications; the
        // crown-jewel redistribution and individual-Aha account rules can only
        // be exercised where they are disclosed (the canonical registry).
        let disclosed = !registry().is_public_projection();
        if disclosed {
            leaking = entry.clone();
            leaking.license_class = LicenseClass::ThirdParty;
            leaking.redistribution_oem_rights = RedistributionRights::ThirdPartyTerms;
            assert!(validate_record(&leaking).is_err());
        }
        for license in [
            LicenseClass::PrivateService,
            LicenseClass::CommercialSource,
            LicenseClass::SdkOem,
        ] {
            leaking = entry.clone();
            leaking.license_class = license;
            leaking.redistribution_oem_rights = RedistributionRights::ApacheRights;
            assert!(validate_record(&leaking).is_err());
        }
        leaking = entry.clone();
        leaking.runtime_delivery = RuntimeDelivery::SourceBuild;
        assert!(validate_record(&leaking).is_err());
        if disclosed {
            leaking = entry.clone();
            leaking.account_required = true;
            assert!(validate_record(&leaking).is_err());
        }
    }

    #[test]
    fn legacy_ids_do_not_paywall_local_or_small_team_aha() {
        for entry in registry().entries() {
            if matches!(
                entry.aha_criticality,
                AhaCriticality::IndividualAha | AhaCriticality::TeamAha
            ) {
                assert!(
                    registry().allows(Plan::Community, &entry.id),
                    "{}",
                    entry.id
                );
            }
        }
        for id in [
            "enterprise.execution_policy",
            "enterprise.sso_scim",
            "team.sso_oidc",
        ] {
            assert!(!registry().allows(Plan::Community, id));
            assert!(!registry().allows(Plan::Team, id));
        }
    }

    /// D7: the public export withholds strategy (moat, Aha, maturity, private
    /// source locations) without changing a single access decision.
    #[test]
    fn public_projection_withholds_strategy_but_keeps_every_access_decision() {
        let canonical = ProductCapabilityRegistry::parse(REGISTRY_SOURCE).unwrap();
        if canonical.is_public_projection() {
            // Public export: parse() already enforced the projection; the
            // canonical comparison runs where the canonical registry lives.
            assert!(!REGISTRY_SOURCE.contains("crown_jewel"));
            return;
        }
        let source = public_projection(REGISTRY_SOURCE).unwrap();
        let projected = ProductCapabilityRegistry::parse(&source).unwrap();
        assert!(projected.is_public_projection());
        assert_eq!(public_projection(&source).unwrap(), source);
        assert!(!source.contains("crown_jewel"));
        assert_eq!(projected.entries().len(), canonical.entries().len());
        for (before, after) in canonical.entries().iter().zip(projected.entries()) {
            if before.source_visibility == SourceVisibility::PublicApache {
                assert_eq!(before, after);
            } else {
                assert_eq!(after.moat_sensitivity, MoatSensitivity::Undisclosed);
                assert_eq!(after.aha_criticality, AhaCriticality::Undisclosed);
                assert_eq!(after.public_maturity, PublicStatus::Private);
                assert_eq!(after.source_path, UNDISCLOSED_SOURCE);
                let mut restored = after.clone();
                restored.aha_criticality = before.aha_criticality;
                restored.moat_sensitivity = before.moat_sensitivity;
                restored.public_maturity = before.public_maturity;
                restored.source_path.clone_from(&before.source_path);
                assert_eq!(
                    before, &restored,
                    "projection changed rights of {}",
                    before.id
                );
            }
            for plan in Plan::all() {
                assert_eq!(
                    canonical.allows(*plan, &before.id),
                    projected.allows(*plan, &before.id)
                );
            }
        }

        let hidden = canonical
            .entries()
            .iter()
            .find(|entry| entry.moat_sensitivity == MoatSensitivity::CrownJewel)
            .unwrap();
        let block = format!("id = \"{}\"", hidden.id);
        let (head, tail) = source.split_once(&block).unwrap();
        let partial = format!(
            "{head}{block}{}",
            tail.replacen(
                "moat_sensitivity = \"undisclosed\"",
                "moat_sensitivity = \"crown_jewel\"",
                1
            )
        );
        for (registry, case) in [
            (
                mutated_first("moat_sensitivity", "undisclosed".into()),
                "canonical",
            ),
            (partial, "partial projection"),
            (
                source.replacen("projection = \"public\"", "projection = \"other\"", 1),
                "unknown",
            ),
            (
                source.replacen(
                    &format!("source_path = \"{}\"", registry().entries()[0].source_path),
                    &format!("source_path = \"{UNDISCLOSED_SOURCE}\""),
                    1,
                ),
                "public record withheld",
            ),
        ] {
            assert!(
                ProductCapabilityRegistry::parse(&registry).is_err(),
                "accepted {case}"
            );
        }
    }

    /// A3 (Hive): local cockpit views are free, filter packs and team or
    /// governance rights are Hive plans, Personal Cloud is retired, and no
    /// schema-3 record keeps a legacy price class.
    #[test]
    fn hive_classification_keeps_local_free_and_retires_personal_cloud() {
        let registry = registry();
        for entry in registry.entries() {
            assert!(
                matches!(
                    entry.price_class,
                    UserPriceClass::Free
                        | UserPriceClass::Hive
                        | UserPriceClass::Retired
                        | UserPriceClass::SdkOemCommercial
                ),
                "{} keeps a legacy price class",
                entry.id
            );
            if entry.price_class == UserPriceClass::Free {
                assert!(!entry.account_required, "{}", entry.id);
            }
        }
        for id in [
            "pro.dashboard.knowledge_workflows",
            "pro.runtime.adaptive_context",
            "pro.runtime.memory_curation",
            "pro.runtime.context_policy",
        ] {
            let entry = registry.find(id).unwrap();
            assert_eq!(entry.price_class, UserPriceClass::Free, "{id}");
            assert!(registry.allows(Plan::Community, id), "{id}");
        }
        for (id, plan) in [
            ("pro.runtime.semantic_detectors", HivePlan::Personal),
            ("pro.runtime.code_security", HivePlan::Personal),
            ("pro.runtime.personal_protection", HivePlan::Personal),
            ("team.private_registry", HivePlan::Team),
            ("team.managed_connectors", HivePlan::Team),
            ("team.sso_oidc", HivePlan::Enterprise),
            ("team.audit_retention", HivePlan::Enterprise),
            (
                "enterprise.runtime.org_policy_verifier",
                HivePlan::Enterprise,
            ),
        ] {
            let entry = registry.find(id).unwrap();
            assert_eq!(entry.price_class, UserPriceClass::Hive, "{id}");
            assert_eq!(entry.hive_plan, Some(plan), "{id}");
            assert_eq!(entry.minimum_plan(), plan.plan(), "{id}");
            assert!(!registry.allows(Plan::Community, id), "{id}");
            assert!(registry.allows(Plan::Enterprise, id), "{id}");
        }
        for id in ["pro.cloud_sync", "pro.hosted_index", "team.revenue_share"] {
            let entry = registry.find(id).unwrap();
            assert_eq!(entry.price_class, UserPriceClass::Retired, "{id}");
            assert_ne!(entry.minimum_plan(), Plan::Community, "{id}");
            for plan in Plan::all() {
                assert!(!registry.allows(*plan, id), "{id} granted to {plan:?}");
            }
        }
        // Stable lookup names survive the reclassification.
        assert_eq!(registry.find("cloud_sync").unwrap().id, "pro.cloud_sync");
        assert_eq!(registry.find("sso_oidc").unwrap().id, "team.sso_oidc");

        let hive = registry.find("team.context.shared").unwrap().clone();
        for legacy in [
            UserPriceClass::CloudPaid,
            UserPriceClass::ProPaid,
            UserPriceClass::TeamPaid,
        ] {
            let mut broken = hive.clone();
            broken.price_class = legacy;
            broken.hive_plan = None;
            broken.runtime_delivery = RuntimeDelivery::ManagedCloudService;
            assert!(validate_record(&broken).is_err(), "{legacy:?} accepted");
        }
        let mut planless = hive.clone();
        planless.hive_plan = None;
        assert!(validate_record(&planless).is_err());
        let mut stray = registry.find("free.cockpit.ui").unwrap().clone();
        stray.hive_plan = Some(HivePlan::Personal);
        assert!(validate_record(&stray).is_err());
        let mut local = hive;
        local.managed_cloud_available = false;
        assert!(validate_record(&local).is_err());
    }

    #[test]
    fn legacy_aliases_cannot_shadow_ids_or_point_to_unclassified_capabilities() {
        for target in ["missing.capability", "compression"] {
            let mut document: toml::Value = toml::from_str(REGISTRY_SOURCE).unwrap();
            document["aliases"]
                .as_table_mut()
                .unwrap()
                .insert("bad_alias".into(), target.into());
            assert!(
                ProductCapabilityRegistry::parse(&toml::to_string(&document).unwrap()).is_err()
            );
        }
        let mut document: toml::Value = toml::from_str(REGISTRY_SOURCE).unwrap();
        document["aliases"].as_table_mut().unwrap().insert(
            "trust.runtime.compression".into(),
            "trust.runtime.caching".into(),
        );
        assert!(ProductCapabilityRegistry::parse(&toml::to_string(&document).unwrap()).is_err());
    }
}
