// SPDX-License-Identifier: Apache-2.0
//! HTTP transport to the Hive engine API (`<origin>/v1`).
//!
//! HTTPS only (loopback may use http), redirects refused, bounded response
//! bodies, `Hive-Protocol` checked on every answer. Tokens travel only in
//! the `Authorization` header and never reach an error message.

use std::time::Duration;

use serde_json::Value;
use zeroize::Zeroizing;

use super::HiveError;
use super::config::normalize_origin;
use lean_ctx_protocol::hive::{
    HEADER_IDEMPOTENCY_KEY, HEADER_INSTANCE, HEADER_MIN_ENGINE, HEADER_NAMES, HEADER_PROTOCOL,
    HIVE_API_PREFIX, MAX_BODY_BYTES, NAMES_VAULT, parse_protocol_header, protocol_compatible,
    protocol_header_value,
};

/// `User-Agent` of every request.
pub fn user_agent() -> String {
    format!("lean-ctx/{}", env!("CARGO_PKG_VERSION"))
}

/// A client bound to one Hive origin, optionally authenticated.
pub struct HiveClient {
    origin: String,
    agent: ureq::Agent,
    auth: Option<(String, Zeroizing<String>)>,
    /// `Hive-Protocol` sent; `None` = this engine's 1.x version.
    protocol: Option<&'static str>,
}

/// Hive name of `Hive-Protocol` (same value). Sent alongside it; a response
/// may carry either, `Hive-Protocol` is read first.
pub const HEADER_PROTOCOL_HIVE: &str = "Hive-Protocol";

/// The C0 2.0 protocol, spoken only to a Hive that answered the instance
/// card (`bus::relay_v2::speaks_v2`).
pub const PROTOCOL_V2: &str = "2.0";

/// The protocol major of a header value.
fn major(value: &str) -> Option<u16> {
    parse_protocol_header(value).map(|(major, _)| major)
}

/// A 2xx or 304 answer.
#[derive(Debug, Clone, PartialEq)]
pub struct HiveResponse {
    pub status: u16,
    /// `Value::Null` for 204/304.
    pub body: Value,
    pub etag: Option<String>,
}

impl HiveClient {
    pub fn new(origin: &str) -> Result<Self, HiveError> {
        let origin = normalize_origin(origin)?;
        let agent: ureq::Agent = ureq::config::Config::builder()
            .tls_config(crate::core::http_client::platform_tls_config())
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_resolve(Some(Duration::from_secs(5)))
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build()
            .into();
        Ok(Self {
            origin,
            agent,
            auth: None,
            protocol: None,
        })
    }

