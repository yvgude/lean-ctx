// SPDX-License-Identifier: Apache-2.0

//! Opt-in v2 materialization: the unchanged v1 exchange, presented under a
//! presenter capability (`core::archive::presented`, "Presenters").
//!
//! The capability proves that a later request comes from a holder of the
//! capability these sources were presented under. It names no agent, attests
//! no identity and authorizes nothing: every later use re-checks project,
//! role, policy identity, freshness and the admission of each source.
//!
//! This transport writes no decision receipt, so the presenter appears in no
//! receipt here; the response carries its content-free reference, and the
//! lineage of the delivered text records the same reference.

use std::path::Path;

use lean_ctx_protocol::{
    EngineContextSourceMaterializationRequestV2, EngineContextSourceMaterializationResponseV2,
    EnginePresenterGrantV1, EnginePresenterRequestV1, ProtocolReference,
};

use crate::core::archive::presented::{self, PresenterCapability};

/// The one refusal for everything about a capability on this transport:
/// shown where none belongs, missing where one is needed, unreadable,
/// unknown, expired, revoked, or not issuable.
pub(crate) const CAPABILITY_REFUSED: &str = "presenter_capability_refused";

/// Materialize `request` as its v1 payload and record the presentations
/// under a presenter capability.
///
/// - `issue`: a capability is minted, the materialization runs under it, and
///   only a successful response carries it. A materialization that fails
///   leaves a revoked presenter and returns no capability.
/// - `renew`: `shown` is the capability from the host's capability file; it
///   must be an issued one that is neither expired nor revoked. It is never
///   returned again.
pub(crate) fn materialize_v2(
    root: &Path,
    request: &EngineContextSourceMaterializationRequestV2,
    shown: Option<PresenterCapability>,
) -> Result<EngineContextSourceMaterializationResponseV2, &'static str> {
    request
        .validate_payload()
        .map_err(|_| "invalid_source_materialization_request")?;
    let (capability, expires_at, fresh) = match (request.presenter, shown) {
        (EnginePresenterRequestV1::Issue { ttl_seconds }, None) => {
            let issued = presented::issue(ttl_seconds).ok_or(CAPABILITY_REFUSED)?;
            (issued.capability, issued.expires_at, true)
        }
        (EnginePresenterRequestV1::Renew, Some(capability)) => {
            let expires_at =
                presented::issued_lifetime_end(&capability).ok_or(CAPABILITY_REFUSED)?;
            (capability, expires_at, false)
        }
        _ => return Err(CAPABILITY_REFUSED),
    };
    let response = presented::with_presenter(Some(capability.clone()), || {
        super::materialize(root, &request.materialization)
    })
    .and_then(|materialization| {
        let response = EngineContextSourceMaterializationResponseV2 {
            schema_version: 2,
            materialization,
            presenter: EnginePresenterGrantV1 {
                presenter_ref: ProtocolReference::new(capability.reference())
                    .map_err(|_| "internal")?,
                expires_at_unix: u64::try_from(expires_at).map_err(|_| "internal")?,
                capability: fresh.then(|| String::clone(&capability.expose())),
            },
        };
        response
            .validate()
            .map_err(|_| "source_materialization_invalid")?;
        Ok(response)
    });
    if response.is_err() && fresh {
        // Never returned to anyone: it must not stay usable either.
        presented::revoke(&capability);
    }
    response
}
