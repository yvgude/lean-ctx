use std::time::{Duration, Instant};

const DEFAULT_RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const DEGRADED_MULTIPLIER: u32 = 2;
const MAX_RESPONSE_BODY_BYTES: u64 = 10 * 1024 * 1024;

#[cfg(test)]
pub(crate) mod redirect_tests;

#[derive(Debug, Clone)]
pub enum HttpOutcome {
    Success {
        status: u16,
        body: String,
        elapsed_ms: u64,
    },
    Timeout {
        elapsed_ms: u64,
        phase: String,
    },
    NetworkError {
        message: String,
        elapsed_ms: u64,
    },
    HttpError {
        status: u16,
        body: String,
        elapsed_ms: u64,
    },
}

pub struct HardenedClient {
    agent: ureq::Agent,
    provider_id: String,
}

pub fn hardened_agent() -> ureq::Agent {
    if let Some(remaining) = super::provenance::reuse_time_remaining() {
        return ureq::config::Config::builder()
            .tls_config(crate::core::http_client::platform_tls_config())
            .max_redirects(0)
            .timeout_global(Some(remaining.max(Duration::from_nanos(1))))
            .build()
            .into();
    }
    let multiplier = if crate::core::io_health::environment()
        == crate::core::io_health::IoEnvironment::Degraded
    {
        DEGRADED_MULTIPLIER
    } else {
        1
    };

    // Provider credentials include custom headers (for example PRIVATE-TOKEN)
    // that HTTP clients do not remove on cross-origin redirects. A configured
    // endpoint is the authority: require the operator to select its final URL.
    crate::core::http_client::ureq_agent_without_redirects(
        Some(DEFAULT_RESOLVE_TIMEOUT * multiplier),
        Some(DEFAULT_CONNECT_TIMEOUT * multiplier),
        Some(DEFAULT_RESPONSE_TIMEOUT * multiplier),
    )
}

impl HardenedClient {
    pub fn new(provider_id: &str) -> Self {
        Self {
            agent: hardened_agent(),
            provider_id: provider_id.to_owned(),
        }
    }

    pub fn get(&self, url: &str) -> HttpOutcome {
        let request = self.agent.get(url);
        self.execute_request(|| request.call())
    }

    pub fn get_with_headers(&self, url: &str, headers: &[(&str, &str)]) -> HttpOutcome {
        let mut request = self.agent.get(url);
        for &(key, value) in headers {
            request = request.header(key, value);
        }
        self.execute_request(|| request.call())
    }

    pub fn post(&self, url: &str, body: &str) -> HttpOutcome {
        let request = self.agent.post(url);
        self.execute_request(|| request.send(body))
    }

    pub fn post_with_headers(
        &self,
        url: &str,
        body: &str,
        headers: &[(&str, &str)],
    ) -> HttpOutcome {
        let mut request = self.agent.post(url);
        for &(key, value) in headers {
            request = request.header(key, value);
        }
        self.execute_request(|| request.send(body))
    }

    fn execute_request<F>(&self, request: F) -> HttpOutcome
    where
        F: FnOnce() -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    {
        let started = Instant::now();
        let response = match request() {
            Ok(response) => response,
            Err(error) => return self.error_outcome(error, elapsed_ms(started)),
        };
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            // Neither a Location value nor a server error body is safe context:
            // either may echo credentials, URLs or private response contents.
            return HttpOutcome::HttpError {
                status,
                body: String::new(),
                elapsed_ms: elapsed_ms(started),
            };
        }
        let body = match response
            .into_body()
            .into_with_config()
            .limit(MAX_RESPONSE_BODY_BYTES)
            .read_to_string()
        {
            Ok(body) => body,
            Err(error) => return self.error_outcome(error, elapsed_ms(started)),
        };
        let elapsed_ms = elapsed_ms(started);

        HttpOutcome::Success {
            status,
            body,
            elapsed_ms,
        }
    }

    fn error_outcome(&self, error: ureq::Error, elapsed_ms: u64) -> HttpOutcome {
        match error {
            ureq::Error::Timeout(timeout) => HttpOutcome::Timeout {
                elapsed_ms,
                phase: timeout.to_string(),
            },
            ureq::Error::StatusCode(status) => HttpOutcome::HttpError {
                status,
                body: String::new(),
                elapsed_ms,
            },
            error => HttpOutcome::NetworkError {
                message: format!("{}: {error}", self.provider_id),
                elapsed_ms,
            },
        }
    }
}

