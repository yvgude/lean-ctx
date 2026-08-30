use std::path::{Path, PathBuf};

use lean_ctx_protocol::{
    EngineInvocationV1, EngineObservationV1, ProtocolReference, SemanticVersion, Sha256Digest,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::core::engine_interface::{
    ENGINE_INTERFACE_VERSION, ENGINE_TRANSPORT_VERSION, EngineTransportError,
    EngineTransportRecoveryDescriptor, EngineTransportResult, EngineTransportView,
    execute_transport_context_view, recover_transport_source,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EngineOperation {
    ContextView,
    Recover,
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
    let cli = parse_cli_args(args)?;
    let request = read_json_file(&cli.json_file)?;
    let result = match cli.operation {
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

fn parse_cli_args(args: &[String]) -> Result<EngineCliArgs, EngineCliError> {
    let operation = match args.first().map(String::as_str) {
        Some("context-view") => EngineOperation::ContextView,
        Some("recover") => EngineOperation::Recover,
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
