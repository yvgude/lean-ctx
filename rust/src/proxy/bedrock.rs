//! Amazon Bedrock Runtime request validation and AWS Signature Version 4.
//!
//! Credentials are read only from the standard AWS environment variables at
//! request time. The final body bytes (after any bounded proxy transform) are
//! the bytes covered by the payload digest and signature.

use std::collections::BTreeMap;

use axum::http::{
    HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header, request::Parts,
};
use chrono::Utc;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::core::config::ResolvedProvider;

const SERVICE: &str = "bedrock";
const MAX_BODY_BYTES: usize = 25_000_000;
const MAX_CREDENTIAL_BYTES: usize = 4 * 1024;
const ACCESS_KEY_ENV: &str = "AWS_ACCESS_KEY_ID";
const SECRET_KEY_ENV: &str = "AWS_SECRET_ACCESS_KEY";
const SESSION_TOKEN_ENV: &str = "AWS_SESSION_TOKEN";

#[allow(clippy::needless_pass_by_value)]
pub(super) fn passthrough_request_body(
    parsed: Value,
    original_size: usize,
) -> (Vec<u8>, usize, usize) {
    let body = serde_json::to_vec(&parsed).unwrap_or_default();
    (body, original_size, original_size)
}

#[derive(Clone)]
pub(super) struct SigningContext {
    pub(super) region: String,
}

impl std::fmt::Debug for SigningContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SigningContext")
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SigningError {
    MissingCredential,
    InvalidCredential,
    InvalidUrl,
    InvalidHeader,
    InvalidTimestamp,
}

struct Credentials {
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
}

impl Credentials {
    fn from_environment() -> Result<Self, SigningError> {
        Ok(Self {
            access_key: required_credential(ACCESS_KEY_ENV)?,
            secret_key: required_credential(SECRET_KEY_ENV)?,
            session_token: optional_credential(SESSION_TOKEN_ENV)?,
        })
    }
}

fn required_credential(name: &str) -> Result<String, SigningError> {
    optional_credential(name)?.ok_or(SigningError::MissingCredential)
}

fn optional_credential(name: &str) -> Result<Option<String>, SigningError> {
    let value = match std::env::var(name) {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => return Err(SigningError::InvalidCredential),
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_CREDENTIAL_BYTES || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(SigningError::InvalidCredential);
    }
    Ok(Some(value.to_string()))
}

pub(super) fn attach_signing_context(
    provider: &ResolvedProvider,
    request: &mut Request<axum::body::Body>,
) -> Result<(), StatusCode> {
    let Some(region) = provider
        .aws_region
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return Err(StatusCode::BAD_GATEWAY);
    };
    Credentials::from_environment().map_err(|_| StatusCode::BAD_GATEWAY)?;
    strip_untrusted_signing_headers(request.headers_mut());
    request.extensions_mut().insert(SigningContext {
        region: region.to_string(),
    });
    Ok(())
}

pub(super) fn request_body_limit(parts: &Parts) -> Option<usize> {
    parts
        .extensions
        .get::<SigningContext>()
        .map(|_| MAX_BODY_BYTES)
}