impl HttpOutcome {
    pub fn into_body(self) -> Result<String, String> {
        match self {
            Self::Success { body, .. } => Ok(body),
            Self::Timeout { phase, .. } => Err(format!("timeout during {phase}")),
            Self::NetworkError { message, .. } => Err(message),
            Self::HttpError { status, body, .. } => Err(format!("HTTP {status}: {body}")),
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    pub fn body(&self) -> Option<&str> {
        match self {
            Self::Success { body, .. } => Some(body),
            _ => None,
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        match self {
            Self::Success { elapsed_ms, .. }
            | Self::Timeout { elapsed_ms, .. }
            | Self::NetworkError { elapsed_ms, .. }
            | Self::HttpError { elapsed_ms, .. } => *elapsed_ms,
        }
    }

    pub fn into_result(self) -> Result<String, String> {
        match self {
            Self::Success { body, .. } => Ok(body),
            Self::Timeout { phase, .. } => Err(format!("HTTP request timed out during {phase}")),
            Self::NetworkError { message, .. } => Err(message),
            Self::HttpError { status, body, .. } => {
                if body.is_empty() {
                    Err(format!("HTTP request failed with status {status}"))
                } else {
                    Err(format!("HTTP request failed with status {status}: {body}"))
                }
            }
        }
    }
}

pub fn provider_get(provider_id: &str, url: &str) -> HttpOutcome {
    HardenedClient::new(provider_id).get(url)
}

pub fn provider_get_with_headers(
    provider_id: &str,
    url: &str,
    headers: &[(&str, &str)],
) -> HttpOutcome {
    HardenedClient::new(provider_id).get_with_headers(url, headers)
}

pub fn provider_post(provider_id: &str, url: &str, body: &str) -> HttpOutcome {
    HardenedClient::new(provider_id).post(url, body)
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_CONNECT_TIMEOUT, DEFAULT_RESOLVE_TIMEOUT, DEFAULT_RESPONSE_TIMEOUT,
        DEGRADED_MULTIPLIER, HardenedClient, HttpOutcome, hardened_agent,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    #[test]
    fn test_hardened_agent_creates_agent() {
        let _agent = hardened_agent();
    }

    #[test]
    fn test_http_outcome_success_is_success() {
        let outcome = success_outcome(12);
        assert!(outcome.is_success());
    }

    #[test]
    fn test_http_outcome_timeout_not_success() {
        let outcome = timeout_outcome(12);
        assert!(!outcome.is_success());
    }

    #[test]
    fn test_http_outcome_network_error_not_success() {
        let outcome = HttpOutcome::NetworkError {
            message: "offline".to_owned(),
            elapsed_ms: 12,
        };
        assert!(!outcome.is_success());
    }

    #[test]
    fn test_http_outcome_into_result_success() {
        assert_eq!(success_outcome(12).into_result(), Ok("payload".to_owned()));
    }

    #[test]
    fn test_http_outcome_into_result_timeout() {
        let error = timeout_outcome(12)
            .into_result()
            .expect_err("timeout expected");
        assert!(error.contains("timed out"));
        assert!(error.contains("connect"));
    }

    #[test]
    fn test_http_outcome_into_body_variants() {
        assert_eq!(success_outcome(12).into_body(), Ok("payload".to_owned()));
        assert_eq!(
            timeout_outcome(12).into_body(),
            Err("timeout during connect".to_owned())
        );
        assert_eq!(
            HttpOutcome::NetworkError {
                message: "offline".to_owned(),
                elapsed_ms: 12,
            }
            .into_body(),
            Err("offline".to_owned())
        );
        assert_eq!(
            HttpOutcome::HttpError {
                status: 503,
                body: "unavailable".to_owned(),
                elapsed_ms: 12,
            }
            .into_body(),
            Err("HTTP 503: unavailable".to_owned())
        );
    }

    #[test]
    fn test_get_with_headers() {
        let (get_url, get_request) = serve_once();
        let client = HardenedClient::new("test");
        assert!(
            client
                .get_with_headers(&get_url, &[("X-Test-Header", "get-value")])
                .is_success()
        );
        assert!(
            get_request
                .recv()
                .expect("GET request expected")
                .to_ascii_lowercase()
                .contains("x-test-header: get-value")
        );
    }

    #[test]
    fn test_http_outcome_body_extraction() {
        let success = success_outcome(12);
        let error = timeout_outcome(12);
        assert_eq!(success.body(), Some("payload"));
        assert_eq!(error.body(), None);
    }

    #[test]
    fn test_http_outcome_elapsed_tracking() {
        let outcomes = [
            success_outcome(1),
            timeout_outcome(2),
            HttpOutcome::NetworkError {
                message: "offline".to_owned(),
                elapsed_ms: 3,
            },
            HttpOutcome::HttpError {
                status: 503,
                body: "unavailable".to_owned(),
                elapsed_ms: 4,
            },
        ];
        let elapsed: Vec<u64> = outcomes.iter().map(HttpOutcome::elapsed_ms).collect();
        assert_eq!(elapsed, [1, 2, 3, 4]);
    }

    #[test]
    fn test_degraded_multiplier_value() {
        assert_eq!(DEGRADED_MULTIPLIER, 2);
    }

    #[test]
    fn test_default_timeouts_reasonable() {
        assert_eq!(DEFAULT_RESOLVE_TIMEOUT, Duration::from_secs(5));
        assert_eq!(DEFAULT_CONNECT_TIMEOUT, Duration::from_secs(10));
        assert_eq!(DEFAULT_RESPONSE_TIMEOUT, Duration::from_secs(30));
    }

    fn success_outcome(elapsed_ms: u64) -> HttpOutcome {
        HttpOutcome::Success {
            status: 200,
            body: "payload".to_owned(),
            elapsed_ms,
        }
    }

    fn timeout_outcome(elapsed_ms: u64) -> HttpOutcome {
        HttpOutcome::Timeout {
            elapsed_ms,
            phase: "connect".to_owned(),
        }
    }

    fn serve_once() -> (String, Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test listener should bind");
        let address = listener
            .local_addr()
            .expect("test listener address should exist");
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request should connect");
            let mut buffer = [0_u8; 4096];
            let length = stream
                .read(&mut buffer)
                .expect("request should be readable");
            sender
                .send(String::from_utf8_lossy(&buffer[..length]).into_owned())
                .expect("request should be received");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .expect("response should be writable");
        });
        (format!("http://{address}/test"), receiver)
    }
}