    /// A client whose every request ends within `total` (resolve, connect,
    /// send and receive together): the bounded calls made from a tool call
    /// (C5c, rule 3 exception).
    pub fn bounded(origin: &str, total: Duration) -> Result<Self, HiveError> {
        let origin = normalize_origin(origin)?;
        let agent: ureq::Agent = ureq::config::Config::builder()
            .tls_config(crate::core::http_client::platform_tls_config())
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(total))
            .build()
            .into();
        Ok(Self {
            origin,
            agent,
            auth: None,
            protocol: None,
        })
    }

    /// Speak C0 2.0 (`Hive-Protocol: 2.0`) with this client. Only for an
    /// Hive known to speak it; the 1.x default never changes on its own.
    pub fn speaking_v2(mut self, v2: bool) -> Self {
        self.protocol = v2.then_some(PROTOCOL_V2);
        self
    }

    /// Authenticate subsequent requests as `instance_id`.
    pub fn with_token(mut self, instance_id: &str, access_token: &str) -> Self {
        self.auth = Some((
            instance_id.to_string(),
            Zeroizing::new(access_token.to_string()),
        ));
        self
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn get(&self, path: &str) -> Result<Value, HiveError> {
        Ok(self.send("GET", path, None, &[], MAX_BODY_BYTES)?.body)
    }

    /// `POST` with a fresh `Idempotency-Key`.
    pub fn post(&self, path: &str, body: &Value) -> Result<Value, HiveError> {
        let key = uuid::Uuid::new_v4().simple().to_string();
        self.post_idempotent(path, body, &key)
    }

    /// `POST` with a caller-chosen `Idempotency-Key` (outbox item id).
    pub fn post_idempotent(&self, path: &str, body: &Value, key: &str) -> Result<Value, HiveError> {
        Ok(self
            .send(
                "POST",
                path,
                Some(body),
                &[(HEADER_IDEMPOTENCY_KEY, key)],
                MAX_BODY_BYTES,
            )?
            .body)
    }

    pub fn delete(&self, path: &str) -> Result<(), HiveError> {
        self.send("DELETE", path, None, &[], MAX_BODY_BYTES)
            .map(|_| ())
    }

    /// Any request; `extra` headers win over the defaults, `max_body` bounds
    /// the response. 2xx and 304 are `Ok`, everything else maps to [`HiveError`].
    pub fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        extra: &[(&str, &str)],
        max_body: usize,
    ) -> Result<HiveResponse, HiveError> {
        let (status, etag, _, value) = self.exchange(method, path, body, extra, max_body)?;
        if (200..300).contains(&status) || status == 304 {
            if (200..300).contains(&status) && status != 204 && value.is_null() {
                return Err(HiveError::Protocol(format!(
                    "{method} {path}: body is not JSON"
                )));
            }
            return Ok(HiveResponse {
                status,
                body: value,
                etag,
            });
        }
        Err(HiveError::from_response(status, &value))
    }

    /// `GET` of raw bytes (at most `max_body`), e.g. a file Hive stores.
    pub fn get_bytes(&self, path: &str, max_body: usize) -> Result<Vec<u8>, HiveError> {
        let (status, _, bytes, value) = self.exchange("GET", path, None, &[], max_body)?;
        if (200..300).contains(&status) {
            return Ok(bytes);
        }
        Err(HiveError::from_response(status, &value))
    }

    /// Any request, answering its status and JSON body even when that is an
    /// error, for C0 2.0 routes whose `error.reason` the caller words
    /// itself; transport failures are still errors.
    pub fn send_answer(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        max_body: usize,
    ) -> Result<(u16, Value), HiveError> {
        let (status, _, _, value) = self.exchange(method, path, body, &[], max_body)?;
        Ok((status, value))
    }

    /// `PUT` of raw bytes (`application/octet-stream`), answering its status
    /// and JSON body even when that is an error (E51 bundle upload).
    pub fn put_bytes_answer(
        &self,
        path: &str,
        bytes: &[u8],
        max_body: usize,
    ) -> Result<(u16, Value), HiveError> {
        let (status, _, _, value) = self.exchange_raw(
            "PUT",
            path,
            Some((bytes, "application/octet-stream")),
            &[],
            max_body,
        )?;
        Ok((status, value))
    }

    /// `GET` of raw bytes (at most `max_body`), answering the status and the
    /// bytes; an error body is parsed as JSON (`Null` when it is none).
    pub fn get_bytes_answer(
        &self,
        path: &str,
        max_body: usize,
    ) -> Result<(u16, Vec<u8>, Value), HiveError> {
        let (status, _, bytes, value) = self.exchange_raw("GET", path, None, &[], max_body)?;
        Ok((status, bytes, value))
    }

    /// One request: its status, `ETag`, body bytes and the body as JSON
    /// (`Null` when it is none); `426` and an incompatible protocol are errors.
    fn exchange(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        extra: &[(&str, &str)],
        max_body: usize,
    ) -> Result<(u16, Option<String>, Vec<u8>, Value), HiveError> {
        let encoded = match (method, body) {
            ("GET" | "DELETE", _) => None,
            (_, Some(body)) => Some(
                serde_json::to_vec(body)
                    .map_err(|e| HiveError::Protocol(format!("encode request: {e}")))?,
            ),
            (_, None) => Some(Vec::new()),
        };
        self.exchange_raw(
            method,
            path,
            encoded.as_deref().map(|bytes| (bytes, "application/json")),
            extra,
            max_body,
        )
    }

    /// [`Self::exchange`] with a body already encoded: `(bytes, content type)`.
    fn exchange_raw(
        &self,
        method: &str,
        path: &str,
        body: Option<(&[u8], &str)>,
        extra: &[(&str, &str)],
        max_body: usize,
    ) -> Result<(u16, Option<String>, Vec<u8>, Value), HiveError> {
        let url = format!("{}{HIVE_API_PREFIX}{path}", self.origin);
        let protocol = self
            .protocol
            .map_or_else(protocol_header_value, str::to_string);
        let user_agent = user_agent();
        let bearer = self
            .auth
            .as_ref()
            .map(|(_, token)| Zeroizing::new(format!("Bearer {}", token.as_str())));
        let mut headers: Vec<(&str, &str)> = vec![
            (HEADER_PROTOCOL, &protocol),
            (HEADER_NAMES, NAMES_VAULT),
            ("User-Agent", &user_agent),
        ];
        if let (Some((instance, _)), Some(bearer)) = (&self.auth, &bearer) {
            headers.push((HEADER_INSTANCE, instance));
            headers.push(("Authorization", bearer));
        }
        headers.retain(|(name, _)| !extra.iter().any(|(e, _)| e.eq_ignore_ascii_case(name)));
        headers.extend_from_slice(extra);
        // The major this request was sent with (an explicit header wins).
        let sent_protocol = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(HEADER_PROTOCOL))
            .map(|(_, value)| *value);
        let sent_major = sent_protocol.and_then(major);
        // The Hive name of the same header, same value (Hive servers read it first).
        if let Some(value) = sent_protocol
            && !headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(HEADER_PROTOCOL_HIVE))
        {
            headers.insert(0, (HEADER_PROTOCOL_HIVE, value));
        }

        let result = match (method, body) {
            ("GET", _) => with_headers(self.agent.get(&url), &headers).call(),
            ("DELETE", _) => with_headers(self.agent.delete(&url), &headers).call(),
            (_, body) => {
                let (bytes, content_type) = body.unwrap_or((&[], "application/json"));
                let request = match method {
                    "PUT" => self.agent.put(&url),
                    "PATCH" => self.agent.patch(&url),
                    _ => self.agent.post(&url),
                };
                with_headers(request, &headers)
                    .header("Content-Type", content_type)
                    .send(bytes)
            }
        };
        let response = result.map_err(|e| offline(&e))?;
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let peer_protocol = header(HEADER_PROTOCOL_HIVE).or_else(|| header(HEADER_PROTOCOL));
        let min_engine = header(HEADER_MIN_ENGINE);
        let etag = header("etag");
        let retry_after = header("retry-after");
        let bytes = response
            .into_body()
            .into_with_config()
            .limit(max_body as u64)
            .read_to_vec()
            .map_err(|e| HiveError::Protocol(format!("response body: {e}")))?;
        let mut value = if bytes.iter().all(u8::is_ascii_whitespace) {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        honour_retry_after(status, retry_after.as_deref(), &mut value);
        if status == 426 {
            let min =
                min_engine.or_else(|| value["error"]["min_engine"].as_str().map(str::to_string));
            return Err(HiveError::UpgradeRequired { min });
        }
        // A 2.x answer is right for a 2.x request; anything else must be the
        // 1.x major this engine speaks.
        if let Some(peer) = &peer_protocol
            && !protocol_compatible(peer)
            && !(sent_major == Some(2) && major(peer) == Some(2))
        {
            return Err(HiveError::Protocol(match parse_protocol_header(peer) {
                Some((major, _)) => format!("Hive speaks protocol {major}, this engine 1"),
                None => "Hive sent no valid protocol version".into(),
            }));
        }
        Ok((status, etag, bytes, value))
    }
}

