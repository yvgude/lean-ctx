// SPDX-License-Identifier: Apache-2.0
//! The typed, content-free refusal of the proxy (split from `admitted.rs`).

use axum::body::Body;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use serde_json::Value;

use super::{
    FALLBACK_CODE, REASON_CODES_HEADER, REFUSAL_HEADER, REFUSAL_KIND, WITHHELD_BLOCKS_HEADER,
    WITHHELD_STATUS, refusal_text,
};
use crate::core::context_admission::withheld as egress_withheld;

/// Why the gateway did not forward a request. Content-free by construction:
/// only reason codes made of `[a-z0-9._-]` survive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GatewayRefusal {
    pub(super) status: StatusCode,
    pub(super) codes: Vec<String>,
    /// Ids of the withheld blocks the refusal is about (`w-<hex>`).
    pub(super) blocks: Vec<String>,
}

impl GatewayRefusal {
    /// The gateway withheld the request (400).
    pub(crate) fn withheld(code: &str) -> Self {
        Self::new(WITHHELD_STATUS, [code])
    }

    /// The same refusal naming the withheld blocks it is about; anything
    /// that is not a block id is dropped.
    #[must_use]
    pub(crate) fn with_blocks(mut self, blocks: Vec<String>) -> Self {
        self.blocks = blocks
            .into_iter()
            .filter(|id| egress_withheld::valid_id(id))
            .collect();
        self
    }

    /// The gateway could not establish what it must prove (503): a required
    /// receipt or the current policy is unavailable. Nothing was sent.
    pub(crate) fn unavailable(code: &str) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, [code])
    }

    /// From the content-free refusal text of the egress admission, which
    /// names its reason codes in parentheses.
    pub(crate) fn from_reason(reason: &str) -> Self {
        let listed = reason
            .rfind('(')
            .and_then(|open| reason[open + 1..].split(')').next())
            .unwrap_or_default();
        Self::new(WITHHELD_STATUS, listed.split(','))
    }

    pub(super) fn new<'a>(status: StatusCode, codes: impl IntoIterator<Item = &'a str>) -> Self {
        let mut codes: Vec<String> = codes
            .into_iter()
            .map(|code| {
                code.trim()
                    .chars()
                    .filter(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
                    })
                    .collect::<String>()
            })
            .filter(|code| !code.is_empty())
            .collect();
        if codes.is_empty() {
            codes.push(FALLBACK_CODE.to_owned());
        }
        Self {
            status,
            codes,
            blocks: Vec::new(),
        }
    }

    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }

    fn is_withheld(&self) -> bool {
        self.status == WITHHELD_STATUS
    }

    /// One stable, content-free sentence for logs and error bodies: what
    /// was withheld, which blocks, and how a person proceeds. Deterministic.
    pub(crate) fn message(&self) -> String {
        let codes = self.codes.join(", ");
        if !self.is_withheld() {
            return format!("request not forwarded: context gateway unavailable ({codes})");
        }
        let mut message = refusal_text(&codes);
        message.push_str(" — this is not a login problem; logging in again does not help");
        match self.blocks.first() {
            Some(first) => {
                message.push_str(&format!(
                    ". Withheld block: {}. Explain: `lean-ctx gateway why {first}`. \
                     A person may allow it in their own terminal: \
                     `lean-ctx gateway allow-once {first}`",
                    self.blocks.join(", ")
                ));
            }
            None => {
                message
                    .push_str(". Explain: `lean-ctx gateway list` or `lean-ctx inspect --proxy`");
            }
        }
        message
    }

    /// WebSocket close reason (at most 123 bytes by RFC 6455).
    pub(crate) fn close_reason(&self) -> String {
        let mut reason = format!("{REFUSAL_KIND}: {}", self.codes.join(","));
        reason.truncate(123);
        reason
    }

    /// The machine-readable part shared by every transport.
    pub(crate) fn detail(&self) -> Value {
        serde_json::json!({
            "refusal": REFUSAL_KIND,
            "reason_codes": self.codes,
            "blocks": self.blocks,
        })
    }

    /// The refusal in the error shape the caller's SDK understands.
    pub(crate) fn response(&self, provider_label: &str) -> Response {
        let message = self.message();
        let withheld = self.is_withheld();
        let body = match provider_label {
            "Anthropic" | "Bedrock" | "Copilot" => serde_json::json!({
                "type": "error",
                "error": {
                    "type": if withheld { "invalid_request_error" } else { "api_error" },
                    "message": message,
                },
                "lean_ctx": self.detail(),
            }),
            "Gemini" => serde_json::json!({
                "error": {
                    "code": self.status.as_u16(),
                    "message": message,
                    "status": if withheld { "FAILED_PRECONDITION" } else { "UNAVAILABLE" },
                },
                "lean_ctx": self.detail(),
            }),
            _ => serde_json::json!({
                "error": {
                    "message": message,
                    "type": if withheld { "invalid_request_error" } else { "server_error" },
                    "param": null,
                    "code": "context_gateway_refused",
                },
                "lean_ctx": self.detail(),
            }),
        };
        let mut response = Response::new(Body::from(body.to_string()));
        *response.status_mut() = self.status;
        let headers = response.headers_mut();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(REFUSAL_HEADER, HeaderValue::from_static(REFUSAL_KIND));
        if let Ok(value) = HeaderValue::from_str(&self.codes.join(",")) {
            headers.insert(REASON_CODES_HEADER, value);
        }
        if !self.blocks.is_empty()
            && let Ok(value) = HeaderValue::from_str(&self.blocks.join(","))
        {
            headers.insert(WITHHELD_BLOCKS_HEADER, value);
        }
        response
    }
}
