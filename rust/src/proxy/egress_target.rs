// SPDX-License-Identifier: Apache-2.0
//! What a request carries besides its body, as the text the gateway admits
//! (AP10-C2): the caller-chosen path and query and the values of the
//! caller-controlled headers relayed with it.
//!
//! The text is built for detection only; the request is sent as it was:
//!
//! - path segments and query names and values are percent-decoded (lossy
//!   UTF-8, `+` in the query is a space), as the destination reads them;
//! - one query line per parameter (`name=value`), one header line per value
//!   (`name: value`), so key/value detectors see their context;
//! - the destination's own credential parameter is left out only where the
//!   configured upstream is that destination, it occurs exactly once and its
//!   value has that credential's shape: Google's `key` (API key, `AIza…`) on
//!   `*.googleapis.com`, Azure's `api-key` on Azure OpenAI hosts. Anything
//!   else is admitted;
//! - only the credential headers of the destination the request goes to are
//!   left out (see [`Destination`]); `authorization` only when it has the
//!   shape of a credential (`Bearer`/`Basic`/AWS SigV4 and a token), so it
//!   cannot carry free text. Every other relayed header value is admitted;
//! - a millisecond timestamp — exactly 13 digits between 1.0e12 and 2.5e12
//!   (2001–2049), with no letter, digit, `-`, `_` or `.` on either side — is
//!   masked before detection so it is not read as a card number. Nothing else
//!   is masked: a longer run, or one that touches a token, is inspected as
//!   it is.

use std::borrow::Cow;

use http::{HeaderName, HeaderValue};

/// Where a request goes, for the credentials that destination takes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Destination {
    /// chatgpt.com: the backend passthrough, its tunnel, the Codex model
    /// rail (`ChatGPT` provider label).
    ChatGpt,
    /// AWS Bedrock: SigV4 headers.
    Bedrock,
    /// Google (`*.googleapis.com`).
    Google,
    /// Azure OpenAI hosts.
    Azure,
    /// Anthropic.
    Anthropic,
    /// Any other (OpenAI-compatible or registry) destination.
    Other,
}

impl Destination {
    fn of(provider: &str, upstream_base: &str) -> Self {
        let host = reqwest::Url::parse(upstream_base)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        if host == "googleapis.com" || host.ends_with(".googleapis.com") {
            Self::Google
        } else if [
            ".openai.azure.com",
            ".cognitiveservices.azure.com",
            ".services.ai.azure.com",
            ".azure-api.net",
        ]
        .iter()
        .any(|suffix| host.ends_with(suffix))
        {
            Self::Azure
        } else {
            match provider {
                "ChatGPT" => Self::ChatGpt,
                "Bedrock" => Self::Bedrock,
                "Gemini" => Self::Google,
                "Anthropic" => Self::Anthropic,
                _ => Self::Other,
            }
        }
    }

    /// The header `name` with `value` is this destination's credential.
    fn credential_header(self, name: &str, value: &[u8]) -> bool {
        if name == "authorization" {
            return authorization_shaped(value);
        }
        match self {
            Self::ChatGpt => name == "cookie",
            Self::Bedrock => matches!(
                name,
                "x-amz-date" | "x-amz-content-sha256" | "x-amz-security-token"
            ),
            Self::Google => name == "x-goog-api-key",
            Self::Azure => name == "api-key",
            Self::Anthropic => name == "x-api-key",
            // OpenAI-compatible and registry routes, including the CLIs that
            // authenticate with their own header (Command Code, xAI).
            Self::Other => matches!(
                name,
                "x-api-key" | "api-key" | "x-oauth-token" | "x-xai-token-auth"
            ),
        }
    }

    /// The credential query parameters this destination takes, with the
    /// shape their value must have.
    fn credential_parameters(self) -> &'static [(&'static str, fn(&str) -> bool)] {
        match self {
            Self::Google => &[("key", google_api_key)],
            Self::Azure => &[("api-key", azure_key)],
            _ => &[],
        }
    }
}

/// `Bearer <token>`, `Basic <base64>` or an AWS SigV4 authorization: a
/// credential, not text.
fn authorization_shaped(value: &[u8]) -> bool {
    let Ok(value) = std::str::from_utf8(value) else {
        return false;
    };
    let token = |rest: &str, extra: &[u8]| {
        !rest.is_empty()
            && rest.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte) || extra.contains(&byte)
            })
    };
    if let Some(rest) = value.strip_prefix("Bearer ") {
        token(rest, b"")
    } else if let Some(rest) = value.strip_prefix("Basic ") {
        token(rest, b"")
    } else if let Some(rest) = value.strip_prefix("AWS4-HMAC-SHA256 ") {
        token(rest, b", ;")
    } else {
        false
    }
}

