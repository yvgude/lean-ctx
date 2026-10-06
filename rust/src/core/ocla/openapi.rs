//! OpenAPI 3.1 projection of the public OCLA wire contract.

use serde_json::{Value, json};

use super::{
    OCLA_API_VERSION,
    wire::{agent_envelope_schema, canonical_envelope_schema},
};

fn schema_ref(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}

fn error_response(description: &str) -> Value {
    json!({
        "description": description,
        "content": {
            "application/json": {"schema": schema_ref("OclaError")}
        }
    })
}

fn capability_schema() -> Value {
    json!({
        "type": "object",
        "required": ["kind", "api_version", "status", "limits"],
        "properties": {
            "kind": {
                "type": "string",
                "enum": [
                    "observation_hook", "usage_sink", "metrics_exporter",
                    "savings_ledger", "intent_classifier", "outcome_tracker",
                    "compression_provider", "response_optimizer",
                    "efficiency_analyzer", "config_tuner", "experiment_runner",
                    "connector_scheduler", "agent_gateway"
                ]
            },
            "api_version": {"type": "string"},
            "status": {
                "type": "string",
                "enum": ["available", "degraded", "unavailable"]
            },
            "limits": {
                "type": "object",
                "additionalProperties": {"type": "integer", "minimum": 0}
            }
        }
    })
}

fn savings_event_schema() -> Value {
    let required = json!([
        "ts",
        "tool",
        "mechanism",
        "model_id",
        "tokenizer",
        "baseline_tokens",
        "actual_tokens",
        "saved_tokens",
        "bounce_adjustment",
        "unit_price_per_m_usd",
        "saved_usd",
        "repo_hash",
        "agent_id",
        "prev_hash",
        "entry_hash",
        "version"
    ]);
    let mut properties = serde_json::Map::new();
    for fields in [
        json!({
            "ts": {"type": "string"},
            "tool": {"type": "string"},
            "mechanism": {"type": "string", "enum": ["compression", "routing", "caching"]},
            "model_id": {"type": "string"},
            "tokenizer": {"type": "string"},
            "baseline_tokens": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
            "actual_tokens": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
            "saved_tokens": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
            "bounce_adjustment": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
            "unit_price_per_m_usd": {"type": "number"},
            "saved_usd": {"type": "number"},
            "repo_hash": {"type": "string"},
            "agent_id": {"type": "string"},
            "trace_id": {"type": ["string", "null"]},
            "prev_hash": {"type": "string"},
            "entry_hash": {"type": "string"},
            "version": {"type": "string"},
        }),
        json!({
            "intent_tag": {"type": ["string", "null"]},
            "outcome": {"type": ["string", "null"]},
            "model_original": {"type": ["string", "null"]},
            "model_routed": {"type": ["string", "null"]},
            "routing_savings": {"type": ["integer", "null"], "minimum": 0, "maximum": u64::MAX},
            "response_original_tokens": {"type": ["integer", "null"], "minimum": 0, "maximum": u64::MAX},
            "response_delivered_tokens": {"type": ["integer", "null"], "minimum": 0, "maximum": u64::MAX},
            "agent_chain_id": {"type": ["string", "null"]},
            "chain_depth": {"type": ["integer", "null"], "minimum": 0, "maximum": u8::MAX},
            "measurement_method": {
                "type": ["string", "null"],
                "enum": ["direct_count", "holdout", "baseline_estimate", "provider_reconciled", "unknown", null]
            },
            "evidence_class": {
                "type": ["string", "null"],
                "enum": ["measured", "approximated", "statistical", "declared", "unclassified", null]
            },
            "confidence": {"type": ["number", "null"], "minimum": 0, "maximum": 1},
            "quality_signal": {"type": ["string", "null"]},
            "attribution_group": {"type": ["string", "null"]},
            "attribution_id": {"type": ["string", "null"]},
            "baseline_ref": {"type": ["string", "null"]},
            "price_version": {"type": ["string", "null"]},
        }),
        json!({
            "customer_approval": {
                "type": ["string", "null"],
                "enum": ["pending", "approved", "disputed", "superseded", null]
            },
            "settlement_status": {
                "type": ["string", "null"],
                "enum": ["ineligible", "eligible", "settled", "reversed", null]
            }
        }),
    ] {
        properties.extend(fields.as_object().expect("event fields object").clone());
    }
    json!({"type": "object", "required": required, "properties": properties})
}

