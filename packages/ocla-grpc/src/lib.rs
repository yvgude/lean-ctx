//! Bounded, loopback-only gRPC projection of the public OCLA v1 verifier.

#![forbid(unsafe_code)]

use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use lean_ctx_client::{
    AgentEnvelopeV1, CanonicalTokenEnvelopeV1, OCLA_API_VERSION, OclaGatewayAdmissibilityError,
    OclaRequestContext, OclaWireError, TokenBalanceV1, TokenEnvelopeSurface, TokenFlowDirection,
    decode_agent_envelope, decode_canonical_token_envelope, verify_agent_gateway_admissibility,
};
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status};

/// Generated public OCLA v1 gRPC types and client/server bindings.
#[allow(clippy::all, clippy::pedantic, missing_docs)]
pub mod proto {
    tonic::include_proto!("leanctx.ocla.v1");
}

use proto::ocla_verifier_server::{OclaVerifier, OclaVerifierServer};
use proto::{
    AgentEnvelope, CanonicalTokenEnvelope, Direction, Rejection, RequestContext, Surface,
    VerificationResult,
};

/// Maximum accepted encoded gRPC request size.
pub const MAX_GRPC_MESSAGE_BYTES: usize = 64 * 1024;
/// Maximum encoded response size.
pub const MAX_GRPC_RESPONSE_BYTES: usize = 4 * 1024;
/// Server-side upper bound for one unary verification call.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// Per-connection in-flight request ceiling.
pub const MAX_IN_FLIGHT_PER_CONNECTION: usize = 16;
/// Process-wide in-flight verifier-call ceiling across all server instances.
///
/// Accepted sockets and standard gRPC health calls are outside this limit.
pub const MAX_GLOBAL_IN_FLIGHT: usize = 64;

fn process_verifier_permits() -> Arc<Semaphore> {
    static PROCESS_VERIFIER_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(
        PROCESS_VERIFIER_PERMITS.get_or_init(|| Arc::new(Semaphore::new(MAX_GLOBAL_IN_FLIGHT))),
    )
}

/// Public OCLA verifier service.
#[derive(Debug)]
pub struct OclaVerifierService {
    permits: Arc<Semaphore>,
}

impl Default for OclaVerifierService {
    fn default() -> Self {
        Self {
            permits: process_verifier_permits(),
        }
    }
}

impl OclaVerifierService {
    #[cfg(test)]
    fn with_permits(permits: Arc<Semaphore>) -> Self {
        Self { permits }
    }

    fn permit(&self) -> Result<OwnedSemaphorePermit, Status> {
        self.permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("OCLA verifier capacity exhausted"))
    }
}

#[tonic::async_trait]
impl OclaVerifier for OclaVerifierService {
    async fn verify_token_envelope(
        &self,
        request: Request<CanonicalTokenEnvelope>,
    ) -> Result<Response<VerificationResult>, Status> {
        let _permit = self.permit()?;
        Ok(Response::new(verify_token(request.into_inner())))
    }

    async fn verify_agent_envelope(
        &self,
        request: Request<AgentEnvelope>,
    ) -> Result<Response<VerificationResult>, Status> {
        let _permit = self.permit()?;
        Ok(Response::new(verify_agent(request.into_inner())))
    }
}

/// Parse a listener address and enforce the unauthenticated v1 loopback boundary.
///
/// # Errors
///
/// Returns a stable error when the value is not a socket address or is not loopback.
pub fn parse_loopback_addr(value: &str) -> Result<SocketAddr, &'static str> {
    let address = value
        .parse::<SocketAddr>()
        .map_err(|_| "invalid listen address")?;
    if !address.ip().is_loopback() {
        return Err("OCLA gRPC v1 requires a loopback listen address");
    }
    Ok(address)
}

/// Serve the OCLA verifier over a pre-bound loopback listener.
///
/// # Errors
///
/// Returns an I/O or transport error when the listener is not loopback or serving fails.
pub async fn serve<F>(
    listener: TcpListener,
    shutdown: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    F: Future<Output = ()> + Send + 'static,
{
    let address = listener.local_addr()?;
    if !address.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "OCLA gRPC v1 requires a loopback listener",
        )
        .into());
    }

    let verifier = OclaVerifierServer::new(OclaVerifierService::default())
        .max_decoding_message_size(MAX_GRPC_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_GRPC_RESPONSE_BYTES);
    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<OclaVerifierServer<OclaVerifierService>>()
        .await;
    let health_service = health_service
        .max_decoding_message_size(MAX_GRPC_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_GRPC_RESPONSE_BYTES);

    tonic::transport::Server::builder()
        .timeout(REQUEST_TIMEOUT)
        .concurrency_limit_per_connection(MAX_IN_FLIGHT_PER_CONNECTION)
        .add_service(health_service)
        .add_service(verifier)
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown)
        .await?;
    Ok(())
}

fn verify_token(message: CanonicalTokenEnvelope) -> VerificationResult {
    let wire = match token_wire(message) {
        Ok(wire) => wire,
        Err(rejection) => return rejected(rejection),
    };
    match serde_json::to_vec(&wire)
        .map_err(|_| OclaWireError::Malformed("serialization rejected".into()))
        .and_then(|bytes| decode_canonical_token_envelope(&bytes))
    {
        Ok(_) => accepted(),
        Err(error) => rejected(map_wire_error(&error)),
    }
}

