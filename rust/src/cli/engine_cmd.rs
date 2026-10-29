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

mod delivery;
mod host;
mod presenter;

use delivery::{
    EngineCall, deliver_engine_document, exit_unless_delivered, named_project_root, operation_word,
    scope_root,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineOperation {
    ContextPlan,
    ContextPlanSources,
    ContextMaterializeSources,
    ContextView,
    EgressAdmit,
    EgressPrepare,
    EgressReport,
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

/// The call of `lean-ctx engine <operation> …`. A host operation learns its
/// project from what it will execute — its arguments, the working directory
/// it selects, or the operator settings on `stdin` — so `stdin` is read once
/// here and the same bytes are handed to the operation.
fn engine_call<'a>(args: &'a [String], stdin: &mut impl Read) -> EngineCall<'a, EngineCliError> {
    if host::is_host_operation(args) {
        let input = host::read_input(args, stdin);
        return EngineCall {
            project_root: host::project_root(args, &input),
            run: Box::new(move || host::run(args, &mut input.as_slice())),
        };
    }
    EngineCall {
        project_root: named_project_root(args).map(scope_root),
        run: Box::new(move || run_engine(args)),
    }
}

pub(crate) fn cmd_engine(args: &[String]) {
    if args.first().map(String::as_str) == Some("tool-session") {
        super::agent_tools_cmd::cmd_agent_tools(&args[1..]);
        return;
    }
    let operation = operation_word(args);
    if args.first().map(String::as_str) == Some("runtime") {
        let call = EngineCall {
            project_root: named_project_root(args).map(scope_root),
            run: Box::new(|| crate::core::intelligence_runtime::run(&args[1..])),
        };
        let delivered = deliver_engine_document(operation, call, &mut std::io::stdout());
        exit_unless_delivered("engine runtime", delivered, |error| error.to_string());
        return;
    }
    // The stdin lock ends with this statement: an operation that reads its
    // request from stdin itself (`--json-file -`) takes it again.
    let call = engine_call(args, &mut std::io::stdin().lock());
    let delivered = deliver_engine_document(operation, call, &mut std::io::stdout());
    exit_unless_delivered("engine", delivered, |error| error.code().to_owned());
}