fn ledger_summary_schema() -> Value {
    json!({
        "type": "object",
        "required": [
            "total_events", "saved_tokens", "saved_usd", "bounce_tokens",
            "bounce_events", "tokenizers", "by_model", "by_day",
            "by_tool", "by_mechanism", "net_saved_tokens"
        ],
        "properties": {
            "total_events": {"type": "integer", "minimum": 0},
            "saved_tokens": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
            "saved_usd": {"type": "number"},
            "bounce_tokens": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
            "bounce_events": {"type": "integer", "minimum": 0},
            "tokenizers": {"type": "array", "items": {"type": "string"}},
            "by_model": {"type": "array", "items": {"$ref": "#/components/schemas/LedgerModelTotals"}},
            "by_day": {"type": "array", "items": {"$ref": "#/components/schemas/LedgerDayTotals"}},
            "by_tool": {"type": "array", "items": {"$ref": "#/components/schemas/LedgerToolTotals"}},
            "by_mechanism": {"type": "array", "items": {"$ref": "#/components/schemas/LedgerMechanismTotals"}},
            "net_saved_tokens": {"type": "integer", "minimum": 0, "maximum": u64::MAX}
        }
    })
}

fn idempotency_key_parameter() -> Value {
    json!({
        "name": "Idempotency-Key",
        "in": "header",
        "required": true,
        "description": "Client-supplied key used to make envelope submission idempotent.",
        "schema": {"type": "string", "minLength": 1}
    })
}

fn agent_schema() -> Value {
    json!({
        "type": "object",
        "required": ["agent_id", "status"],
        "properties": {
            "agent_id": {"type": "string", "minLength": 1},
            "status": {"type": "string", "enum": ["active", "idle", "offline"]},
            "last_seen": {"type": ["string", "null"]},
            "capabilities": {"type": "array", "items": {"type": "string"}}
        }
    })
}

fn agents_response_schema() -> Value {
    json!({
        "type": "object",
        "required": ["api_version", "agents"],
        "properties": {
            "api_version": {"const": OCLA_API_VERSION},
            "agents": {"type": "array", "items": schema_ref("OclaAgent")}
        }
    })
}

fn metric_schema() -> Value {
    json!({
        "type": "object",
        "required": ["name", "value_milli"],
        "properties": {
            "name": {"type": "string", "minLength": 1},
            "value_milli": {"type": "integer"},
            "dimensions": {"type": "object", "additionalProperties": {"type": "string"}}
        }
    })
}

fn metrics_response_schema() -> Value {
    json!({
        "type": "object",
        "required": ["api_version", "metrics"],
        "properties": {
            "api_version": {"const": OCLA_API_VERSION},
            "metrics": {"type": "array", "items": schema_ref("OclaMetric")}
        }
    })
}

fn dlq_entry_schema() -> Value {
    json!({
        "type": "object",
        "required": [
            "id", "tenant_id", "project_id", "delivery", "original_message",
            "target_agent", "error", "attempts",
            "first_failed_at", "last_failed_at"
        ],
        "properties": {
            "id": {"type": "string", "minLength": 1},
            "tenant_id": {"type": "string", "minLength": 1},
            "project_id": {"type": "string", "minLength": 1},
            "delivery": {
                "oneOf": [
                    {"const": "local_agent_bus"},
                    {
                        "type": "object",
                        "required": ["remote_http"],
                        "properties": {
                            "remote_http": {
                                "type": "object",
                                "required": ["endpoint_url"],
                                "properties": {"endpoint_url": {"type": "string", "format": "uri"}}
                            }
                        }
                    }
                ]
            },
            "original_message": {"type": "string"},
            "target_agent": {"type": "string"},
            "error": {"type": "string"},
            "attempts": {"type": "integer", "minimum": 0, "maximum": 255},
            "first_failed_at": {"type": "string"},
            "last_failed_at": {"type": "string"}
        }
    })
}

