// SPDX-License-Identifier: Apache-2.0
use std::io::Read;
use std::path::{Path, PathBuf};

use lean_ctx_protocol::{
    EngineContextPlanRequestV1, EngineContextSourceMaterializationRequestV1,
    EngineContextSourcePlanRequestV1, EngineInvocationV1, EngineObservationV1,
    MAX_ENGINE_CONTEXT_PLAN_REQUEST_BYTES, MAX_ENGINE_SOURCE_PLAN_REQUEST_BYTES, ProtocolReference,
    SemanticVersion, Sha256Digest,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::core::engine_interface::{
    ENGINE_INTERFACE_VERSION, ENGINE_TRANSPORT_VERSION, EngineTransportError,
    EngineTransportRecoveryDescriptor, EngineTransportResult, EngineTransportView,
    execute_transport_context_view, recover_transport_source,
};

mod host;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineOperation {
    ContextPlan,
    ContextPlanSources,
    ContextMaterializeSources,
    ContextView,
    EgressAdmit,
    Recover,
    ContextLineage,
    ContextPolicyEvidence,
}

#[derive(Debug)]
enum EngineCliError {
    Usage,
    Request,
    JsonFile,
    UnsupportedSchema,
    UnsupportedTransport,
    UnsupportedInterface,
    Engine(EngineTransportError),
    Host(&'static str),
}

impl EngineCliError {
    fn code(&self) -> &'static str {
        match self {
            Self::Usage | Self::Request => "invalid_request",
            Self::JsonFile => "request_file_unavailable",
            Self::UnsupportedSchema => "unsupported_schema_version",
            Self::UnsupportedTransport => "unsupported_transport_version",
            Self::UnsupportedInterface => "unsupported_engine_interface_version",
            Self::Engine(error) => error.code(),
            Self::Host(code) => code,
        }
    }
}

#[derive(Debug)]
struct EngineCliArgs {
    operation: EngineOperation,
    project_root: PathBuf,
    json_file: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextViewRequestV1 {
    schema_version: u32,
    transport_version: u32,
    engine_interface_version: SemanticVersion,
    path: String,
    mode: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoverRequestV1 {
    schema_version: u32,
    transport_version: u32,
    engine_interface_version: SemanticVersion,
    path: String,
    recovery_ref: ProtocolReference,
    source_ref: ProtocolReference,
    source_digest: Sha256Digest,
}

/// A read of the Context Store for one tenant/project scope. The project ID
/// defaults to the project root, as `lean-ctx autopilot` names it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextStoreRequestV1 {
    schema_version: u32,
    transport_version: u32,
    engine_interface_version: SemanticVersion,
    #[serde(default)]
    project_id: Option<lean_ctx_protocol::ProjectId>,
    #[serde(default)]
    tenant_id: Option<lean_ctx_protocol::TenantId>,
    /// Required for lineage, refused for evidence.
    #[serde(default)]
    task_id: Option<lean_ctx_protocol::TaskId>,
}

#[derive(Debug, Serialize)]
struct ContextStoreResponseV1<T: Serialize> {
    schema_version: u32,
    transport_version: u32,
    engine_interface_version: SemanticVersion,
    #[serde(flatten)]
    body: T,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct EngineWireViewV1 {
    text: String,
    output_ref: Option<ProtocolReference>,
    output_digest: Option<Sha256Digest>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct EngineWireRecoveryV1 {
    recovery_ref: ProtocolReference,
    source_ref: ProtocolReference,
    source_digest: Sha256Digest,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct EngineWireResponseV1 {
    schema_version: u32,
    transport_version: u32,
    engine_interface_version: SemanticVersion,
    view: EngineWireViewV1,
    invocation: Option<EngineInvocationV1>,
    observation: Option<EngineObservationV1>,
    recovery: EngineWireRecoveryV1,
}

pub(crate) fn cmd_engine(args: &[String]) {
    if args.first().map(String::as_str) == Some("runtime") {
        match crate::core::intelligence_runtime::run(&args[1..]) {
            Ok(output) => println!("{output}"),
            Err(error) => {
                eprintln!("engine runtime: {error}");
                std::process::exit(2);
            }
        }
        return;
    }
    if args.first().map(String::as_str) == Some("tool-session") {
        super::agent_tools_cmd::cmd_agent_tools(&args[1..]);
        return;
    }
    match run_engine(args) {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("engine: {}", error.code());
            std::process::exit(2);
        }
    }
}

fn run_engine(args: &[String]) -> Result<String, EngineCliError> {
    if matches!(
        args.first().map(String::as_str),
        Some(
            "context-view-receipt"
                | "context-sources-receipt"
                | "context-sources-receipt-v2"
                | "context-outcome"
                | "context-outcome-receipt"
                | "context-checkpoint"
                | "context-checkpoint-resume"
                | "context-checkpoint-package"
                | "context-checkpoint-import"
                | "context-checkpoint-continue"
                | "context-checkpoint-sources"
                | "context-personal-sync"
                | "signer-info"
        )
    ) {
        return host::run(args, &mut std::io::stdin().lock());
    }
    let cli = parse_cli_args(args)?;
    if matches!(
        cli.operation,
        EngineOperation::ContextLineage | EngineOperation::ContextPolicyEvidence
    ) {
        let request: ContextStoreRequestV1 = decode_request(&read_plan_request(
            &cli.json_file,
            MAX_CONTEXT_STORE_REQUEST_BYTES,
        )?)?;
        return context_store_read(
            cli.operation,
            &cli.project_root,
            request,
            LearningStore::open_default,
        );
    }
    if cli.operation == EngineOperation::EgressAdmit {
        let request: lean_ctx_protocol::EngineEgressAdmissionRequestV1 =
            decode_request(&read_plan_request(
                &cli.json_file,
                lean_ctx_protocol::MAX_ENGINE_EGRESS_REQUEST_BYTES,
            )?)?;
        request.validate().map_err(|_| EngineCliError::Request)?;
        let response = egress_admit(&request);
        return serde_json::to_string(&response).map_err(|_| EngineCliError::Host("internal"));
    }
    if cli.operation == EngineOperation::ContextMaterializeSources {
        let request: EngineContextSourceMaterializationRequestV1 = decode_request(
            &read_plan_request(&cli.json_file, MAX_ENGINE_SOURCE_PLAN_REQUEST_BYTES)?,
        )?;
        validate_header(
            request.source_plan.planning.schema_version,
            request.source_plan.planning.transport_version,
            &request.source_plan.planning.engine_interface_version,
        )?;
        let response =
            crate::core::engine_interface::context_plan::materialize(&cli.project_root, &request)
                .map_err(EngineCliError::Host)?;
        return serde_json::to_string(&response).map_err(|_| EngineCliError::Host("internal"));
    }
    if cli.operation == EngineOperation::ContextPlanSources {
        let request: EngineContextSourcePlanRequestV1 = decode_request(&read_plan_request(
            &cli.json_file,
            MAX_ENGINE_SOURCE_PLAN_REQUEST_BYTES,
        )?)?;
        validate_header(
            request.planning.schema_version,
            request.planning.transport_version,
            &request.planning.engine_interface_version,
        )?;
        let response =
            crate::core::engine_interface::context_plan::sources::plan(&cli.project_root, request)
                .map_err(EngineCliError::Host)?;
        return serde_json::to_string(&response).map_err(|_| EngineCliError::Host("internal"));
    }
    if cli.operation == EngineOperation::ContextPlan {
        let request: EngineContextPlanRequestV1 = decode_request(&read_plan_request(
            &cli.json_file,
            MAX_ENGINE_CONTEXT_PLAN_REQUEST_BYTES,
        )?)?;
        validate_header(
            request.schema_version,
            request.transport_version,
            &request.engine_interface_version,
        )?;
        let response =
            crate::core::engine_interface::context_plan::plan(&cli.project_root, request)
                .map_err(EngineCliError::Host)?;
        return serde_json::to_string(&response).map_err(|_| EngineCliError::Host("internal"));
    }
    let request = read_json_file(&cli.json_file)?;
    let result = match cli.operation {
        EngineOperation::ContextPlan
        | EngineOperation::ContextPlanSources
        | EngineOperation::ContextMaterializeSources
        | EngineOperation::EgressAdmit
        | EngineOperation::ContextLineage
        | EngineOperation::ContextPolicyEvidence => {
            return Err(EngineCliError::Request);
        }
        EngineOperation::ContextView => {
            let request: ContextViewRequestV1 = decode_request(&request)?;
            validate_header(
                request.schema_version,
                request.transport_version,
                &request.engine_interface_version,
            )?;
            if request.path.trim().is_empty() {
                return Err(EngineCliError::Request);
            }
            if request.mode != "aggressive" {
                return Err(EngineCliError::Engine(
                    EngineTransportError::UnsupportedMode,
                ));
            }
            execute_transport_context_view(&cli.project_root, &request.path)
                .map_err(EngineCliError::Engine)?
        }
        EngineOperation::Recover => {
            let request: RecoverRequestV1 = decode_request(&request)?;
            validate_header(
                request.schema_version,
                request.transport_version,
                &request.engine_interface_version,
            )?;
            if request.path.trim().is_empty() {
                return Err(EngineCliError::Request);
            }
            recover_transport_source(
                &cli.project_root,
                &request.path,
                &request.recovery_ref,
                &request.source_ref,
                &request.source_digest,
            )
            .map_err(EngineCliError::Engine)?
        }
    };
    encode_response(result)
}

const MAX_CONTEXT_STORE_REQUEST_BYTES: usize = 16 * 1024;

type LearningStore = crate::core::context_kernel::autopilot::learning_store::AdaptiveLearningStore;

/// Content-free reads of the Context Store: a task's lineage (ledger events
/// joined with its Decision Receipts) or the scope's read-strategy evidence.
fn context_store_read(
    operation: EngineOperation,
    project_root: &Path,
    request: ContextStoreRequestV1,
    open_learning: impl FnOnce(
        lean_ctx_protocol::ProjectId,
        Option<lean_ctx_protocol::TenantId>,
    ) -> anyhow::Result<LearningStore>,
) -> Result<String, EngineCliError> {
    validate_header(
        request.schema_version,
        request.transport_version,
        &request.engine_interface_version,
    )?;
    // The same root checks as every other operation, even though the scope
    // is named by the project ID.
    crate::core::engine_interface::bind_transport_root(project_root)
        .map_err(EngineCliError::Engine)?;
    let project = match request.project_id {
        Some(project) => project,
        None => lean_ctx_protocol::ProjectId::new(
            project_root
                .to_str()
                .ok_or(EngineCliError::Request)?
                .to_owned(),
        )
        .map_err(|_| EngineCliError::Request)?,
    };
    let header = |body| ContextStoreResponseV1 {
        schema_version: 1,
        transport_version: ENGINE_TRANSPORT_VERSION,
        engine_interface_version: request.engine_interface_version.clone(),
        body,
    };
    let encoded = match (operation, &request.task_id) {
        (EngineOperation::ContextLineage, Some(task)) => {
            let scope =
                crate::core::context_store::task_scope(request.tenant_id.as_ref(), &project);
            let lineage = crate::core::context_store::lineage::load(&scope, task.as_str());
            serde_json::to_string(&header(serde_json::json!({ "lineage": lineage })))
        }
        (EngineOperation::ContextPolicyEvidence, None) => {
            let store = open_learning(project, request.tenant_id.clone())
                .map_err(|_| EngineCliError::Host("context_store_unavailable"))?;
            let evidence = store
                .policy_evidence()
                .map_err(|_| EngineCliError::Host("context_store_unavailable"))?;
            serde_json::to_string(&header(serde_json::json!({ "evidence": evidence })))
        }
        _ => return Err(EngineCliError::Request),
    };
    encoded.map_err(|_| EngineCliError::Host("internal"))
}

/// Egress admission for a host that forwards model requests itself: the same
/// admission the lean-ctx proxy applies before a request leaves this machine.
fn egress_admit(
    request: &lean_ctx_protocol::EngineEgressAdmissionRequestV1,
) -> lean_ctx_protocol::EngineEgressAdmissionResponseV1 {
    use crate::core::context_admission::egress::{self, EgressBody, EgressTarget};
    use lean_ctx_protocol::{
        ENGINE_EGRESS_SCHEMA_VERSION, EgressDispositionV1, EngineEgressAdmissionResponseV1,
    };

    let target = EgressTarget {
        provider: &request.provider,
        model: request
            .body
            .get("model")
            .and_then(serde_json::Value::as_str),
        upstream_base: &request.upstream_base,
    };
    let outcome = egress::admit_request(&request.body, &target);
    let original = serde_json::to_vec(&request.body).unwrap_or_default();
    let (disposition, body, refusal) = match &outcome.body {
        EgressBody::Unchanged => (
            EgressDispositionV1::Forward,
            Some(request.body.clone()),
            None,
        ),
        EgressBody::Rewritten(admitted) => {
            (EgressDispositionV1::Rewritten, Some(admitted.clone()), None)
        }
        EgressBody::Refused(reason) => (EgressDispositionV1::Refused, None, Some(reason.clone())),
    };
    let forwarded = body
        .as_ref()
        .map(|body| serde_json::to_vec(body).unwrap_or_default());
    let receipt = egress::finish(&outcome, forwarded.as_deref(), original.len(), None);
    EngineEgressAdmissionResponseV1 {
        schema_version: ENGINE_EGRESS_SCHEMA_VERSION,
        disposition,
        body,
        refusal,
        classification: outcome.classification,
        receipt,
    }
}

fn parse_cli_args(args: &[String]) -> Result<EngineCliArgs, EngineCliError> {
    let operation = match args.first().map(String::as_str) {
        Some("context-plan") => EngineOperation::ContextPlan,
        Some("context-plan-sources") => EngineOperation::ContextPlanSources,
        Some("context-materialize-sources") => EngineOperation::ContextMaterializeSources,
        Some("context-view") => EngineOperation::ContextView,
        Some("egress-admit") => EngineOperation::EgressAdmit,
        Some("recover") => EngineOperation::Recover,
        Some("context-lineage") => EngineOperation::ContextLineage,
        Some("context-policy-evidence") => EngineOperation::ContextPolicyEvidence,
        _ => return Err(EngineCliError::Usage),
    };
    let mut project_root = None;
    let mut json_file = None;
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = match flag {
            "--project-root" | "--json-file" => {
                index += 1;
                args.get(index).ok_or(EngineCliError::Usage)?.clone()
            }
            _ => return Err(EngineCliError::Usage),
        };
        match flag {
            "--project-root" => {
                if project_root.replace(PathBuf::from(value)).is_some() {
                    return Err(EngineCliError::Usage);
                }
            }
            "--json-file" => {
                if json_file.replace(PathBuf::from(value)).is_some() {
                    return Err(EngineCliError::Usage);
                }
            }
            _ => unreachable!("flag was validated above"),
        }
        index += 1;
    }
    Ok(EngineCliArgs {
        operation,
        project_root: project_root.ok_or(EngineCliError::Usage)?,
        json_file: json_file.ok_or(EngineCliError::Usage)?,
    })
}

fn read_json_file(path: &Path) -> Result<String, EngineCliError> {
    let bytes = std::fs::read(path).map_err(|_| EngineCliError::JsonFile)?;
    String::from_utf8(bytes).map_err(|_| EngineCliError::JsonFile)
}

fn read_plan_request(path: &Path, max_bytes: usize) -> Result<String, EngineCliError> {
    if path == Path::new("-") {
        return read_plan_payload(std::io::stdin().lock(), max_bytes);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| EngineCliError::JsonFile)?;
    if !file
        .metadata()
        .map_err(|_| EngineCliError::JsonFile)?
        .is_file()
    {
        return Err(EngineCliError::JsonFile);
    }
    read_plan_payload(file, max_bytes)
}

fn read_plan_payload(reader: impl Read, max_bytes: usize) -> Result<String, EngineCliError> {
    let mut bytes = Vec::new();
    reader
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| EngineCliError::JsonFile)?;
    if bytes.len() > max_bytes {
        return Err(EngineCliError::Request);
    }
    String::from_utf8(bytes).map_err(|_| EngineCliError::Request)
}

fn decode_request<T: DeserializeOwned>(json: &str) -> Result<T, EngineCliError> {
    let mut deserializer = serde_json::Deserializer::from_str(json);
    let request = T::deserialize(&mut deserializer).map_err(|_| EngineCliError::Request)?;
    deserializer.end().map_err(|_| EngineCliError::Request)?;
    Ok(request)
}

fn validate_header(
    schema_version: u32,
    transport_version: u32,
    engine_interface_version: &SemanticVersion,
) -> Result<(), EngineCliError> {
    if schema_version != 1 {
        return Err(EngineCliError::UnsupportedSchema);
    }
    if transport_version != ENGINE_TRANSPORT_VERSION {
        return Err(EngineCliError::UnsupportedTransport);
    }
    if engine_interface_version.as_str() != ENGINE_INTERFACE_VERSION {
        return Err(EngineCliError::UnsupportedInterface);
    }
    Ok(())
}

fn encode_response(result: EngineTransportResult) -> Result<String, EngineCliError> {
    let interface_version =
        SemanticVersion::new(ENGINE_INTERFACE_VERSION).map_err(|_| EngineCliError::Request)?;
    let EngineTransportResult {
        view,
        invocation,
        observation,
        recovery,
    } = result;
    serde_json::to_string(&EngineWireResponseV1 {
        schema_version: 1,
        transport_version: ENGINE_TRANSPORT_VERSION,
        engine_interface_version: interface_version,
        view: wire_view(view),
        invocation,
        observation,
        recovery: wire_recovery(recovery),
    })
    .map_err(|_| EngineCliError::Request)
}

fn wire_view(view: EngineTransportView) -> EngineWireViewV1 {
    EngineWireViewV1 {
        text: view.text,
        output_ref: view.output_ref,
        output_digest: view.output_digest,
    }
}

fn wire_recovery(recovery: EngineTransportRecoveryDescriptor) -> EngineWireRecoveryV1 {
    EngineWireRecoveryV1 {
        recovery_ref: recovery.recovery_ref,
        source_ref: recovery.source_ref,
        source_digest: recovery.source_digest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egress_admit_masks_a_credential_and_reports_classification_and_receipt() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let key = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let request = lean_ctx_protocol::EngineEgressAdmissionRequestV1 {
            schema_version: lean_ctx_protocol::ENGINE_EGRESS_SCHEMA_VERSION,
            provider: "openai".into(),
            upstream_base: "https://api.openai.com".into(),
            body: serde_json::json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": format!("deploy with {key} today")}]
            }),
        };
        let response = egress_admit(&request);
        response.validate().expect("a consistent response");
        assert_eq!(
            response.disposition,
            lean_ctx_protocol::EgressDispositionV1::Rewritten
        );
        let forwarded = response.body.expect("a body to forward").to_string();
        assert!(!forwarded.contains(key), "{forwarded}");
        assert!(forwarded.contains("deploy with"));
        assert!(response.classification.is_some());
        let receipt = response.receipt.expect("a receipt");
        assert!(receipt.security.redactions >= 1);
    }

    #[test]
    fn context_store_reads_are_scoped_strict_and_content_free() {
        let data = crate::core::data_dir::isolated_data_dir();
        // Private learning storage verifies its directory chain, which temp
        // dirs do not pass; tests hand the read a host-opened database.
        let open = |project, tenant| {
            LearningStore::new(
                rusqlite::Connection::open(data.path().join("learning.sqlite3"))?,
                project,
                tenant,
            )
        };
        let request = |extra: &str| -> ContextStoreRequestV1 {
            decode_request(&format!(
                r#"{{"schema_version":1,"transport_version":1,"engine_interface_version":"1.0.0","project_id":"p"{extra}}}"#
            ))
            .expect("valid request")
        };
        let project = tempfile::tempdir().expect("project root");
        let root = project.path();
        let evidence: serde_json::Value = serde_json::from_str(
            &context_store_read(
                EngineOperation::ContextPolicyEvidence,
                root,
                request(""),
                open,
            )
            .expect("evidence"),
        )
        .expect("json");
        assert_eq!(evidence["schema_version"], 1);
        assert_eq!(evidence["evidence"]["records"], serde_json::json!([]));

        let lineage: serde_json::Value = serde_json::from_str(
            &context_store_read(
                EngineOperation::ContextLineage,
                root,
                request(r#","task_id":"task-1""#),
                open,
            )
            .expect("lineage"),
        )
        .expect("json");
        assert_eq!(lineage["lineage"]["task_id"], "task-1");

        for (operation, extra) in [
            (EngineOperation::ContextLineage, ""),
            (
                EngineOperation::ContextPolicyEvidence,
                r#","task_id":"task-1""#,
            ),
        ] {
            assert!(context_store_read(operation, root, request(extra), open).is_err());
        }
        assert!(
            decode_request::<ContextStoreRequestV1>(
                r#"{"schema_version":1,"transport_version":1,"engine_interface_version":"1.0.0","query":"x"}"#
            )
            .is_err(),
            "no free-form fields"
        );
    }

    #[test]
    fn cli_requires_one_operation_and_two_explicit_files() {
        assert!(parse_cli_args(&[]).is_err());
        assert!(parse_cli_args(&["context-view".into()]).is_err());
        assert!(
            parse_cli_args(&[
                "context-view".into(),
                "--project-root".into(),
                "/tmp/project".into(),
            ])
            .is_err()
        );
    }

    #[test]
    fn request_shape_is_strict_and_versions_are_pinned() {
        let valid = r#"{
            "schema_version":1,
            "transport_version":1,
            "engine_interface_version":"1.0.0",
            "path":"fixture.md",
            "mode":"aggressive"
        }"#;
        let request: ContextViewRequestV1 = decode_request(valid).expect("valid request");
        validate_header(
            request.schema_version,
            request.transport_version,
            &request.engine_interface_version,
        )
        .expect("supported versions");
        assert!(
            decode_request::<ContextViewRequestV1>(
                &valid.replace("\"mode\":\"aggressive\"", "\"mode\":\"full\"")
            )
            .is_ok()
        );
        assert!(
            decode_request::<ContextViewRequestV1>(
                &valid.replace("\n        }", ",\n            \"unknown\":true\n        }")
            )
            .is_err()
        );
    }
}