fn run_engine(args: &[String]) -> Result<String, EngineCliError> {
    if host::is_host_operation(args) {
        return host::run(args, &mut std::io::stdin().lock());
    }
    if matches!(
        args.first().map(String::as_str),
        Some(presenter::MATERIALIZE_V2 | presenter::REVOKE | presenter::USE)
    ) {
        return presenter::run(args);
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
        let project_root = crate::core::engine_interface::bind_transport_root(&cli.project_root)
            .map_err(EngineCliError::Engine)?;
        let response = crate::core::policy::runtime::with_explicit_project_source_view(
            &project_root,
            |authority| {
                let fingerprint = authority.fingerprint_bytes();
                egress_admit(&request, Some(&fingerprint), Some(authority))
            },
        )
        .map_err(|_| EngineCliError::Host("project_policy_changed"))?;
        return serde_json::to_string(&response).map_err(|_| EngineCliError::Host("internal"));
    }
    if cli.operation == EngineOperation::EgressPrepare {
        let request: lean_ctx_protocol::EngineEgressAdmissionRequestV1 =
            decode_request(&read_plan_request(
                &cli.json_file,
                lean_ctx_protocol::MAX_ENGINE_EGRESS_REQUEST_BYTES,
            )?)?;
        request.validate().map_err(|_| EngineCliError::Request)?;
        let project_root = crate::core::engine_interface::bind_transport_root(&cli.project_root)
            .map_err(EngineCliError::Engine)?;
        let response = crate::core::policy::runtime::with_explicit_project_source_view(
            &project_root,
            |authority| {
                let fingerprint = authority.fingerprint_bytes();
                egress_prepare(&request, Some(&fingerprint), Some(authority))
            },
        )
        .map_err(|_| EngineCliError::Host("project_policy_changed"))?;
        response
            .validate()
            .map_err(|_| EngineCliError::Host("internal"))?;
        return serde_json::to_string(&response).map_err(|_| EngineCliError::Host("internal"));
    }
    if cli.operation == EngineOperation::EgressReport {
        let request: lean_ctx_protocol::EngineEgressHostReportRequestV1 =
            decode_request(&read_plan_request(
                &cli.json_file,
                lean_ctx_protocol::MAX_ENGINE_EGRESS_REPORT_BYTES,
            )?)?;
        request.validate().map_err(|_| EngineCliError::Request)?;
        let response = crate::core::context_admission::egress::record_host_observation(&request)
            .map_err(|_| EngineCliError::Host("egress_observation_unavailable"))?;
        response
            .validate()
            .map_err(|_| EngineCliError::Host("egress_observation_unavailable"))?;
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
        | EngineOperation::EgressPrepare
        | EngineOperation::EgressReport
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
    project_fingerprint: Option<&[u8]>,
    project_authority: Option<&crate::core::policy::runtime::PublicationAuthority>,
) -> lean_ctx_protocol::EngineEgressAdmissionResponseV1 {
    egress_prepare_as(
        "egress-admit",
        request,
        project_fingerprint,
        project_authority,
    )
    .decision
}

/// Opt-in preparation API: unlike the frozen V1 `egress-admit` wire shape,
/// this response carries the content-addressed link needed by `egress-report`.
fn egress_prepare(
    request: &lean_ctx_protocol::EngineEgressAdmissionRequestV1,
    project_fingerprint: Option<&[u8]>,
    project_authority: Option<&crate::core::policy::runtime::PublicationAuthority>,
) -> lean_ctx_protocol::EngineEgressPrepareResponseV2 {
    egress_prepare_as(
        "egress-prepare",
        request,
        project_fingerprint,
        project_authority,
    )
}

/// The preparation both operations share. The host dispatches the request
/// itself, so its decision receipt is a host-egress receipt of `operation`,
/// indexed under the project the request was admitted for — the project the
/// explicit source view bound (`with_explicit_project_source_view`). Without
/// a bound project the receipt keeps the proxy's index, as before.
fn egress_prepare_as(
    operation: &str,
    request: &lean_ctx_protocol::EngineEgressAdmissionRequestV1,
    project_fingerprint: Option<&[u8]>,
    project_authority: Option<&crate::core::policy::runtime::PublicationAuthority>,
) -> lean_ctx_protocol::EngineEgressPrepareResponseV2 {
    use crate::core::context_admission::egress::{self, EgressBody, EgressTarget};
    use lean_ctx_protocol::{
        ENGINE_EGRESS_PREPARE_SCHEMA_VERSION, ENGINE_EGRESS_SCHEMA_VERSION, EgressDispositionV1,
        EngineEgressAdmissionResponseV1, EngineEgressPrepareResponseV2,
    };

    let target = EgressTarget {
        provider: &request.provider,
        model: request
            .body
            .get("model")
            .and_then(serde_json::Value::as_str),
        upstream_base: &request.upstream_base,
    };
    let outcome = project_fingerprint.map_or_else(
        || egress::admit_request(&request.body, &target),
        |fingerprint| egress::admit_request_for_project(&request.body, &target, fingerprint),
    );
    let policy_epoch_current = || {
        let source_policy = project_authority
            .map(crate::core::policy::runtime::PublicationAuthority::verify)
            .transpose()
            .is_ok();
        let gateway_policy = match project_fingerprint {
            Some(fingerprint) => outcome.policy_epoch_current_for_project(fingerprint),
            None => true,
        };
        source_policy && gateway_policy
    };
    let changed_policy = || EngineEgressPrepareResponseV2 {
        schema_version: ENGINE_EGRESS_PREPARE_SCHEMA_VERSION,
        decision: EngineEgressAdmissionResponseV1 {
            schema_version: ENGINE_EGRESS_SCHEMA_VERSION,
            disposition: EgressDispositionV1::Refused,
            body: None,
            refusal: Some("policy_changed".to_owned()),
            classification: outcome.classification,
            receipt: None,
        },
        prepared_body: None,
        admission_receipt_digest: None,
    };
    if !policy_epoch_current() {
        return changed_policy();
    }
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
    let project = crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .map(|root| root.to_string_lossy().into_owned());
    let caller = match project.as_deref() {
        Some(project_root) => egress::EgressCaller::Host {
            project_root,
            operation,
        },
        None => egress::EgressCaller::Proxy,
    };
    let finished =
        egress::finish_admission_for(&outcome, forwarded.as_deref(), original.len(), None, caller);
    let evidence = match finished {
        Ok(evidence) => evidence,
        Err(reason) => {
            return EngineEgressPrepareResponseV2 {
                schema_version: ENGINE_EGRESS_PREPARE_SCHEMA_VERSION,
                decision: EngineEgressAdmissionResponseV1 {
                    schema_version: ENGINE_EGRESS_SCHEMA_VERSION,
                    disposition: EgressDispositionV1::Refused,
                    body: None,
                    refusal: Some(reason.to_owned()),
                    classification: outcome.classification,
                    receipt: None,
                },
                prepared_body: None,
                admission_receipt_digest: None,
            };
        }
    };
    if !policy_epoch_current() {
        return changed_policy();
    }
    let (receipt, admission_receipt_digest) = evidence.map_or((None, None), |evidence| {
        (evidence.decision_receipt, evidence.admission_digest)
    });
    let prepared_body = forwarded.map(|bytes| {
        String::from_utf8(bytes).expect("JSON serialization always emits valid UTF-8")
    });
    EngineEgressPrepareResponseV2 {
        schema_version: ENGINE_EGRESS_PREPARE_SCHEMA_VERSION,
        decision: EngineEgressAdmissionResponseV1 {
            schema_version: ENGINE_EGRESS_SCHEMA_VERSION,
            disposition,
            body,
            refusal,
            classification: outcome.classification,
            receipt,
        },
        prepared_body,
        admission_receipt_digest,
    }
}

fn parse_cli_args(args: &[String]) -> Result<EngineCliArgs, EngineCliError> {
    let operation = match args.first().map(String::as_str) {
        Some("context-plan") => EngineOperation::ContextPlan,
        Some("context-plan-sources") => EngineOperation::ContextPlanSources,
        Some("context-materialize-sources") => EngineOperation::ContextMaterializeSources,
        Some("context-view") => EngineOperation::ContextView,
        Some("egress-admit") => EngineOperation::EgressAdmit,
        Some("egress-prepare") => EngineOperation::EgressPrepare,
        Some("egress-report") => EngineOperation::EgressReport,
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
    fn legacy_egress_admit_wire_stays_v1_while_prepare_exposes_report_link() {
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
        let legacy = egress_admit(&request, None, None);
        legacy.validate().expect("a consistent legacy response");
        let legacy_json = serde_json::to_value(&legacy).expect("legacy JSON");
        assert!(legacy_json.get("admission_receipt_digest").is_none());

        let prepared = egress_prepare(&request, None, None);
        prepared
            .validate()
            .expect("a consistent preparation response");
        let admission_digest = prepared
            .admission_receipt_digest
            .clone()
            .expect("the prepared bytes have a durable admission digest");
        let response = prepared.decision;
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

        let report = lean_ctx_protocol::EngineEgressHostReportRequestV1 {
            schema_version: lean_ctx_protocol::ENGINE_EGRESS_SCHEMA_VERSION,
            admission_receipt_digest: admission_digest,
            attempt: 1,
            transport: lean_ctx_protocol::context_gateway::EgressTransportV1::Http,
            result: lean_ctx_protocol::context_gateway::EgressTransportResultV1::ResponseObserved,
            http_status: Some(503),
            failure: None,
        };
        report.validate().expect("bounded host report");
        let stored = crate::core::context_admission::egress::record_host_observation(&report)
            .expect("the report names a stored prepared admission");
        stored
            .validate()
            .expect("the Engine labels it HostReported");
        assert_eq!(
            stored.observation.observer,
            lean_ctx_protocol::context_gateway::EgressTransportObserverV1::HostReported
        );
    }

    #[test]
    fn egress_admit_refuses_body_when_mandatory_receipt_store_fails() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let config = crate::core::paths::config_dir_read_only().expect("config directory");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("governed config");
        let data = crate::core::data_dir::lean_ctx_data_dir().expect("data directory");
        std::fs::create_dir_all(&data).expect("data directory");
        std::fs::write(data.join("gateway"), b"blocked test store").expect("store obstruction");
        let request = lean_ctx_protocol::EngineEgressAdmissionRequestV1 {
            schema_version: lean_ctx_protocol::ENGINE_EGRESS_SCHEMA_VERSION,
            provider: "openai".into(),
            upstream_base: "http://127.0.0.1:11434".into(),
            body: serde_json::json!({"model":"m","messages":[{"role":"user","content":"hello"}]}),
        };
        let response = egress_admit(&request, None, None);
        response.validate().expect("consistent refusal");
        assert_eq!(
            response.disposition,
            lean_ctx_protocol::EgressDispositionV1::Refused
        );
        assert!(response.body.is_none());
        assert!(response.receipt.is_none());
        assert_eq!(
            response.refusal.as_deref(),
            Some(crate::core::context_admission::egress::RECEIPT_UNAVAILABLE)
        );
    }

    #[test]
    fn egress_admit_refuses_dispatch_when_lifecycle_store_fails_in_governed_mode() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let config = crate::core::paths::config_dir_read_only().expect("config directory");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("governed config");
        let data = crate::core::data_dir::lean_ctx_data_dir().expect("data directory");
        std::fs::create_dir_all(data.join("gateway")).expect("legacy receipt store");
        std::fs::write(data.join("gateway/egress"), b"blocked lifecycle store")
            .expect("lifecycle store obstruction");
        let request = lean_ctx_protocol::EngineEgressAdmissionRequestV1 {
            schema_version: lean_ctx_protocol::ENGINE_EGRESS_SCHEMA_VERSION,
            provider: "openai".into(),
            upstream_base: "http://127.0.0.1:11434".into(),
            body: serde_json::json!({
                "model":"m",
                "messages":[{"role":"user","content":"hello"}]
            }),
        };
        let response = egress_admit(&request, None, None);
        response.validate().expect("consistent refusal");
        assert_eq!(
            response.disposition,
            lean_ctx_protocol::EgressDispositionV1::Refused
        );
        assert!(response.body.is_none());
        assert!(
            serde_json::to_value(&response)
                .expect("legacy refusal JSON")
                .get("admission_receipt_digest")
                .is_none()
        );
        assert_eq!(
            response.refusal.as_deref(),
            Some(crate::core::context_admission::egress::RECEIPT_UNAVAILABLE)
        );
    }

    #[test]
    #[serial_test::serial(cache_telemetry)]
    fn cli_restricted_remote_refusal_returns_a_persisted_linked_withheld_receipt() {
        use lean_ctx_protocol::context_gateway::{
            ClassificationV1, DeliveryOutcomeV1, EgressAdmissionOutcomeV1,
        };
        use lean_ctx_protocol::{
            EgressDispositionV1, EngineEgressAdmissionRequestV1, EngineEgressAdmissionResponseV1,
            EngineEgressPrepareResponseV2,
        };

        let _data = crate::core::data_dir::isolated_data_dir();
        let config = crate::core::paths::config_dir_read_only().expect("config directory");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"governed\"\n",
        )
        .expect("governed config");

        let project = tempfile::tempdir().expect("project root");
        let cases = [
            (
                "restricted",
                serde_json::json!({
                    "model": "m",
                    "messages": [{
                        "role": "user",
                        "content": "Classification: Secret\nprivate deployment plan"
                    }]
                }),
                DeliveryOutcomeV1::Withheld,
                false,
            ),
            // A restricted block next to admitted content is withheld in
            // place and the request goes (no lockout, 26.10.): see
            // `egress_tests::a_restricted_block_in_the_history_is_withheld_in_place…`.
        ];

        for (name, body, expected_decision_outcome, has_admitted_source) in cases {
            let request = EngineEgressAdmissionRequestV1 {
                schema_version: lean_ctx_protocol::ENGINE_EGRESS_SCHEMA_VERSION,
                provider: "anthropic".into(),
                upstream_base: "https://api.anthropic.com".into(),
                body,
            };
            let request_file = project.path().join(format!("{name}.json"));
            std::fs::write(
                &request_file,
                serde_json::to_vec(&request).expect("request JSON"),
            )
            .expect("request file");

            for operation in ["egress-admit", "egress-prepare"] {
                let args = vec![
                    operation.to_owned(),
                    "--project-root".to_owned(),
                    project.path().to_string_lossy().into_owned(),
                    "--json-file".to_owned(),
                    request_file.to_string_lossy().into_owned(),
                ];
                let wire: serde_json::Value =
                    serde_json::from_str(&run_engine(&args).expect("CLI response"))
                        .expect("response JSON");
                let (decision, returned_admission_digest) = if operation == "egress-admit" {
                    assert!(wire.get("admission_receipt_digest").is_none());
                    (
                        serde_json::from_value::<EngineEgressAdmissionResponseV1>(wire.clone())
                            .expect("legacy V1 response"),
                        None,
                    )
                } else {
                    let prepared: EngineEgressPrepareResponseV2 =
                        serde_json::from_value(wire.clone()).expect("prepare V2 response");
                    prepared.validate().expect("consistent prepared refusal");
                    assert!(prepared.prepared_body.is_none());
                    (prepared.decision, prepared.admission_receipt_digest)
                };

                decision.validate().expect("consistent egress refusal");
                assert_eq!(decision.disposition, EgressDispositionV1::Refused);
                assert!(decision.body.is_none());
                assert_eq!(decision.classification, Some(ClassificationV1::Restricted));
                let refusal = decision.refusal.as_deref().expect("refusal reason");
                assert!(
                    refusal.contains("destination.remote_restricted"),
                    "{refusal}"
                );
                assert!(!refusal.contains("deployment plan"), "{refusal}");
                let receipt = decision
                    .receipt
                    .as_ref()
                    .expect("persisted decision receipt");
                assert_eq!(receipt.outcome, expected_decision_outcome);
                assert_eq!(receipt.sources.permitted > 0, has_admitted_source);
                assert!(receipt.final_context.is_none());
                assert!(receipt.decisions.iter().any(|entry| {
                    entry
                        .reason_codes
                        .iter()
                        .any(|reason| reason.as_str() == "destination.remote_restricted")
                }));
                if operation == "egress-prepare" {
                    assert!(returned_admission_digest.is_some());
                }

                let decision_digest = receipt.canonical_digest().expect("decision digest");
                let lifecycle = crate::core::context_admission::egress::load_egress_for_decision(
                    &decision_digest,
                )
                .expect("load persisted lifecycle")
                .expect("decision lifecycle link");
                let admission = lifecycle.admission.expect("linked admission receipt");
                assert_eq!(
                    admission.admission.decision_receipt_digest.as_ref(),
                    Some(&decision_digest)
                );
                assert_eq!(
                    admission.admission.outcome,
                    EgressAdmissionOutcomeV1::Withheld
                );
                assert!(admission.admission.prepared_bytes_digest.is_none());
                if let Some(returned_digest) = returned_admission_digest {
                    assert_eq!(returned_digest, admission.digest);
                }
            }
        }
    }

    #[test]
    #[serial_test::serial(cache_telemetry)]
    fn cli_egress_operations_apply_explicit_project_policy_and_keep_v1_shape() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let config = crate::core::paths::config_dir_read_only().expect("config directory");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::write(
            config.join("config.toml"),
            "[context_gateway]\nmode = \"developer\"\n",
        )
        .expect("gateway config");

        let project = tempfile::tempdir().expect("project root");
        let policy = project.path().join(".lean-ctx");
        std::fs::create_dir_all(&policy).expect("project policy directory");
        std::fs::write(
            policy.join("policy.toml"),
            "name = \"cli-egress-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n[filters.blocked_patterns]\ncustomer = 'K-482193'\n",
        )
        .expect("project policy");
        let request_file = project.path().join("request.json");
        std::fs::write(
            &request_file,
            r#"{"schema_version":1,"provider":"openai","upstream_base":"https://api.openai.com","body":{"model":"m","messages":[{"role":"user","content":"Contact K-482193"}]}}"#,
        )
        .expect("egress request");

        for operation in ["egress-admit", "egress-prepare"] {
            let args = vec![
                operation.to_owned(),
                "--project-root".to_owned(),
                project.path().to_string_lossy().into_owned(),
                "--json-file".to_owned(),
                request_file.to_string_lossy().into_owned(),
            ];
            let response: serde_json::Value =
                serde_json::from_str(&run_engine(&args).expect("egress response"))
                    .expect("egress JSON");
            let decision = if operation == "egress-prepare" {
                &response["decision"]
            } else {
                assert!(response.get("admission_receipt_digest").is_none());
                &response
            };
            assert_eq!(decision["disposition"], "refused", "{response}");
            assert!(decision["body"].is_null(), "{response}");
            assert!(
                !response.to_string().contains("K-482193"),
                "the refusal must not echo blocked content: {response}"
            );
        }
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
        let report = parse_cli_args(&[
            "egress-report".into(),
            "--project-root".into(),
            "/tmp/project".into(),
            "--json-file".into(),
            "-".into(),
        ])
        .expect("host report operation");
        assert_eq!(report.operation, EngineOperation::EgressReport);
        let prepare = parse_cli_args(&[
            "egress-prepare".into(),
            "--project-root".into(),
            "/tmp/project".into(),
            "--json-file".into(),
            "-".into(),
        ])
        .expect("opt-in preparation operation");
        assert_eq!(prepare.operation, EngineOperation::EgressPrepare);
    }

    fn gateway_mode(mode: &str) {
        let config = crate::core::paths::config_dir_read_only().expect("config directory");
        std::fs::create_dir_all(&config).expect("config directory");
        std::fs::write(
            config.join("config.toml"),
            format!("[context_gateway]\nmode = \"{mode}\"\n"),
        )
        .expect("gateway config");
    }

    /// G08: an Engine-interface document is ONE receipted delivery of the
    /// project its operation works on. The receipt binds exactly the bytes
    /// written and is sealed before the write; the hand-over is recorded
    /// only after the write succeeded; without the mandatory receipt nothing
    /// is written.
    #[test]
    fn an_engine_document_is_receipted_for_its_project_or_not_written() {
        use super::delivery::DeliveryFailure;
        use crate::core::context_admission::receipt_store::{self, observed, roots, trace};
        use std::io::Write;
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        gateway_mode("governed");
        let project = tempfile::tempdir().expect("project");
        let root = project.path().canonicalize().expect("canonical root");
        let key = root.to_string_lossy().into_owned();
        let call = |source: &'static str| EngineCall::<EngineCliError> {
            project_root: Some(root.clone()),
            run: Box::new(move || {
                crate::core::context_admission::admit_context_source(source)
                    .map(|admitted| serde_json::json!({ "view": admitted.text }).to_string())
                    .map_err(|_| EngineCliError::Host("source_withheld"))
            }),
        };

        let mut out = Vec::new();
        deliver_engine_document("context-view", call("pub fn engine_body() {}\n"), &mut out)
            .expect("delivered");
        assert!(out.ends_with(b"}\n") && String::from_utf8_lossy(&out).contains("engine_body"));
        let found = receipt_store::by_output(&key, &receipt_store::output_digest(&out));
        assert_eq!(
            found.len(),
            1,
            "one receipt binds exactly the written bytes"
        );
        let written = found[0]
            .1
            .as_ref()
            .expect("a verified receipt")
            .digest
            .clone();
        let listed = receipt_store::latest(&key, 8);
        assert_eq!(listed.len(), 1, "indexed under the operation's project");
        assert_eq!(listed[0].0, written.hex());
        assert!(receipt_store::latest(roots::ENGINE_KEY, 8).is_empty());
        let trace::StoredTrace::Bound { sidecar, .. } = trace::load(&written) else {
            panic!("a mandatory receipt has its sidecar");
        };
        assert_eq!(sidecar.transport, trace::Transport::EngineInterface);
        assert_eq!(sidecar.release_prepared.channel, trace::Channel::Stdout);
        assert!(matches!(
            observed::load(&written),
            observed::StoredObservation::Bound {
                observed: observed::Observed::HandedOver { .. },
                ..
            }
        ));

        // Sealed, then the write fails: the receipt stays, nothing is observed.
        let unwritten = call("pub fn unwritten_body() {}\n");
        let failed = deliver_engine_document("context-view", unwritten, &mut Closed);
        assert!(matches!(failed, Err(DeliveryFailure::Write(_))));
        let listed = receipt_store::latest(&key, 8);
        assert_eq!(listed.len(), 2, "the receipt was sealed before the write");
        let sealed = listed[0].1.as_ref().expect("a verified receipt");
        assert_eq!(
            observed::load(&sealed.digest),
            observed::StoredObservation::Absent
        );

        // A document whose sources pass the document admission of the
        // checkpoint operations: the receipt of the invocation carries those
        // decisions with their field references, not `no_source_decision`.
        #[cfg(unix)]
        {
            let checkpoint = EngineCall::<EngineCliError> {
                project_root: Some(root.clone()),
                run: Box::new(|| {
                    let document = serde_json::json!({ "findings": ["The lexer drops commas"] });
                    crate::core::execution_ledger::host::admit_foreign_document_narrative(
                        "checkpoint:",
                        &document,
                        "context-checkpoint-import",
                    )
                    .map(|()| document.to_string())
                    .map_err(|_| EngineCliError::Host("host_checkpoint_import_rejected"))
                }),
            };
            deliver_engine_document(
                "context-checkpoint-import",
                checkpoint,
                &mut Vec::<u8>::new(),
            )
            .expect("delivered");
            let listed = receipt_store::latest(&key, 8);
            let newest = listed[0].1.as_ref().expect("a verified receipt");
            let trace::StoredTrace::Bound { sidecar, .. } = trace::load(&newest.digest) else {
                panic!("a mandatory receipt has its sidecar");
            };
            assert!(!sidecar.unsourced, "the document's sources were decided on");
            assert!(
                sidecar.objects.iter().any(|object| matches!(
                    &object.source,
                    Some(trace::SourceRef::Id(reference)) if reference.starts_with("checkpoint:")
                )),
                "decisions carry the document's field references"
            );
        }

        // The store cannot persist: a regular file where its directory would be.
        let gateway = crate::core::data_dir::lean_ctx_data_dir()
            .expect("data directory")
            .join("gateway");
        std::fs::remove_dir_all(&gateway).expect("the store of the first calls");
        std::fs::write(&gateway, b"not a directory").expect("blocked store");
        let mut out = Vec::new();
        let withheld = call("pub fn withheld_body() {}\n");
        let refused = deliver_engine_document("context-view", withheld, &mut out);
        assert!(matches!(refused, Err(DeliveryFailure::ReceiptNotPersisted)));
        assert!(out.is_empty(), "no receipt, no byte");
    }

    /// G08: a host operation's project is learned from what it executes —
    /// here the operator settings on stdin, read once — before its capture
    /// is opened, so a protection that exists ONLY as that project's policy
    /// pack makes its receipt mandatory and the receipt is the project's.
    #[test]
    fn a_host_operation_is_scoped_to_the_project_its_settings_name() {
        use crate::core::context_admission::receipt_store::{self, roots, trace};
        let _data = crate::core::data_dir::isolated_data_dir();
        let _live = crate::core::policy::runtime::lock_test_override();
        gateway_mode("developer");
        let project = tempfile::tempdir().expect("project");
        let root = project.path().canonicalize().expect("canonical root");
        std::fs::create_dir_all(root.join(".lean-ctx")).expect("policy directory");
        std::fs::write(
            root.join(".lean-ctx/policy.toml"),
            "name = \"host-scope-pack\"\nversion = \"1.0.0\"\ndescription = \"test\"\n",
        )
        .expect("project policy");
        let settings = serde_json::json!({
            "schema_version": 1,
            "signing_key_hex": "skipped by the scope step",
            "checkpoint_resume": {
                "project_root": project.path(), "project_id": "p",
                "tenant_id": "t", "workspace_id": "w",
            },
        })
        .to_string();
        let args = [
            "context-checkpoint-resume",
            "--json",
            "request.json",
            "--host-stdin",
        ]
        .map(String::from);

        let input = host::read_input(&args, &mut settings.as_bytes());
        assert_eq!(
            input.as_slice(),
            settings.as_bytes(),
            "read once, handed on"
        );
        assert!(host::read_input(&args[..3], &mut settings.as_bytes()).is_empty());
        let scoped = host::project_root(&args, &input);
        assert_eq!(scoped.as_deref(), Some(root.as_path()));
        let keyed = ["signer-info", "--key-stdin"].map(String::from);
        assert_eq!(
            host::project_root(&keyed, &input),
            None,
            "works on no project"
        );

        let call = EngineCall::<EngineCliError> {
            project_root: scoped,
            run: Box::new(|| Ok(r#"{"schema_version":1}"#.to_owned())),
        };
        deliver_engine_document("context-checkpoint-resume", call, &mut Vec::<u8>::new())
            .expect("delivered");
        let listed = receipt_store::latest(&root.to_string_lossy(), 8);
        assert_eq!(listed.len(), 1, "mandatory through the pack alone");
        let stored = listed[0].1.as_ref().expect("a verified receipt");
        let trace::StoredTrace::Bound { sidecar, .. } = trace::load(&stored.digest) else {
            panic!("a mandatory receipt has its sidecar");
        };
        assert_eq!(sidecar.pack_name.as_deref(), Some("host-scope-pack"));
        assert!(receipt_store::latest(roots::ENGINE_KEY, 8).is_empty());
    }
}