fn dlq_stats_schema() -> Value {
    json!({
        "type": "object",
        "required": ["total", "max_scope_depth", "oldest_age_seconds", "by_target_agent"],
        "properties": {
            "total": {"type": "integer", "minimum": 0},
            "max_scope_depth": {"type": "integer", "minimum": 0, "maximum": 1000},
            "oldest_age_seconds": {"type": "integer", "minimum": 0},
            "by_target_agent": {"type": "object", "additionalProperties": {"type": "integer", "minimum": 0}}
        }
    })
}

fn dlq_response_schema() -> Value {
    json!({
        "type": "object",
        "required": ["dead_letters", "stats"],
        "properties": {
            "dead_letters": {"type": "array", "items": schema_ref("DeadLetter")},
            "stats": schema_ref("DlqStats")
        }
    })
}

fn dlq_retry_response_schema() -> Value {
    json!({
        "type": "object",
        "required": ["id", "retried"],
        "properties": {"id": {"type": "string"}, "retried": {"const": true}}
    })
}

fn health_status_schema() -> Value {
    json!({
        "oneOf": [
            {"const": "healthy"},
            {
                "type": "object",
                "required": ["degraded"],
                "properties": {"degraded": {"type": "string"}}
            },
            {
                "type": "object",
                "required": ["unhealthy"],
                "properties": {"unhealthy": {"type": "string"}}
            }
        ]
    })
}

fn health_component_schema() -> Value {
    json!({
        "type": "object",
        "required": ["name", "status", "latency_ms"],
        "properties": {
            "name": {"type": "string"},
            "status": health_status_schema(),
            "latency_ms": {"type": ["integer", "null"], "minimum": 0},
            "details": {
                "type": "object",
                "required": ["total", "oldest_age_secs"],
                "properties": {
                    "total": {"type": "integer", "minimum": 0},
                    "oldest_age_secs": {"type": "integer", "minimum": 0}
                }
            }
        }
    })
}

fn health_response_schema() -> Value {
    json!({
        "type": "object",
        "required": ["overall", "components", "uptime_seconds", "version"],
        "properties": {
            "overall": health_status_schema(),
            "components": {"type": "array", "items": health_component_schema()},
            "uptime_seconds": {"type": "integer", "minimum": 0},
            "version": {"type": "string", "const": OCLA_API_VERSION}
        }
    })
}

fn envelope_batch_response_schema() -> Value {
    json!({
        "type": "object",
        "required": ["api_version", "accepted", "rejected", "envelopes"],
        "properties": {
            "api_version": {"const": OCLA_API_VERSION},
            "accepted": {"type": "integer", "minimum": 0},
            "rejected": {"type": "integer", "minimum": 0},
            "envelopes": {
                "type": "array",
                "items": {
                    "oneOf": [
                        schema_ref("CanonicalTokenEnvelopeV1"),
                        schema_ref("AgentEnvelopeV1")
                    ]
                }
            }
        }
    })
}

fn budget_request_schema() -> Value {
    json!({
        "type": "object",
        "required": ["scope", "max_tokens_per_day", "max_usd_per_day"],
        "properties": {
            "scope": {
                "type": "string",
                "pattern": "^(org|team|user):.+$",
                "minLength": 5
            },
            "max_tokens_per_day": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
            "max_usd_per_day": {"type": "number", "minimum": 0}
        }
    })
}

fn budget_response_schema() -> Value {
    json!({
        "type": "object",
        "required": [
            "scope", "max_tokens_per_day", "max_usd_per_day",
            "consumed_tokens", "consumed_usd"
        ],
        "properties": {
            "scope": {"type": "string"},
            "max_tokens_per_day": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
            "max_usd_per_day": {"type": "number", "minimum": 0},
            "consumed_tokens": {"type": "integer", "minimum": 0, "maximum": u64::MAX},
            "consumed_usd": {"type": "number", "minimum": 0}
        }
    })
}

