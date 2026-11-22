// SPDX-License-Identifier: Apache-2.0
//! HTTP transport to the Cloud engine API (`<origin>/v1`).
//!
//! HTTPS only (loopback may use http), redirects refused, bounded response
//! bodies, `Cloud-Protocol` checked on every answer. Tokens travel only in
//! the `Authorization` header and never reach an error message.

use std::time::Duration;

use serde_json::Value;
use zeroize::Zeroizing;

use super::CloudError;
use super::config::normalize_origin;
use lean_ctx_protocol::cloud::{
    CLOUD_API_PREFIX, HEADER_IDEMPOTENCY_KEY, HEADER_INSTANCE, HEADER_MIN_ENGINE, HEADER_NAMES,
    HEADER_PROTOCOL, MAX_BODY_BYTES, NAMES_VAULT, parse_protocol_header,
};

/// `User-Agent` of every request.
pub fn user_agent() -> String {
    format!("lean-ctx/{}", env!("CARGO_PKG_VERSION"))
}

/// A client bound to one Cloud origin, optionally authenticated.
pub struct CloudClient {
    origin: String,
    agent: ureq::Agent,
    auth: Option<(String, Zeroizing<String>)>,
    /// The minor of the last 2.x `Cloud-Protocol` answer; `u16::MAX` until
    /// one came.
    answered_minor: std::sync::atomic::AtomicU16,
}

/// The C0 protocol every request carries, the enrollment included. Cloud
/// accepts only major 2 and answers `426` for a missing or malformed header
/// or another major. From 2.1 an enrollment carries key set v3 and the
/// policy bundle no TOML org policy. Cloud answers `2.<min(minor, 2)>`: a
/// 2.2 answer allows the 2.2 writes (doc nodes, decision anchors), which
/// the engine gates on the answered minor (`relay_v2::protocol_minor`).
pub const PROTOCOL_V2: &str = "2.2";

/// The protocol major this engine speaks and accepts in an answer.
const PROTOCOL_MAJOR: u16 = 2;

/// Why an answer's `Cloud-Protocol` is refused: only a 2.x Cloud is spoken
/// to, and the error names the protocol required.
fn incompatible(peer: &str) -> Option<CloudError> {
    match parse_protocol_header(peer) {
        Some((PROTOCOL_MAJOR, _)) => None,
        Some((major, minor)) => Some(CloudError::Protocol(format!(
            "LeanCTX Cloud speaks protocol {major}.{minor}; this instance requires Cloud-Protocol \
             {PROTOCOL_MAJOR}.x: upgrade the LeanCTX Cloud server"
        ))),
        None => Some(CloudError::Protocol(format!(
            "LeanCTX Cloud sent no valid protocol version; this instance requires Cloud-Protocol \
             {PROTOCOL_MAJOR}.x"
        ))),
    }
}

/// A 2xx or 304 answer.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudResponse {
    pub status: u16,
    /// `Value::Null` for 204/304.
    pub body: Value,
    pub etag: Option<String>,
}

impl CloudClient {
    /// The minor version of the last 2.x answer this client got (`2.2` →
    /// `Some(2)`); `None` before any answer.
    pub fn answered_minor(&self) -> Option<u16> {
        let minor = self
            .answered_minor
            .load(std::sync::atomic::Ordering::Relaxed);
        (minor != u16::MAX).then_some(minor)
    }