pub(super) fn validate_invoke_request<B>(request: &Request<B>) -> Result<(), StatusCode> {
    if request.method() != Method::POST || request.uri().query().is_some() {
        return Err(StatusCode::METHOD_NOT_ALLOWED);
    }
    let Some(rest) = request.uri().path().strip_prefix("/model/") else {
        return Err(StatusCode::NOT_FOUND);
    };
    let Some((model, operation)) = rest.rsplit_once('/') else {
        return Err(StatusCode::NOT_FOUND);
    };
    if model.is_empty()
        || model.len() > 2_048
        || model
            .split('/')
            .any(|segment| segment.is_empty() || segment == "..")
        || !matches!(operation, "invoke" | "invoke-with-response-stream")
    {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(())
}

pub(super) fn sign_request_from_environment(
    parts: &mut Parts,
    url: &str,
    body: &[u8],
) -> Result<(), StatusCode> {
    let Some(context) = parts.extensions.get::<SigningContext>().cloned() else {
        return Ok(());
    };
    let credentials = Credentials::from_environment().map_err(|_| StatusCode::BAD_GATEWAY)?;
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    if !parts.headers.contains_key(header::CONTENT_TYPE) {
        parts.headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    sign_headers_at(
        &parts.method,
        url,
        &mut parts.headers,
        body,
        &credentials,
        &context.region,
        SERVICE,
        &timestamp,
    )
    .map_err(|_| StatusCode::BAD_GATEWAY)
}

fn strip_untrusted_signing_headers(headers: &mut HeaderMap) {
    let names = headers
        .keys()
        .filter(|name| {
            name.as_str().eq_ignore_ascii_case("authorization")
                || name.as_str().starts_with("x-amz-")
        })
        .cloned()
        .collect::<Vec<_>>();
    for name in names {
        headers.remove(name);
    }
}

#[allow(clippy::too_many_arguments)]
fn sign_headers_at(
    method: &Method,
    url: &str,
    headers: &mut HeaderMap,
    body: &[u8],
    credentials: &Credentials,
    region: &str,
    service: &str,
    timestamp: &str,
) -> Result<(), SigningError> {
    if timestamp.len() != 16
        || timestamp.as_bytes().get(8) != Some(&b'T')
        || timestamp.as_bytes().last() != Some(&b'Z')
        || !timestamp
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 8 | 15) || byte.is_ascii_digit())
    {
        return Err(SigningError::InvalidTimestamp);
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| SigningError::InvalidUrl)?;
    let host = canonical_host(&parsed)?;
    strip_untrusted_signing_headers(headers);
    let payload_hash = sha256_hex(body);
    insert_header(headers, "x-amz-content-sha256", &payload_hash)?;
    insert_header(headers, "x-amz-date", timestamp)?;
    if let Some(token) = credentials.session_token.as_deref() {
        insert_header(headers, "x-amz-security-token", token)?;
    }

    let (canonical_headers, signed_headers) = canonical_headers(headers, &host)?;
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.as_str(),
        canonical_uri(parsed.path()),
        canonical_query(parsed.query().unwrap_or_default()),
        canonical_headers,
        signed_headers,
        payload_hash,
    );
    let date = &timestamp[..8];
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let date_key = hmac_sha256(
        format!("AWS4{}", credentials.secret_key).as_bytes(),
        date.as_bytes(),
    );
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, service.as_bytes());
    let signing_key = hmac_sha256(&service_key, b"aws4_request");
    let signature = hex(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key
    );
    insert_header(headers, header::AUTHORIZATION.as_str(), &authorization)?;
    Ok(())
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) -> Result<(), SigningError> {
    let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| SigningError::InvalidHeader)?;
    let value = HeaderValue::from_str(value).map_err(|_| SigningError::InvalidHeader)?;
    headers.insert(name, value);
    Ok(())
}

fn canonical_host(url: &reqwest::Url) -> Result<String, SigningError> {
    let host = url.host_str().ok_or(SigningError::InvalidUrl)?;
    let default_port = match url.scheme() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    let port = url.port().filter(|value| Some(*value) != default_port);
    Ok(port.map_or_else(|| host.to_string(), |port| format!("{host}:{port}")))
}

fn canonical_headers(headers: &HeaderMap, host: &str) -> Result<(String, String), SigningError> {
    let mut values = BTreeMap::<String, Vec<String>>::new();
    values.insert("host".into(), vec![host.to_string()]);
    for (name, value) in headers {
        let name = name.as_str().to_ascii_lowercase();
        if name != "content-type"
            && name != "x-amz-content-sha256"
            && name != "x-amz-date"
            && name != "x-amz-security-token"
            && !name.starts_with("x-amzn-")
        {
            continue;
        }
        let value = value.to_str().map_err(|_| SigningError::InvalidHeader)?;
        values.entry(name).or_default().push(collapse_spaces(value));
    }
    let signed_headers = values.keys().cloned().collect::<Vec<_>>().join(";");
    let canonical = values
        .into_iter()
        .fold(String::new(), |mut output, (name, values)| {
            use std::fmt::Write as _;
            let _ = writeln!(output, "{name}:{}", values.join(","));
            output
        });
    Ok((canonical, signed_headers))
}