fn budget_scope_parameter() -> Value {
    json!({
        "name": "scope",
        "in": "path",
        "required": true,
        "schema": {"type": "string", "pattern": "^(org|team|user):.+$"}
    })
}

/// Builds the CI-visible OpenAPI 3.1 document for the OCLA OSS surface.
#[must_use]
pub fn ocla_openapi_spec() -> Value {
    let envelope_request = json!({
        "oneOf": [
            schema_ref("CanonicalTokenEnvelopeV1"),
            schema_ref("AgentEnvelopeV1")
        ]
    });

    let mut spec = json!({
        "openapi": "3.1.0",
        "jsonSchemaDialect": "https://json-schema.org/draft/2020-12/schema",
        "info": {
            "title": "LeanCTX OCLA API",
            "description": "Provider-neutral Open Context & Token Lifecycle Architecture contract.",
            "version": OCLA_API_VERSION,
            "license": {"name": "Apache-2.0"}
        },
        "servers": [{"url": "/"}],
        "paths": {
            "/ocla/v1/health": {
                "get": {
                    "operationId": "oclaHealth",
                    "summary": "Check OCLA availability",
                    "responses": {
                        "200": {"description": "OCLA is available", "content": {"application/json": {"schema": schema_ref("HealthResponse")}}},
                        "503": error_response("OCLA is unavailable")
                    }
                }
            },
            "/ocla/v1/capabilities": {
                "get": {
                    "operationId": "oclaCapabilities",
                    "summary": "List registered OCLA capabilities",
                    "responses": {
                        "200": {"description": "Registered capabilities", "content": {"application/json": {"schema": schema_ref("CapabilitiesResponse")}}},
                        "503": error_response("Capability registry is unavailable")
                    }
                }
            },
            "/ocla/v1/agents": {
                "get": {
                    "operationId": "oclaAgents",
                    "summary": "List connected OCLA agents",
                    "responses": {
                        "200": {"description": "Connected agents", "content": {"application/json": {"schema": schema_ref("AgentsResponse")}}},
                        "503": error_response("Agent registry is unavailable")
                    }
                }
            },
            "/ocla/v1/metrics": {
                "get": {
                    "operationId": "oclaMetrics",
                    "summary": "Read OCLA metrics",
                    "responses": {
                        "200": {"description": "Current OCLA metrics", "content": {"application/json": {"schema": schema_ref("MetricsResponse")}}},
                        "503": error_response("Metrics exporter is unavailable")
                    }
                }
            },
            "/ocla/v1/envelope": {
                "post": {
                    "operationId": "submitOclaEnvelope",
                    "summary": "Validate and accept a payload-free OCLA envelope",
                    "parameters": [idempotency_key_parameter()],
                    "requestBody": {"required": true, "content": {"application/json": {"schema": envelope_request}}},
                    "responses": {
                        "200": {"description": "Envelope accepted", "content": {"application/json": {"schema": envelope_request}}},
                        "400": error_response("Envelope failed validation")
                    }
                }
            },
            "/ocla/v1/envelope/batch": {
                "post": {
                    "operationId": "submitOclaEnvelopeBatch",
                    "summary": "Validate and accept multiple OCLA envelopes",
                    "parameters": [idempotency_key_parameter()],
                    "requestBody": {
                        "required": true,
                        "content": {"application/json": {"schema": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": 1000,
                            "items": envelope_request
                        }}}
                    },
                    "responses": {
                        "200": {"description": "Envelope batch accepted", "content": {"application/json": {"schema": schema_ref("EnvelopeBatchResponse")}}},
                        "400": error_response("One or more envelopes failed validation")
                    }
                }
            },
            "/ocla/v1/ledger/summary": {
                "get": {
                    "operationId": "getOclaLedger",
                    "summary": "Read the verified local savings ledger",
                    "parameters": [
                        {"name": "limit", "in": "query", "required": false, "schema": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 100}},
                        {"name": "mechanism", "in": "query", "required": false, "schema": {"type": "string", "enum": ["compression", "routing", "caching"]}}
                    ],
                    "responses": {
                        "200": {"description": "Verified ledger snapshot", "content": {"application/json": {"schema": schema_ref("LedgerResponse")}}},
                        "503": error_response("Ledger is unavailable or invalid")
                    }
                }
            },
            "/ocla/v1/budget": {
                "post": {
                    "operationId": "setOclaBudget",
                    "summary": "Set a daily OCLA budget limit",
                    "requestBody": {
                        "required": true,
                        "content": {"application/json": {"schema": schema_ref("BudgetRequest")}}
                    },
                    "responses": {
                        "200": {"description": "Budget limit set", "content": {"application/json": {"schema": schema_ref("BudgetResponse")}}},
                        "400": error_response("Budget request is invalid")
                    }
                }
            },
            "/ocla/v1/budget/{scope}": {
                "get": {
                    "operationId": "getOclaBudget",
                    "summary": "Read a daily OCLA budget limit and consumption",
                    "parameters": [budget_scope_parameter()],
                    "responses": {
                        "200": {"description": "Budget limit and consumption", "content": {"application/json": {"schema": schema_ref("BudgetResponse")}}},
                        "400": error_response("Budget scope is invalid"),
                        "404": error_response("Budget limit was not found")
                    }
                },
                "delete": {
                    "operationId": "deleteOclaBudget",
                    "summary": "Remove a daily OCLA budget limit",
                    "parameters": [budget_scope_parameter()],
                    "responses": {
                        "204": {"description": "Budget limit removed"},
                        "400": error_response("Budget scope is invalid"),
                        "404": error_response("Budget limit was not found")
                    }
                }
            },
            "/ocla/v1/dlq": {
                "get": {
                    "operationId": "getOclaDlq",
                    "summary": "List dead letters for the authenticated server-bound scope",
                    "security": [{"bearerAuth": []}],
                    "responses": {
                        "200": {"description": "Dead-letter queue snapshot", "content": {"application/json": {"schema": schema_ref("DlqResponse")}}},
                        "401": error_response("Authentication is required"),
                        "503": error_response("Dead-letter store is unavailable")
                    }
                }
            },
            "/ocla/v1/dlq/{id}/retry": {
                "post": {
                    "operationId": "retryOclaDeadLetter",
                    "summary": "Retry a dead letter in the authenticated server-bound scope",
                    "security": [{"bearerAuth": []}],
                    "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "string", "minLength": 1}}],
                    "responses": {
                        "200": {"description": "Dead letter retried", "content": {"application/json": {"schema": schema_ref("DlqRetryResponse")}}},
                        "400": error_response("Dead letter retry failed"),
                        "401": error_response("Authentication is required"),
                        "503": error_response("Dead-letter store is unavailable")
                    }
                }
            },
            "/ocla/v1/dlq/{id}": {
                "delete": {
                    "operationId": "deleteOclaDeadLetter",
                    "summary": "Remove a dead letter in the authenticated server-bound scope",
                    "security": [{"bearerAuth": []}],
                    "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "string", "minLength": 1}}],
                    "responses": {
                        "204": {"description": "Dead letter removed"},
                        "401": error_response("Authentication is required"),
                        "404": error_response("Dead letter was not found"),
                        "503": error_response("Dead-letter store is unavailable")
                    }
                }
            },

        },
        "components": {
            "securitySchemes": {
                "bearerAuth": {"type": "http", "scheme": "bearer"}
            },
            "schemas": {
                "CanonicalTokenEnvelopeV1": canonical_envelope_schema(),
                "AgentEnvelopeV1": agent_envelope_schema(),
                "AgentsResponse": agents_response_schema(),
                "OclaAgent": agent_schema(),
                "MetricsResponse": metrics_response_schema(),
                "OclaMetric": metric_schema(),
                "EnvelopeBatchResponse": envelope_batch_response_schema(),
                "BudgetRequest": budget_request_schema(),
                "BudgetResponse": budget_response_schema(),
                "DeadLetter": dlq_entry_schema(),
                "DlqStats": dlq_stats_schema(),
                "DlqResponse": dlq_response_schema(),
                "DlqRetryResponse": dlq_retry_response_schema(),
                "HealthResponse": health_response_schema(),

                "CapabilitiesResponse": {
                    "type": "object",
                    "required": ["api_version", "capabilities"],
                    "properties": {"api_version": {"const": OCLA_API_VERSION}, "capabilities": {"type": "array", "items": schema_ref("OclaCapability")}}
                },
                "OclaCapability": capability_schema(),
                "OclaError": {
                    "type": "object",
                    "required": ["error"],
                    "properties": {"error": {"type": "string"}, "code": {"type": "string"}}
                },
                "SavingsEvent": savings_event_schema(),
                "LedgerModelTotals": {"type": "array", "prefixItems": [{"type": "string"}, {"type": "integer", "minimum": 0}, {"type": "number"}], "minItems": 3, "maxItems": 3},
                "LedgerDayTotals": {"type": "array", "prefixItems": [{"type": "string", "pattern": "^\\d{4}-\\d{2}-\\d{2}$"}, {"type": "integer", "minimum": 0}, {"type": "number"}, {"type": "integer", "minimum": 0}], "minItems": 3, "maxItems": 4},
                "LedgerToolTotals": {"type": "array", "prefixItems": [{"type": "string"}, {"type": "integer", "minimum": 0}], "minItems": 2, "maxItems": 2},
                "LedgerMechanismTotals": {"type": "array", "prefixItems": [{"type": "string"}, {"type": "integer", "minimum": 0}, {"type": "number"}], "minItems": 3, "maxItems": 3},
                "LedgerSummary": ledger_summary_schema(),
                "LedgerResponse": {
                    "type": "object",
                    "required": ["api_version", "verified", "events", "summary"],
                    "properties": {
                        "api_version": {"const": OCLA_API_VERSION},
                        "verified": {"type": "boolean"},
                        "events": {"type": "array", "items": schema_ref("SavingsEvent")},
                        "summary": schema_ref("LedgerSummary")
                    }
                }
            }
        }
    });
    if let Some(paths) = spec["paths"].as_object_mut() {
        paths.insert("/ocla/v1/capsule".into(), capsule_register_path());
        paths.insert("/ocla/v1/capsule/{ref}".into(), capsule_resolve_path());
        paths.insert("/ocla/v1/capsule/{ref}/fork".into(), capsule_fork_path());
    }
    if let Some(schemas) = spec["components"]["schemas"].as_object_mut() {
        schemas.insert("CapsuleRegisterResponse".into(), json!({"type": "object", "required": ["capsule_ref"], "properties": {"capsule_ref": {"type": "string", "pattern": "^capsule:[0-9a-f]{64}$"}}}));
        schemas.insert("CapsuleResolveResponse".into(), json!({"type": "object", "required": ["capsule_ref", "data"], "properties": {"capsule_ref": {"type": "string"}, "data": {"type": "string"}}}));
        schemas.insert("CapsuleForkRequest".into(), json!({"type": "object", "required": ["budget_tokens"], "properties": {"budget_tokens": {"type": "integer", "minimum": 0}}}));
    }
    spec
}

