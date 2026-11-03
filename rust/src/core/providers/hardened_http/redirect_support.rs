// SPDX-License-Identifier: Apache-2.0
//! A real loopback HTTP server for provider tests, shared with the server
//! crate's tests.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

pub const CANARY: &str = "synthetic-provider-credential";

pub struct Server {
    pub url: String,
    requests: Arc<Mutex<Vec<String>>>,
    response: Arc<Mutex<String>>,
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    pub fn new(status: u16, location: Option<&str>, body: &str) -> Self {
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

    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// The request line (`GET /path?query HTTP/1.1`) of the latest request.
    pub fn last_request_line(&self) -> Option<String> {
        let requests = self.requests.lock().unwrap();
        requests
            .last()
            .and_then(|request| request.lines().next().map(str::to_owned))
    }

    pub fn respond(&self, status: u16, body: &str) {
        *self.response.lock().unwrap() = format!(
            "HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
    }

    pub fn received_canary(&self) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.contains(CANARY))
    }

    pub fn received_body(&self, body: &str) -> bool {
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
