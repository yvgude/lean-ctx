// SPDX-License-Identifier: Apache-2.0
//! Real loopback HTTP requests: redirects must never receive provider authority.
use super::{HardenedClient, HttpOutcome};
use crate::core::providers::config_provider::{http, schema::ResourceConfig};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

const CANARY: &str = "synthetic-provider-credential";

pub(crate) struct Server {
    pub(crate) url: String,
    requests: Arc<Mutex<Vec<String>>>,
    response: Arc<Mutex<String>>,
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub(crate) fn new(status: u16, location: Option<&str>, body: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let mut response = format!("HTTP/1.1 {status} Test\r\nConnection: close\r\n");
        if let Some(location) = location {
            response.push_str(&format!("Location: {location}\r\n"));
        }
        response.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        let response = Arc::new(Mutex::new(response));
        let next_response = Arc::clone(&response);
        let (stop, receiver) = mpsc::channel();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = Arc::clone(&requests);
        let thread = std::thread::spawn(move || {
            while matches!(
                receiver.recv_timeout(Duration::from_millis(2)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // macOS inherits the listener's O_NONBLOCK on accept.
                        // Header/body reads below need their bounded timeout.
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = Vec::new();
                        let mut byte = [0];
                        while !request.ends_with(b"\r\n\r\n") && request.len() < 16 * 1024 {
                            if !matches!(stream.read(&mut byte), Ok(1)) {
                                break;
                            }
                            request.push(byte[0]);
                        }
                        if !request.ends_with(b"\r\n\r\n") {
                            continue;
                        }
                        let header = String::from_utf8_lossy(&request);
                        let length = header
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        assert!(length <= 16 * 1024);
                        let mut body = vec![0; length];
                        if stream.read_exact(&mut body).is_err() {
                            continue;
                        }
                        request.extend(body);
                        received
                            .lock()
                            .unwrap()
                            .push(String::from_utf8(request).unwrap());
                        // Redirect rejection may close before reading the body.
                        let _ = stream.write_all(next_response.lock().unwrap().as_bytes());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("test accept failed: {error}"),
                }
            }
        });
        Self {
            url,
            requests,
            response,
            stop,
            thread: Some(thread),
        }
    }

    pub(crate) fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    pub(crate) fn respond(&self, status: u16, body: &str) {
        *self.response.lock().unwrap() = format!(
            "HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
    }

    fn received_canary(&self) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.contains(CANARY))
    }

    pub(crate) fn received_body(&self, body: &str) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.ends_with(&format!("\r\n\r\n{body}")))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            assert!(
                thread.join().is_ok() || std::thread::panicking(),
                "HTTP fixture thread failed"
            );
        }
    }
}

#[test]
fn builtin_provider_redirects_never_forward_headers_or_bodies() {
    let target = Server::new(200, None, "[]");
    for status in [301, 302, 303, 307, 308] {
        let origin = Server::new(status, Some(&target.url), CANARY);
        let client = HardenedClient::new("gitlab");
        for outcome in [
            client.get_with_headers(&origin.url, &[("PRIVATE-TOKEN", CANARY)]),
            client.post_with_headers(&origin.url, CANARY, &[("X-Api-Key", CANARY)]),
        ] {
            assert!(
                matches!(&outcome, HttpOutcome::HttpError { status: actual, body, .. } if *actual == status && body.is_empty())
            );
            assert!(!outcome.into_body().unwrap_err().contains(CANARY));
        }
        assert_eq!(origin.count(), 2);
        assert!(origin.received_canary());
        assert!(origin.received_body(CANARY));
        assert_eq!(
            target.count(),
            0,
            "redirect authority reached another origin"
        );
    }
}

#[test]
fn config_provider_redirects_never_forward_any_supported_method() {
    let target = Server::new(200, None, "[]");
    let auth = http::ResolvedAuth::CustomHeader {
        header: "PRIVATE-TOKEN".into(),
        value: CANARY.into(),
    };
    for status in [301, 302, 303, 307, 308] {
        let origin = Server::new(status, Some(&target.url), CANARY);
        for method in ["GET", "DELETE", "POST", "PUT", "PATCH"] {
            let resource: ResourceConfig = serde_json::from_value(serde_json::json!({
                "method": method, "path": "/issues", "response": {"mapping": {"id":"id", "title":"title"}}
            })).unwrap();
            let error =
                http::execute_request(&origin.url, &resource, &auth, &HashMap::new()).unwrap_err();
            assert!(error.contains(&status.to_string()), "{method}: {error}");
            assert!(!error.contains(CANARY));
        }
        assert_eq!(origin.count(), 5);
        assert!(origin.received_canary());
        assert_eq!(target.count(), 0);
    }
}

#[test]
fn direct_config_provider_requests_still_authenticate_and_return_json() {
    let origin = Server::new(200, None, "[{\"id\":1,\"title\":\"Login bug\"}]");
    for method in ["GET", "DELETE", "POST", "PUT", "PATCH"] {
        let resource: ResourceConfig = serde_json::from_value(serde_json::json!({
            "method": method, "path": "/issues", "response": {"mapping": {"id":"id", "title":"title"}}
        })).unwrap();
        let value = http::execute_request(
            &origin.url,
            &resource,
            &http::ResolvedAuth::ApiKeyQuery {
                param: "key".into(),
                value: CANARY.into(),
            },
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(value[0]["id"], 1);
    }
    assert_eq!(origin.count(), 5);
    assert!(origin.received_canary());
}