fn capsule_register_path() -> Value {
    json!({"post": {"operationId": "registerCapsule", "summary": "Register a new content-addressed capsule", "requestBody": {"required": true, "content": {"text/plain": {"schema": {"type": "string"}}}}, "responses": {"201": {"description": "Capsule registered", "content": {"application/json": {"schema": schema_ref("CapsuleRegisterResponse")}}}}}})
}

fn capsule_resolve_path() -> Value {
    json!({"get": {"operationId": "resolveCapsule", "summary": "Resolve a capsule to its materialized data", "parameters": [{"name": "ref", "in": "path", "required": true, "schema": {"type": "string"}}], "responses": {"200": {"description": "Capsule data", "content": {"application/json": {"schema": schema_ref("CapsuleResolveResponse")}}}, "404": error_response("Capsule not found")}}})
}

fn capsule_fork_path() -> Value {
    json!({"post": {"operationId": "forkCapsule", "summary": "Create a CoW fork of an existing capsule", "parameters": [{"name": "ref", "in": "path", "required": true, "schema": {"type": "string"}}], "requestBody": {"required": true, "content": {"application/json": {"schema": schema_ref("CapsuleForkRequest")}}}, "responses": {"201": {"description": "Fork created", "content": {"application/json": {"schema": schema_ref("CapsuleRegisterResponse")}}}, "404": error_response("Parent capsule not found")}}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_snapshot_matches_checked_in_contract() {
        if std::env::var_os("LEANCTX_UPDATE_OCLA_OPENAPI_SNAPSHOT").is_some() {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/ocla_openapi_snapshot.json");
            let json = serde_json::to_string(&ocla_openapi_spec())
                .expect("serialize OCLA OpenAPI snapshot");
            std::fs::write(path, json + "\n").expect("write OCLA OpenAPI snapshot");
            return;
        }
        let expected = include_str!("../../../tests/fixtures/ocla_openapi_snapshot.json").trim();
        let actual =
            serde_json::to_string(&ocla_openapi_spec()).expect("serialize OCLA OpenAPI snapshot");
        assert_eq!(actual, expected);
    }

    #[test]
    fn openapi_exposes_all_ocla_endpoints_and_wire_schemas() {
        let spec = ocla_openapi_spec();
        let paths = spec["paths"].as_object().expect("paths object");
        assert!(paths.contains_key("/ocla/v1/health"));
        assert!(paths.contains_key("/ocla/v1/capabilities"));
        assert!(paths.contains_key("/ocla/v1/agents"));
        assert!(paths.contains_key("/ocla/v1/metrics"));
        assert!(paths.contains_key("/ocla/v1/envelope"));
        assert!(paths.contains_key("/ocla/v1/envelope/batch"));
        assert!(paths.contains_key("/ocla/v1/ledger/summary"));
        assert!(paths.contains_key("/ocla/v1/budget"));
        assert!(paths.contains_key("/ocla/v1/budget/{scope}"));
        assert!(paths.contains_key("/ocla/v1/dlq"));
        assert!(paths.contains_key("/ocla/v1/dlq/{id}/retry"));
        assert!(paths.contains_key("/ocla/v1/dlq/{id}"));
        assert!(paths.contains_key("/ocla/v1/capsule"));
        assert!(paths.contains_key("/ocla/v1/capsule/{ref}"));
        assert!(paths.contains_key("/ocla/v1/capsule/{ref}/fork"));
        assert!(spec["components"]["schemas"]["CanonicalTokenEnvelopeV1"].is_object());
        assert!(spec["components"]["schemas"]["AgentEnvelopeV1"].is_object());
        assert!(spec["components"]["schemas"]["AgentsResponse"].is_object());
        assert!(spec["components"]["schemas"]["MetricsResponse"].is_object());
        assert!(spec["components"]["schemas"]["EnvelopeBatchResponse"].is_object());
        assert!(spec["components"]["schemas"]["BudgetRequest"].is_object());
        assert!(spec["components"]["schemas"]["BudgetResponse"].is_object());
        assert!(spec["components"]["schemas"]["DeadLetter"].is_object());
        assert!(spec["components"]["schemas"]["DlqStats"].is_object());
        assert!(spec["components"]["schemas"]["DlqResponse"].is_object());
        assert!(spec["components"]["schemas"]["DlqRetryResponse"].is_object());
        assert!(spec["components"]["schemas"]["HealthResponse"].is_object());
        assert!(spec["components"]["schemas"]["CapsuleRegisterResponse"].is_object());
        assert!(spec["components"]["schemas"]["CapsuleResolveResponse"].is_object());
        assert!(spec["components"]["schemas"]["CapsuleForkRequest"].is_object());
        let serialized = serde_json::to_string(&spec).expect("serialize OpenAPI spec");
        serde_json::from_str::<Value>(&serialized).expect("OpenAPI spec is valid JSON");
        assert_eq!(
            spec["paths"]["/ocla/v1/envelope"]["post"]["parameters"][0]["name"],
            "Idempotency-Key"
        );
        assert_eq!(
            spec["paths"]["/ocla/v1/envelope/batch"]["post"]["parameters"][0]["name"],
            "Idempotency-Key"
        );
    }
}