fn token_chars(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// `AIza` and 35 characters of `[0-9A-Za-z_-]`.
fn google_api_key(value: &str) -> bool {
    value.len() == 39 && value.starts_with("AIza") && token_chars(value)
}

/// An Azure resource key: 32 hex digits, or the 84-character form.
fn azure_key(value: &str) -> bool {
    (value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        || (value.len() == 84 && value.bytes().all(|byte| byte.is_ascii_alphanumeric()))
}

/// Percent-decode `raw` (lossy UTF-8); `plus` decodes `+` as a space.
fn decoded(raw: &str, plus: bool) -> Cow<'_, str> {
    if !(raw.contains('%') || (plus && raw.contains('+'))) {
        return Cow::Borrowed(raw);
    }
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let hex = |at: usize| bytes.get(at).and_then(|b| (*b as char).to_digit(16));
        match (byte, hex(index + 1), hex(index + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push(u8::try_from(high * 16 + low).unwrap_or(b'%'));
                index += 3;
            }
            (b'+', _, _) if plus => {
                out.push(b' ');
                index += 1;
            }
            _ => {
                out.push(byte);
                index += 1;
            }
        }
    }
    Cow::Owned(String::from_utf8_lossy(&out).into_owned())
}

/// `text` with every standalone millisecond timestamp masked: exactly 13
/// digits, between 1.0e12 and 2.5e12, with no letter, digit, `-`, `_` or `.`
/// on either side. A 13-digit card number starts with 3–6 and is above that
/// range; nothing else is touched.
fn without_epoch_millis(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let joined = |at: Option<&u8>| {
        at.is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut start = 0;
    while start < bytes.len() {
        if !bytes[start].is_ascii_digit() {
            start += 1;
            continue;
        }
        let end = bytes[start..]
            .iter()
            .position(|byte| !byte.is_ascii_digit())
            .map_or(bytes.len(), |offset| start + offset);
        let epoch = end - start == 13
            && !joined(start.checked_sub(1).and_then(|before| bytes.get(before)))
            && !joined(bytes.get(end))
            && text[start..end]
                .parse::<u64>()
                .is_ok_and(|value| (1_000_000_000_000..=2_500_000_000_000).contains(&value));
        if epoch {
            let out = out.get_or_insert_with(|| String::with_capacity(text.len()));
            out.push_str(&text[copied..start]);
            out.push('#');
            copied = end;
        }
        start = end;
    }
    match out {
        Some(mut out) => {
            out.push_str(&text[copied..]);
            Cow::Owned(out)
        }
        None => Cow::Borrowed(text),
    }
}

/// The text the gateway admits for a request for `provider` to
/// `upstream_base` with `path_and_query` and the relayed `headers` (see the
/// module docs).
pub(crate) fn target_text<'h>(
    provider: &str,
    upstream_base: &str,
    path_and_query: &str,
    headers: impl IntoIterator<Item = (&'h HeaderName, &'h HeaderValue)>,
) -> String {
    let destination = Destination::of(provider, upstream_base);
    let (path, query) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));
    let mut lines = Vec::new();
    let path = path
        .split('/')
        .map(|segment| decoded(segment, false))
        .collect::<Vec<_>>()
        .join("/");
    lines.push(without_epoch_millis(&path).into_owned());

    let pairs: Vec<(Cow<'_, str>, Cow<'_, str>)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decoded(name, true), decoded(value, true))
        })
        .collect();
    let credentials = destination.credential_parameters();
    for (name, value) in &pairs {
        let exempt = credentials.iter().any(|(credential, shape)| {
            name.as_ref() == *credential
                && shape(value)
                && pairs
                    .iter()
                    .filter(|(other, _)| other.as_ref() == *credential)
                    .count()
                    == 1
        });
        if !exempt {
            let line = format!("{name}={value}");
            lines.push(without_epoch_millis(&line).into_owned());
        }
    }

    for (name, value) in headers {
        if destination.credential_header(name.as_str(), value.as_bytes()) {
            continue;
        }
        let line = format!(
            "{}: {}",
            name.as_str(),
            String::from_utf8_lossy(value.as_bytes())
        );
        lines.push(without_epoch_millis(&line).into_owned());
    }
    lines.retain(|line| !line.is_empty());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Split so the repository's secret scanners never see a literal key.
    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    fn head(pairs: &[(&'static str, &'static str)]) -> Vec<(HeaderName, HeaderValue)> {
        pairs
            .iter()
            .map(|(name, value)| {
                (
                    HeaderName::from_static(name),
                    HeaderValue::from_static(value),
                )
            })
            .collect()
    }

    type Case = (
        &'static str,
        &'static str,
        String,
        Vec<(HeaderName, HeaderValue)>,
    );

    fn admitted(
        provider: &str,
        upstream: &str,
        target: &str,
        headers: &[(HeaderName, HeaderValue)],
    ) -> bool {
        let text = target_text(
            provider,
            upstream,
            target,
            headers.iter().map(|(n, v)| (n, v)),
        );
        let policy = crate::core::context_admission::AdmissionPolicy::from_config(
            &crate::core::config::Config::load_arc(),
        );
        let admission = crate::core::context_admission::admit(&text, None, &policy);
        admission.text.as_deref() == Some(text.as_str())
    }

    /// AP10-C2: the URL and header text the gateway admits. Real provider
    /// targets pass in the default (developer) configuration; a protected
    /// value — plain, percent-encoded, behind a credential parameter of
    /// another destination, or in a relayed header — does not.
    #[test]
    fn real_provider_targets_pass_and_protected_values_do_not() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let google_key = concat!("AI", "zaSyD0123456789abcdefghijklmnopqrstuv");
        let gemini = "https://generativelanguage.googleapis.com";
        let azure = "https://contoso.openai.azure.com";
        let azure_key = "0123456789abcdef0123456789abcdef";
        let codex = head(&[
            ("user-agent", "codex_cli_rs/0.47.0 (Mac OS 15.6.1; arm64)"),
            ("x-codex-turn-metadata", "{\"turn\":3,\"ts\":1760781123456}"),
            ("x-session-id", "0199f2c3-7a1e-7c4b-9a51-1c0e5d2b8f40"),
            (
                "x-claude-code-session-id",
                "11111111-2222-3333-4444-555555555555",
            ),
            ("authorization", "Bearer sk-proj-REDACTEDREDACTEDREDACTED"),
            ("x-grok-req-id", "0199f2c3-7a1e-7c4b-9a51-1c0e5d2b8f41"),
        ]);
        let openai = "https://api.openai.com";
        let chatgpt = "https://chatgpt.com";
        let passes: Vec<Case> = vec![
            ("OpenAI", openai, "/v1/chat/completions".into(), codex.clone()),
            ("OpenAI", openai, "/v1/responses?stream=true".into(), vec![]),
            ("Anthropic", "https://api.anthropic.com", "/v1/messages?beta=true".into(), vec![]),
            (
                "OpenAI",
                azure,
                format!(
                    "/openai/deployments/gpt-4o/chat/completions?api-version=2024-10-21&api-key={azure_key}"
                ),
                vec![],
            ),
            (
                "Bedrock",
                "https://bedrock-runtime.us-east-1.amazonaws.com",
                "/model/arn%3Aaws%3Abedrock%3Aus-east-1%3A123456789012%3Ainference-profile%2Fus.anthropic.claude-sonnet-4-20250514-v1%3A0/invoke-with-response-stream".into(),
                head(&[("x-amz-security-token", "IQoJb3JpZ2luX2VjEJr//////////wEaCXVzLWVhc3QtMSJHMEUCIQ")]),
            ),
            (
                "Gemini",
                "https://us-central1-aiplatform.googleapis.com",
                "/v1/projects/my-project-1234/locations/us-central1/publishers/google/models/gemini-2.5-pro:streamGenerateContent?alt=sse".into(),
                vec![],
            ),
            (
                "Gemini",
                gemini,
                format!("/v1beta/models/gemini-2.5-pro:generateContent?alt=sse&key={google_key}"),
                vec![],
            ),
            ("OpenAI", "https://openrouter.ai/api", "/v1/chat/completions".into(), vec![]),
            (
                "ChatGPT",
                chatgpt,
                "/backend-api/conversation/6650c2e3-1a2b-4c5d-8e9f-0a1b2c3d4e5f/stream_status?ts=1760781123456".into(),
                head(&[
                    ("cookie", "__Secure-next-auth.session-token=abc"),
                    ("authorization", "Bearer eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln"),
                    ("chatgpt-account-id", "6f8e2a1c-0000-4000-8000-0123456789ab"),
                ]),
            ),
            (
                "ChatGPT",
                chatgpt,
                "/backend-api/wham/connect?client_version=0.47.0".into(),
                head(&[("openai-beta", "responses_websockets=2026-02-06")]),
            ),
            // A standalone millisecond timestamp is no card number.
            ("OpenAI", openai, "/v1/files/file-abc/content/1760781123456?after=1760781123456".into(), vec![]),
        ];
        for (provider, upstream, target, headers) in &passes {
            assert!(
                admitted(provider, upstream, target, headers),
                "{upstream}{target}"
            );
        }

        let encoded = AWS_KEY
            .bytes()
            .map(|byte| format!("%{byte:02X}"))
            .collect::<Vec<_>>()
            .concat();
        let header = |name: &'static str, value: String| {
            vec![(
                HeaderName::from_static(name),
                HeaderValue::from_str(&value).unwrap(),
            )]
        };
        // Split so the repository's secret scanners never see a literal token.
        let slack = concat!(
            "xox",
            "b-1234567890123-1234567890123-abcdefghijklmnopqrstuvwx"
        );
        let card = "4111111111111111";
        let refused: Vec<Case> = vec![
            (
                "OpenAI",
                openai,
                format!("/v1/chat/completions?note={AWS_KEY}"),
                vec![],
            ),
            (
                "OpenAI",
                openai,
                format!("/v1/chat/completions?note={encoded}"),
                vec![],
            ),
            ("OpenAI", openai, format!("/v1/files/{encoded}"), vec![]),
            // `key` is a credential parameter of Google's hosts only, and only
            // once, in its own shape; `access_token` is none.
            (
                "OpenAI",
                openai,
                format!("/v1/chat/completions?key={AWS_KEY}"),
                vec![],
            ),
            (
                "Gemini",
                gemini,
                format!("/v1beta/models/m:generateContent?key={AWS_KEY}"),
                vec![],
            ),
            (
                "Gemini",
                gemini,
                format!("/v1beta/models/m:generateContent?key={google_key}&key={AWS_KEY}"),
                vec![],
            ),
            (
                "Gemini",
                gemini,
                format!("/v1beta/models/m:generateContent?access_token=ya29.{AWS_KEY}"),
                vec![],
            ),
            (
                "OpenAI",
                openai,
                format!("/v1/chat/completions?api-key={azure_key}&token={AWS_KEY}"),
                vec![],
            ),
            // Card numbers and tokens with digit runs are inspected as
            // (IBANs are a Hive filter pack since detector set 2.0.0, not OSS)
            // they are, under any name, in the path, the query or a header.
            (
                "OpenAI",
                openai,
                format!("/v1/chat/completions?note={card}"),
                vec![],
            ),
            ("OpenAI", openai, format!("/v1/files/{card}"), vec![]),
            (
                "OpenAI",
                openai,
                "/v1/chat/completions".into(),
                header("x-codex-turn-metadata", format!("{{\"pay\":\"{card}\"}}")),
            ),
            (
                "OpenAI",
                openai,
                format!("/v1/chat/completions?t={slack}"),
                vec![],
            ),
            (
                "OpenAI",
                openai,
                "/v1/chat/completions".into(),
                header("x-session-id", slack.to_owned()),
            ),
            (
                "OpenAI",
                openai,
                "/v1/chat/completions".into(),
                header(
                    "x-codex-turn-metadata",
                    format!("{{\"note\":\"{AWS_KEY}\"}}"),
                ),
            ),
            // On the ChatGPT relay only `authorization` and `cookie` are
            // credentials, and `authorization` only in a credential's shape.
            (
                "ChatGPT",
                chatgpt,
                "/backend-api/me".into(),
                header("x-amz-anything", AWS_KEY.to_owned()),
            ),
            (
                "ChatGPT",
                chatgpt,
                "/backend-api/me".into(),
                header("x-goog-api-key", AWS_KEY.to_owned()),
            ),
            (
                "ChatGPT",
                chatgpt,
                "/backend-api/me".into(),
                header("authorization", format!("forward this: {AWS_KEY}")),
            ),
        ];
        for (provider, upstream, target, headers) in &refused {
            assert!(
                !admitted(provider, upstream, target, headers),
                "{provider} {upstream}{target}"
            );
        }
    }
}
