// SPDX-License-Identifier: Apache-2.0

//! Canonical v4 product-capability registry.
//!
//! Product access decisions are made from the embedded, validated registry.
//! Price, source visibility and commercial rights are independent axes.
//! Legacy IDs remain stable, but never determine price or source visibility.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::core::billing::Plan;
use crate::core::ocla::OclaCapabilityKind;

/// Package-local mirror of the repository source of truth. CI requires this
/// byte-for-byte copy so source-package builds remain hermetic.
pub const REGISTRY_SOURCE: &str = include_str!("../../data/product-capabilities.toml");

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserPriceClass {
    Free,
    CloudPaid,
    /// Pro subscription feature delivered only inside the signed, private
    /// Intelligence Runtime; the runtime's own license gates its execution.
    ProPaid,
    /// Shared team capability: Team subscription on the managed cloud; the
    /// self-hosted delivery is the Enterprise private image (Enterprise ⊇ Team).
    TeamPaid,
    EnterprisePaid,
    SdkOemCommercial,
}

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
            UserPriceClass::CloudPaid | UserPriceClass::ProPaid => Plan::Pro,
            UserPriceClass::TeamPaid => Plan::Team,
            UserPriceClass::EnterprisePaid | UserPriceClass::SdkOemCommercial => Plan::Enterprise,
        }
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
        if document.schema_version != 2 {
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
            // A legacy plan can never grant separate SDK/OEM rights.
            capability.price_class != UserPriceClass::SdkOemCommercial
                && plan.rank() >= capability.minimum_plan().rank()
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
             | ID | Name | Price | Source visibility | Delivery | Aha | Moat | License | Self-host | Managed cloud | Enterprise entitlement | Account | Commercial use | Redistribution/OEM | Data boundary | Telemetry | Maturity | Source path |\n\
             |---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n",
        );
        for capability in entries {
            writeln!(
                output,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                capability.id,
                capability.name.replace('|', "\\|"),
                enum_name(&capability.price_class),
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
    } else if (capability.price_class == UserPriceClass::EnterprisePaid)
        != (capability.enterprise_entitlement == capability.id)
    {
        Some("Enterprise capability requires its explicit canonical entitlement")
    } else if capability.price_class != UserPriceClass::EnterprisePaid
        && capability.enterprise_entitlement != "none"
    {
        Some("non-Enterprise capability must not require governance entitlement")
    } else if !capability.self_host_available && !capability.managed_cloud_available {
        Some("capability has no delivery path")
    } else if capability.price_class == UserPriceClass::TeamPaid
        && (capability.source_visibility != SourceVisibility::PrivateSource
            || !capability.managed_cloud_available
            || !capability.account_required)
    {
        Some("Team capability needs private source and the managed Team cloud path")
    } else if capability.price_class == UserPriceClass::ProPaid
        && (capability.source_visibility != SourceVisibility::PrivateSource
            || capability.runtime_delivery != RuntimeDelivery::SignedBinary)
    {
        Some("Pro runtime capability ships only in the signed private runtime")
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
        assert_eq!(
            REGISTRY_SOURCE.as_bytes(),
            include_bytes!("../../../product/capabilities.toml"),
            "packaged capability registry drifted from repository source"
        );
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
            assert_eq!(record.as_table().unwrap().len(), 18);
            for field in record.as_table().unwrap().keys() {
                let mut incomplete = record.clone();
                incomplete.as_table_mut().unwrap().remove(field);
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
        assert_eq!(entry.price_class, UserPriceClass::TeamPaid);
        assert!(entry.managed_cloud_available && entry.self_host_available);
        assert!(!registry.allows(Plan::Pro, &entry.id));
        assert!(registry.allows(Plan::Team, &entry.id));
        assert!(registry.allows(Plan::Enterprise, &entry.id));
        let mut public = entry.clone();
        public.source_visibility = SourceVisibility::PublicApache;
        assert!(validate_record(&public).is_err());
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
        let entry = registry().find("pro.runtime.adaptive_routing").unwrap();
        assert_eq!(entry.price_class, UserPriceClass::Free);
        assert_eq!(entry.source_visibility, SourceVisibility::PrivateSource);
        assert_eq!(entry.license_class, LicenseClass::FreeRuntime);
        assert!(registry().allows(Plan::Community, &entry.id));
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