    pub fn new(origin: &str) -> Result<Self, CloudError> {
        let origin = normalize_origin(origin)?;
        let agent: ureq::Agent = ureq::config::Config::builder()
            .tls_config(lean_ctx::core::http_client::platform_tls_config())
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
            answered_minor: std::sync::atomic::AtomicU16::new(u16::MAX),
        })
    }

    /// A client whose every request ends within `total` (resolve, connect,
    /// send and receive together): the bounded calls made from a tool call
    /// (C5c, rule 3 exception).
    pub fn bounded(origin: &str, total: Duration) -> Result<Self, CloudError> {
        let origin = normalize_origin(origin)?;
        let agent: ureq::Agent = ureq::config::Config::builder()
            .tls_config(lean_ctx::core::http_client::platform_tls_config())
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(total))
            .build()
            .into();
        Ok(Self {
            origin,
            agent,
            auth: None,
            answered_minor: std::sync::atomic::AtomicU16::new(u16::MAX),
        })
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

    pub fn get(&self, path: &str) -> Result<Value, CloudError> {
        Ok(self.send("GET", path, None, &[], MAX_BODY_BYTES)?.body)
    }

    /// `POST` with a fresh `Idempotency-Key`.
    pub fn post(&self, path: &str, body: &Value) -> Result<Value, CloudError> {
        let key = uuid::Uuid::new_v4().simple().to_string();
        self.post_idempotent(path, body, &key)
    }

    /// `POST` with a caller-chosen `Idempotency-Key` (outbox item id).
    pub fn post_idempotent(
        &self,
        path: &str,
        body: &Value,
        key: &str,
    ) -> Result<Value, CloudError> {
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

    pub fn delete(&self, path: &str) -> Result<(), CloudError> {
        self.send("DELETE", path, None, &[], MAX_BODY_BYTES)
            .map(|_| ())
    }

    /// Any request; `extra` headers win over the defaults, `max_body` bounds
    /// the response. 2xx and 304 are `Ok`, everything else maps to [`CloudError`].
    pub fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        extra: &[(&str, &str)],
        max_body: usize,
    ) -> Result<CloudResponse, CloudError> {
        let (status, etag, _, value) = self.exchange(method, path, body, extra, max_body)?;
        if (200..300).contains(&status) || status == 304 {
            if (200..300).contains(&status) && status != 204 && value.is_null() {
                return Err(CloudError::Protocol(format!(
                    "{method} {path}: body is not JSON"
                )));
            }
            return Ok(CloudResponse {
                status,
                body: value,
                etag,
            });
        }
        Err(CloudError::from_response(status, &value))
    }

    /// `GET` of raw bytes (at most `max_body`), e.g. a file Cloud stores.
    pub fn get_bytes(&self, path: &str, max_body: usize) -> Result<Vec<u8>, CloudError> {
        let (status, _, bytes, value) = self.exchange("GET", path, None, &[], max_body)?;
        if (200..300).contains(&status) {
            return Ok(bytes);
        }
        Err(CloudError::from_response(status, &value))
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
    ) -> Result<(u16, Value), CloudError> {
        self.send_answer_with(method, path, body, &[], max_body)
    }

    /// [`Self::send_answer`] with `extra` headers, which win over the
    /// defaults (as [`Self::send`]); a caller's `Idempotency-Key`, as
    /// [`Self::post_idempotent`] sends it.
    pub fn send_answer_with(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        extra: &[(&str, &str)],
        max_body: usize,
    ) -> Result<(u16, Value), CloudError> {
        let (status, _, _, value) = self.exchange(method, path, body, extra, max_body)?;
        Ok((status, value))
    }

    /// `PUT` of raw bytes (`application/octet-stream`), answering its status
    /// and JSON body even when that is an error (E51 bundle upload).
    pub fn put_bytes_answer(
        &self,
        path: &str,
        bytes: &[u8],
        max_body: usize,
    ) -> Result<(u16, Value), CloudError> {
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
    ) -> Result<(u16, Vec<u8>, Value), CloudError> {
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
    ) -> Result<(u16, Option<String>, Vec<u8>, Value), CloudError> {
        let encoded = match (method, body) {
            ("GET", _) | ("DELETE", None) => None,
            (_, Some(body)) => Some(
                serde_json::to_vec(body)
                    .map_err(|e| CloudError::Protocol(format!("encode request: {e}")))?,
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
    ) -> Result<(u16, Option<String>, Vec<u8>, Value), CloudError> {
        let url = format!("{}{CLOUD_API_PREFIX}{path}", self.origin);
        let user_agent = user_agent();
        let bearer = self
            .auth
            .as_ref()
            .map(|(_, token)| Zeroizing::new(format!("Bearer {}", token.as_str())));
        let mut headers: Vec<(&str, &str)> = vec![
            (HEADER_PROTOCOL, PROTOCOL_V2),
            (HEADER_NAMES, NAMES_VAULT),
            ("User-Agent", &user_agent),
        ];
        if let (Some((instance, _)), Some(bearer)) = (&self.auth, &bearer) {
            headers.push((HEADER_INSTANCE, instance));
            headers.push(("Authorization", bearer));
        }
        headers.retain(|(name, _)| !extra.iter().any(|(e, _)| e.eq_ignore_ascii_case(name)));
        headers.extend_from_slice(extra);

        let result = match (method, body) {
            ("GET", _) => with_headers(self.agent.get(&url), &headers).call(),
            ("DELETE", None) => with_headers(self.agent.delete(&url), &headers).call(),
            // A DELETE that names what it releases (`DELETE /v1/stop`
            // `{confirm, note}`): the body is sent as for a POST.
            ("DELETE", Some((bytes, content_type))) => {
                with_headers(self.agent.delete(&url).force_send_body(), &headers)
                    .header("Content-Type", content_type)
                    .send(bytes)
            }
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
        let peer_protocol = header(HEADER_PROTOCOL);
        let min_instance = header(HEADER_MIN_ENGINE);
        let etag = header("etag");
        let retry_after = header("retry-after");
        let bytes = response
            .into_body()
            .into_with_config()
            .limit(max_body as u64)
            .read_to_vec()
            .map_err(|e| CloudError::Protocol(format!("response body: {e}")))?;
        let mut value = if bytes.iter().all(u8::is_ascii_whitespace) {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        honour_retry_after(status, retry_after.as_deref(), &mut value);
        if status == 426 {
            let min = min_instance
                .or_else(|| value["error"]["min_instance"].as_str().map(str::to_string));
            return Err(CloudError::UpgradeRequired { min });
        }
        // A load balancer or edge proxy answers 413/429/5xx itself, without
        // (or with a stale) protocol header: the status decides, never a
        // "no valid protocol version" refusal.
        if !answered_by_edge(status)
            && let Some(refused) = peer_protocol.as_deref().and_then(incompatible)
        {
            return Err(refused);
        }
        if let Some((PROTOCOL_MAJOR, minor)) =
            peer_protocol.as_deref().and_then(parse_protocol_header)
        {
            self.answered_minor
                .store(minor, std::sync::atomic::Ordering::Relaxed);
        }
        Ok((status, etag, bytes, value))
    }
}

/// Statuses an edge proxy answers on its own (body too large, rate limit,
/// upstream down): classified by status, whatever the protocol header says.
fn answered_by_edge(status: u16) -> bool {
    matches!(status, 413 | 429 | 500..=599)
}

/// A 429/503 whose body names no `retry_after_s` takes the `Retry-After`
/// header's delta-seconds (API §0: 429 sets both; a proxy may set only the
/// header), so every caller backs off by the same value. A body without an
/// `error` object (an edge's HTML page) gets one carrying only that value.
pub(crate) fn honour_retry_after(status: u16, header: Option<&str>, value: &mut Value) {
    if !matches!(status, 429 | 503) {
        return;
    }
    let Some(seconds) = header.and_then(|h| h.trim().parse::<u64>().ok()) else {
        return;
    };
    if !value.is_object() {
        *value = Value::Object(serde_json::Map::new());
    }
    if let Some(body) = value.as_object_mut()
        && !body.get("error").is_some_and(Value::is_object)
    {
        body.insert("error".into(), Value::Object(serde_json::Map::new()));
    }
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

fn offline(error: &ureq::Error) -> CloudError {
    let kind = match error {
        ureq::Error::Timeout(_) => "timeout".to_string(),
        ureq::Error::HostNotFound => "host not found".to_string(),
        ureq::Error::ConnectionFailed => "connection failed".to_string(),
        ureq::Error::Io(e) => format!("i/o: {}", e.kind()),
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) => "tls".to_string(),
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            return CloudError::Protocol("LeanCTX Cloud answered with a redirect".into());
        }
        other => other.to_string().chars().take(120).collect(),
    };
    CloudError::Offline(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clients_refuse_plain_http_to_remote_hosts() {
        assert!(CloudClient::new("http://cloud.example.com").is_err());
        assert!(CloudClient::new("https://cloud.example.com").is_ok());
        assert!(CloudClient::new("http://127.0.0.1:9").is_ok());
    }

    #[test]
    fn only_a_2x_answer_is_accepted_and_a_refusal_names_the_protocol() {
        assert!(incompatible("2.0").is_none());
        assert!(incompatible("2.7").is_none());
        for peer in ["1.9", "1.8", "1.0", "3.0", "", "two"] {
            let Some(CloudError::Protocol(message)) = incompatible(peer) else {
                panic!("{peer:?} must be refused");
            };
            assert!(message.contains("Cloud-Protocol 2.x"), "{message}");
        }
    }

    /// A Cloud on 127.0.0.1 that answers one request with `raw`.
    fn answering(raw: String) -> String {
        use std::io::{BufRead as _, Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let Some(Ok(stream)) = listener.incoming().next() else {
                return;
            };
            let mut writer = stream.try_clone().unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let _ = writer.write_all(raw.as_bytes());
        });
        origin
    }

    fn edge(status: u16, headers: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} X\r\nContent-Type: text/html\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// Review fix: an edge's HTML error (no or a garbled protocol header) is
    /// classified by status, a bare `Retry-After` is honoured.
    #[test]
    fn edge_errors_are_classified_by_status_not_by_protocol_header() {
        let page = "<html><body>busy</body></html>";
        let garbled = format!("{HEADER_PROTOCOL}: nginx\r\n");
        let origin = answering(edge(429, &format!("{garbled}Retry-After: 42\r\n"), page));
        let limited = CloudClient::new(&origin).unwrap().post("/x", &Value::Null);
        assert_eq!(
            limited,
            Err(CloudError::RateLimited {
                retry_after: Some(42)
            })
        );
        for status in [502, 503, 504] {
            let origin = answering(edge(status, &garbled, page));
            let down = CloudClient::new(&origin).unwrap().get("/keys").unwrap_err();
            assert!(
                matches!(down, CloudError::Server { status: s, .. } if s == status),
                "{status}: {down:?}"
            );
            assert!(down.is_transient());
        }
        let origin = answering(edge(413, "", page));
        let too_large = CloudClient::new(&origin).unwrap().post("/x", &Value::Null);
        assert_eq!(
            too_large,
            Err(CloudError::Server {
                status: 413,
                code: "payload_too_large".into()
            })
        );
        // A 2xx with a garbled header is still refused.
        let origin = answering(format!(
            "HTTP/1.1 200 OK\r\n{garbled}Content-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        ));
        assert!(matches!(
            CloudClient::new(&origin).unwrap().get("/keys"),
            Err(CloudError::Protocol(_))
        ));
    }

    #[test]
    fn a_bare_retry_after_header_becomes_the_error_body() {
        let mut body = Value::Null;
        honour_retry_after(429, Some("17"), &mut body);
        assert_eq!(body, serde_json::json!({"error": {"retry_after_s": 17}}));
        let mut other = serde_json::json!({"detail": "x"});
        honour_retry_after(503, Some("3"), &mut other);
        assert_eq!(
            other,
            serde_json::json!({"detail": "x", "error": {"retry_after_s": 3}})
        );
    }

    #[test]
    fn unreachable_cloud_is_offline() {
        let client = CloudClient::new("http://127.0.0.1:9").unwrap();
        assert!(matches!(client.get("/keys"), Err(CloudError::Offline(_))));
    }
}