fn verify_agent(message: AgentEnvelope) -> VerificationResult {
    let wire = match agent_wire(message) {
        Ok(wire) => wire,
        Err(rejection) => return rejected(rejection),
    };
    match serde_json::to_vec(&wire)
        .map_err(|_| OclaWireError::Malformed("serialization rejected".into()))
        .and_then(|bytes| decode_agent_envelope(&bytes))
    {
        Ok(envelope) => match verify_agent_gateway_admissibility(&envelope) {
            Ok(()) => accepted(),
            Err(OclaGatewayAdmissibilityError::SelfRelay) => rejected(Rejection::SelfRelay),
            Err(_) => rejected(Rejection::InvalidInvariant),
        },
        Err(error) => rejected(map_wire_error(&error)),
    }
}

fn token_wire(message: CanonicalTokenEnvelope) -> Result<CanonicalTokenEnvelopeV1, Rejection> {
    let schema_version =
        u16::try_from(message.schema_version).map_err(|_| Rejection::UnsupportedVersion)?;
    let context = context_wire(message.context)?;
    let balance = message.token_balance.ok_or(Rejection::InvalidInvariant)?;
    let surface = match Surface::try_from(message.surface).ok() {
        Some(Surface::Mcp) => TokenEnvelopeSurface::Mcp,
        Some(Surface::Proxy) => TokenEnvelopeSurface::Proxy,
        Some(Surface::Shell) => TokenEnvelopeSurface::Shell,
        Some(Surface::Agent) => TokenEnvelopeSurface::Agent,
        _ => return Err(Rejection::InvalidInvariant),
    };
    let direction = match Direction::try_from(message.direction).ok() {
        Some(Direction::Input) => TokenFlowDirection::Input,
        Some(Direction::Output) => TokenFlowDirection::Output,
        _ => return Err(Rejection::InvalidInvariant),
    };
    Ok(CanonicalTokenEnvelopeV1 {
        schema_version,
        context,
        surface,
        direction,
        provider: message.provider,
        model: message.model,
        token_balance: TokenBalanceV1 {
            original_tokens: balance.original_tokens,
            materialized_tokens: balance.materialized_tokens,
            delivered_tokens: balance.delivered_tokens,
            provider_billed_tokens: balance.provider_billed_tokens,
        },
        route_ref: message.route_ref,
        policy_ref: message.policy_ref,
        idempotency_key: message.idempotency_key,
    })
}

fn agent_wire(message: AgentEnvelope) -> Result<AgentEnvelopeV1, Rejection> {
    Ok(AgentEnvelopeV1 {
        schema_version: u16::try_from(message.schema_version)
            .map_err(|_| Rejection::UnsupportedVersion)?,
        relay_id: message.relay_id,
        context: context_wire(message.context)?,
        from_agent_id: message.from_agent_id,
        to_agent_id: message.to_agent_id,
        capsule_ref: message.capsule_ref,
        budget_tokens: message.budget_tokens,
    })
}

fn context_wire(context: Option<RequestContext>) -> Result<OclaRequestContext, Rejection> {
    let context = context.ok_or(Rejection::InvalidInvariant)?;
    Ok(OclaRequestContext {
        request_id: context.request_id,
        session_id: context.session_id,
        agent_id: context.agent_id,
        content_ref: context.content_ref,
        tenant_id: context.tenant_id,
    })
}

fn accepted() -> VerificationResult {
    VerificationResult {
        accepted: true,
        rejection: Rejection::None.into(),
        api_version: OCLA_API_VERSION.into(),
    }
}

fn rejected(rejection: Rejection) -> VerificationResult {
    VerificationResult {
        accepted: false,
        rejection: rejection.into(),
        api_version: OCLA_API_VERSION.into(),
    }
}

fn map_wire_error(error: &OclaWireError) -> Rejection {
    match error {
        OclaWireError::Oversize { .. } => Rejection::Oversize,
        OclaWireError::Malformed(_) => Rejection::Malformed,
        OclaWireError::UnsupportedVersion { .. } => Rejection::UnsupportedVersion,
        OclaWireError::NonCanonical => Rejection::NonCanonical,
        _ => Rejection::InvalidInvariant,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_services_share_process_wide_verifier_capacity() {
        let first = OclaVerifierService::default();
        let second = OclaVerifierService::default();
        assert!(Arc::ptr_eq(&first.permits, &second.permits));
        assert_eq!(first.permits.available_permits(), MAX_GLOBAL_IN_FLIGHT);
    }

    #[tokio::test]
    async fn shared_capacity_saturates_without_queueing_and_recovers() {
        let permits = Arc::new(Semaphore::new(1));
        let first = OclaVerifierService::with_permits(Arc::clone(&permits));
        let second = OclaVerifierService::with_permits(permits);
        let held = first.permit().unwrap();

        let error = second
            .verify_token_envelope(Request::new(CanonicalTokenEnvelope::default()))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::ResourceExhausted);
        assert_eq!(error.message(), "OCLA verifier capacity exhausted");

        drop(held);
        let result = second
            .verify_token_envelope(Request::new(CanonicalTokenEnvelope::default()))
            .await
            .unwrap()
            .into_inner();
        assert!(!result.accepted);
        assert_eq!(result.rejection, Rejection::InvalidInvariant as i32);
    }
}