/// A 429/503 whose body names no `retry_after_s` takes the `Retry-After`
/// header's delta-seconds (API §0: 429 sets both; a proxy may set only the
/// header), so every caller backs off by the same value.
fn honour_retry_after(status: u16, header: Option<&str>, value: &mut Value) {
    if !matches!(status, 429 | 503) {
        return;
    }
    let Some(seconds) = header.and_then(|h| h.trim().parse::<u64>().ok()) else {
        return;
    };
    if let Some(error) = value.get_mut("error").and_then(Value::as_object_mut)
        && !error.contains_key("retry_after_s")
    {
        error.insert("retry_after_s".into(), seconds.into());
    }
}

#[cfg(test)]
mod retry_after_tests {
    use super::*;

    #[test]
    fn the_retry_after_header_fills_a_missing_retry_after_s() {
        let mut body = serde_json::json!({"error": {"code": "quota_exceeded", "message": "x"}});
        honour_retry_after(429, Some(" 120 "), &mut body);
        assert_eq!(body["error"]["retry_after_s"], 120);
        // The body's own value wins; other statuses and dates are left alone.
        honour_retry_after(429, Some("5"), &mut body);
        assert_eq!(body["error"]["retry_after_s"], 120);
        let mut other = serde_json::json!({"error": {"code": "not_found", "message": "x"}});
        honour_retry_after(404, Some("5"), &mut other);
        honour_retry_after(429, Some("Wed, 21 Oct 2026 07:28:00 GMT"), &mut other);
        assert!(other["error"].get("retry_after_s").is_none());
    }
}

fn with_headers<B>(
    mut request: ureq::RequestBuilder<B>,
    headers: &[(&str, &str)],
) -> ureq::RequestBuilder<B> {
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request
}

fn offline(error: &ureq::Error) -> HiveError {
    let kind = match error {
        ureq::Error::Timeout(_) => "timeout".to_string(),
        ureq::Error::HostNotFound => "host not found".to_string(),
        ureq::Error::ConnectionFailed => "connection failed".to_string(),
        ureq::Error::Io(e) => format!("i/o: {}", e.kind()),
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) => "tls".to_string(),
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            return HiveError::Protocol("Hive answered with a redirect".into());
        }
        other => other.to_string().chars().take(120).collect(),
    };
    HiveError::Offline(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clients_refuse_plain_http_to_remote_hosts() {
        assert!(HiveClient::new("http://hive.example.com").is_err());
        assert!(HiveClient::new("https://hive.example.com").is_ok());
        assert!(HiveClient::new("http://127.0.0.1:9").is_ok());
    }

    #[test]
    fn unreachable_hive_is_offline() {
        let client = HiveClient::new("http://127.0.0.1:9").unwrap();
        assert!(matches!(client.get("/keys"), Err(HiveError::Offline(_))));
    }
}