fn collapse_spaces(value: &str) -> String {
    value.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

fn canonical_uri(path: &str) -> String {
    if path.is_empty() {
        return "/".into();
    }
    aws_encode(path.as_bytes(), true)
}

fn canonical_query(query: &str) -> String {
    let mut pairs = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (
                aws_encode(name.as_bytes(), false),
                aws_encode(value.as_bytes(), false),
            )
        })
        .collect::<Vec<_>>();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn aws_encode(input: &[u8], preserve_slash: bool) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        let byte = input[index];
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else if preserve_slash && byte == b'/' {
            encoded.push('/');
        } else if byte == b'%'
            && index + 2 < input.len()
            && input[index + 1].is_ascii_hexdigit()
            && input[index + 2].is_ascii_hexdigit()
        {
            encoded.push('%');
            encoded.push(char::from(input[index + 1]).to_ascii_uppercase());
            encoded.push(char::from(input[index + 2]).to_ascii_uppercase());
            index += 2;
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
        index += 1;
    }
    encoded
}

fn sha256_hex(value: &[u8]) -> String {
    hex(&Sha256::digest(value))
}

fn hmac_sha256(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(value);
    mac.finalize().into_bytes().to_vec()
}

fn hex(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(value.len() * 2);
    for byte in value {
        encoded.push(char::from(HEX[(byte >> 4) as usize]));
        encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bedrock_sigv4_is_deterministic_for_fixed_request() {
        let credentials = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        };
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        let mut second = headers.clone();
        let body = br#"{"prompt":"hello"}"#;
        sign_headers_at(
            &Method::POST,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-v2/invoke",
            &mut headers,
            body,
            &credentials,
            "us-east-1",
            SERVICE,
            "20240301T000000Z",
        )
        .unwrap();
        sign_headers_at(
            &Method::POST,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-v2/invoke",
            &mut second,
            body,
            &credentials,
            "us-east-1",
            SERVICE,
            "20240301T000000Z",
        )
        .unwrap();
        assert_eq!(
            headers[header::AUTHORIZATION],
            second[header::AUTHORIZATION]
        );
        assert!(
            headers[header::AUTHORIZATION]
                .to_str()
                .unwrap()
                .contains("/20240301/us-east-1/bedrock/aws4_request")
        );
        assert_eq!(headers["x-amz-content-sha256"], sha256_hex(body));
    }

    #[test]
    fn bedrock_sigv4_matches_fixed_reference_vector() {
        // Fixed AWS SigV4 reference inputs (the same credential/date discipline
        // used by the AWS signing examples), with the Bedrock service scope.
        let credentials = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        };
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));
        let body = br#"{"prompt":"hello"}"#;
        sign_headers_at(
            &Method::POST,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-v2/invoke",
            &mut headers,
            body,
            &credentials,
            "us-east-1",
            SERVICE,
            "20240301T000000Z",
        )
        .unwrap();
        assert_eq!(
            headers[header::AUTHORIZATION],
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240301/us-east-1/bedrock/aws4_request, SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date, Signature=9b82ca1f601090dcdb8213bb724217d852eb1efa80c8ef496cbc1d8c95c89ff8"
        );
    }

    #[test]
    fn eventstream_sigv4_binds_exact_binary_payload() {
        let credentials = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "secret".into(),
            session_token: None,
        };
        let body = [0u8, 1, 2, 0xff, 0x7f];
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.amazon.eventstream"),
        );
        sign_headers_at(
            &Method::POST,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/demo/invoke-with-response-stream",
            &mut headers,
            &body,
            &credentials,
            "us-east-1",
            SERVICE,
            "20240301T000000Z",
        )
        .unwrap();
        assert_eq!(headers["x-amz-content-sha256"], sha256_hex(&body));
        assert!(
            headers[header::AUTHORIZATION]
                .to_str()
                .unwrap()
                .contains("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date")
        );
    }

    #[test]
    fn bedrock_request_metadata_is_signed_when_forwarded() {
        let credentials = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "secret".into(),
            session_token: None,
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-amzn-bedrock-request-metadata",
            HeaderValue::from_static("{\"team\":\"platform\"}"),
        );
        sign_headers_at(
            &Method::POST,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/demo/invoke",
            &mut headers,
            b"{}",
            &credentials,
            "us-east-1",
            SERVICE,
            "20240301T000000Z",
        )
        .unwrap();
        assert!(
            headers[header::AUTHORIZATION]
                .to_str()
                .unwrap()
                .contains("x-amzn-bedrock-request-metadata")
        );
    }

    #[test]
    fn binary_eventstream_never_uses_sse_keepalive() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            "application/vnd.amazon.eventstream".parse().unwrap(),
        );
        assert!(!super::super::forward::response_is_sse(&headers));
        headers.insert(
            "content-type",
            "text/event-stream; charset=utf-8".parse().unwrap(),
        );
        assert!(super::super::forward::response_is_sse(&headers));
        assert!(super::super::forward::is_forwarded_response_header(
            "x-amzn-requestid"
        ));
        assert!(!super::super::forward::is_forwarded_response_header(
            "x-amzn-secret"
        ));
    }

    #[test]
    fn invoke_validation_rejects_queries_and_unknown_operations() {
        for path in [
            "/model/anthropic.claude-v2/invoke",
            "/model/arn%3Aaws%3Abedrock%3Aus-east-1%3A123%3Aprofile%2Fdemo/invoke-with-response-stream",
        ] {
            validate_invoke_request(&Request::post(path).body(()).unwrap()).unwrap();
        }
        for path in [
            "/model/anthropic.claude-v2/converse",
            "/model/anthropic.claude-v2/invoke?unsigned=true",
            "/model/../invoke",
        ] {
            assert!(validate_invoke_request(&Request::post(path).body(()).unwrap()).is_err());
        }
    }

    #[test]
    fn incoming_signing_headers_are_removed_before_new_signature() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("caller"));
        headers.insert("x-amz-date", HeaderValue::from_static("old"));
        headers.insert("x-amz-target", HeaderValue::from_static("old-target"));
        strip_untrusted_signing_headers(&mut headers);
        assert!(headers.is_empty());
    }

    #[test]
    fn body_digest_binds_exact_final_bytes() {
        let credentials = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "secret".into(),
            session_token: None,
        };
        let mut first = HeaderMap::new();
        let mut second = HeaderMap::new();
        sign_headers_at(
            &Method::POST,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/demo/invoke",
            &mut first,
            b"final-body-a",
            &credentials,
            "us-east-1",
            SERVICE,
            "20240301T000000Z",
        )
        .unwrap();
        sign_headers_at(
            &Method::POST,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/demo/invoke",
            &mut second,
            b"final-body-b",
            &credentials,
            "us-east-1",
            SERVICE,
            "20240301T000000Z",
        )
        .unwrap();
        assert_ne!(
            first["x-amz-content-sha256"],
            second["x-amz-content-sha256"]
        );
        assert_ne!(first[header::AUTHORIZATION], second[header::AUTHORIZATION]);
    }

    #[test]
    fn bedrock_body_transform_is_semantic_passthrough() {
        let value = serde_json::json!({
            "messages": [{"role": "user", "content": "keep me"}],
            "temperature": 0.2,
        });
        let original = serde_json::to_vec(&value).unwrap();
        let (body, original_size, compressed_size) =
            passthrough_request_body(value.clone(), original.len());
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), value);
        assert_eq!(original_size, compressed_size);
    }

    #[test]
    fn missing_credentials_fail_closed() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var(ACCESS_KEY_ENV);
        crate::test_env::remove_var(SECRET_KEY_ENV);
        assert!(matches!(
            Credentials::from_environment(),
            Err(SigningError::MissingCredential)
        ));
    }
}
