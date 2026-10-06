// SPDX-License-Identifier: Apache-2.0
//! A previous successful response cannot authorize a new source acquisition.
use super::*;
use crate::core::providers::cache;
use std::net::TcpListener;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::Duration;

/// Reject TLS before any credential can be transmitted; count actual attempts.
struct UnavailableSource {
    host: String,
    attempts: Arc<AtomicUsize>,
    stop: mpsc::Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl UnavailableSource {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let host = listener.local_addr().unwrap().to_string();
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&attempts);
        let (stop, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            while matches!(
                receiver.recv_timeout(Duration::from_millis(2)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        observed.fetch_add(1, Ordering::SeqCst);
                        drop(stream);
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock
                                | std::io::ErrorKind::Interrupted
                                | std::io::ErrorKind::ConnectionAborted
                        ) => {}
                    Err(error) => panic!("source fixture accept failed: {error}"),
                }
            }
        });
        Self {
            host,
            attempts,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for UnavailableSource {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            assert!(thread.join().is_ok() || std::thread::panicking());
        }
    }
}

#[test]
#[serial_test::serial(provider_request_cache)]
fn warm_and_stale_gitlab_results_cannot_replace_source_authorization() {
    for resource in ["issues", "merge_requests"] {
        for ttl in [120, 0] {
            let source = UnavailableSource::new();
            let config = GitLabConfig {
                host: source.host.clone(),
                token: "synthetic-authorization-test-token".into(),
                project_path: Some("5".into()),
            };
            let endpoint = if resource == "issues" {
                issues_endpoint("5", 1, None, None)
            } else {
                merge_requests_endpoint("5", 1, None)
            };
            let previous = ProviderResult {
                provider: "gitlab".into(),
                resource_type: resource.into(),
                total_count: Some(1),
                truncated: false,
                items: vec![ProviderItem {
                    id: "1".into(),
                    title: "previously-authorized-private-data".into(),
                    ..Default::default()
                }],
            };
            let key =
                cache::request_cache_key("gitlab", &config.api_url(&endpoint), Some(&config.token));
            cache::set_cached(&key, &serde_json::to_string(&previous).unwrap(), ttl);
            assert!(
                cache::get_cached(&key).is_some(),
                "positive control: old cache can serve this response"
            );
            let provider = GitLabProvider::with_config(config.clone());
            let params = ProviderParams {
                limit: Some(1),
                ..Default::default()
            };
            let error = provider
                .execute(resource, &params)
                .expect_err("source failure must not return earlier data");
            assert!(
                source.attempts.load(Ordering::SeqCst) >= 1,
                "must contact source despite a cached result"
            );
            assert!(!error.contains(&config.token));
            assert!(!error.contains("previously-authorized-private-data"));
            assert_eq!(provider.cache_ttl_secs(), 0);
        }
    }
}

#[test]
fn gitlab_configuration_debug_never_formats_the_credential() {
    let config = GitLabConfig {
        host: "gitlab.example.test".into(),
        token: "synthetic-debug-canary".into(),
        project_path: Some("5".into()),
    };
    for output in [
        format!("{config:?}"),
        format!("{config:#?}"),
        format!("{:?}", Some(config.clone())),
    ] {
        assert!(!output.contains(&config.token));
        assert!(output.contains("[REDACTED]"));
        assert!(output.contains(&config.host));
    }
}
