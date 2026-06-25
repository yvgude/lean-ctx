//! `OpenAPI` 3.0 document for the public `/v1` surface, generated from a single
//! in-code endpoint inventory ([`endpoints`]). The HTTP route that serves it
//! lives in `http_server`; the SSOT lives here so it stays compiled and
//! drift-tested without the `http-server` feature.
//!
//! Scope: the **public, stable** surface only — the same set documented in
//! `docs/contracts/http-mcp-contract-v1.md`. Internal/experimental routes
//! (agent registry, A2A, `.well-known`, shutdown) are intentionally excluded
//! from the published spec. `tests/openapi_contract_up_to_date.rs` binds this
//! inventory to that contract's Endpoints table.

use serde_json::{Map, Value, json};

/// One documented, public endpoint of the `/v1` surface.
pub struct EndpointDoc {
    pub method: &'static str,
    pub path: &'static str,
    /// `none` or a description containing `bearer` (drives the security block).
    pub auth: &'static str,
    pub summary: &'static str,
}

/// The public endpoint inventory — the single source of truth for the `OpenAPI`
/// document and the contract-doc drift test.
#[must_use]
pub fn endpoints() -> Vec<EndpointDoc> {
    vec![
        EndpointDoc {
            method: "GET",
            path: "/health",
            auth: "none",
            summary: "Liveness probe",
        },
        EndpointDoc {
            method: "GET",
            path: "/v1/manifest",
            auth: "bearer",
            summary: "Full MCP manifest",
        },
        EndpointDoc {
            method: "GET",
            path: "/v1/capabilities",
            auth: "bearer",
            summary: "Instance capabilities discovery",
        },
        EndpointDoc {
            method: "GET",
            path: "/v1/openapi.json",
            auth: "bearer",
            summary: "OpenAPI 3.0 spec for this surface",
        },
        EndpointDoc {
            method: "GET",
            path: "/v1/tools",
            auth: "bearer",
            summary: "Paginated tool list",
        },
        EndpointDoc {
            method: "POST",
            path: "/v1/tools/call",
            auth: "bearer",
            summary: "Execute a single tool",
        },
        EndpointDoc {
            method: "GET",
            path: "/v1/events",
            auth: "bearer",
            summary: "SSE stream with replay",
        },
        EndpointDoc {
            method: "GET",
            path: "/v1/context/summary",
            auth: "bearer",
            summary: "Materialized workspace/channel summary",
        },
        EndpointDoc {
            method: "GET",
            path: "/v1/events/search",
            auth: "bearer",
            summary: "Full-text search over event payloads",
        },
        EndpointDoc {
            method: "GET",
            path: "/v1/events/lineage",
            auth: "bearer",
            summary: "Causal lineage chain for an event",
        },
        EndpointDoc {
            method: "GET",
            path: "/v1/metrics",
            auth: "bearer",
            summary: "JSON metrics snapshot (slo block; ?format=prometheus for text exposition)",
        },
    ]
}

/// Build the `OpenAPI` 3.0.3 document for this build.
#[must_use]
pub fn openapi_value() -> Value {
    let mut paths: Map<String, Value> = Map::new();

    for e in endpoints() {
        let security = if e.auth.contains("bearer") {
            json!([{ "bearerAuth": [] }])
        } else {
            json!([])
        };
        let operation = json!({
            "summary": e.summary,
            "security": security,
            "responses": { "200": { "description": "OK" } },
        });

        let entry = paths
            .entry(e.path.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(obj) = entry.as_object_mut() {
            obj.insert(e.method.to_lowercase(), operation);
        }
    }

    json!({
        "openapi": "3.0.3",
        "info": {
            "title": "lean-ctx HTTP/MCP API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Public /v1 surface of the lean-ctx Context OS. \
                            Full contract: docs/contracts/http-mcp-contract-v1.md. \
                            Discover instance features at GET /v1/capabilities.",
        },
        "components": {
            "securitySchemes": {
                "bearerAuth": { "type": "http", "scheme": "bearer" }
            }
        },
        "paths": Value::Object(paths),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_is_well_formed() {
        let v = openapi_value();
        assert_eq!(v["openapi"], json!("3.0.3"));
        assert!(v["paths"].as_object().is_some_and(|p| !p.is_empty()));
        assert!(v["components"]["securitySchemes"]["bearerAuth"].is_object());
    }

    #[test]
    fn every_endpoint_is_present() {
        let v = openapi_value();
        let paths = v["paths"].as_object().expect("paths object");
        for e in endpoints() {
            let op = &paths[e.path][e.method.to_lowercase()];
            assert!(
                op.is_object(),
                "missing {} {} in OpenAPI paths",
                e.method,
                e.path
            );
        }
    }

    #[test]
    fn bearer_endpoints_require_security() {
        let v = openapi_value();
        let paths = v["paths"].as_object().unwrap();
        let manifest = &paths["/v1/manifest"]["get"];
        assert_eq!(manifest["security"], json!([{ "bearerAuth": [] }]));
        let health = &paths["/health"]["get"];
        assert_eq!(health["security"], json!([]));
    }
}
