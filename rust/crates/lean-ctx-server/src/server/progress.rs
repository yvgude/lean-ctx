// SPDX-License-Identifier: Apache-2.0
use rmcp::RoleServer;
use rmcp::model::{ProgressNotificationParam, ProgressToken};
use rmcp::service::Peer;

/// Sends MCP progress notifications to the client during long-running tool operations.
#[derive(Clone)]
pub struct ProgressSender {
    peer: Peer<RoleServer>,
    token: ProgressToken,
}

impl ProgressSender {
    pub fn new(peer: Peer<RoleServer>, token: ProgressToken) -> Self {
        Self { peer, token }
    }

    pub fn send(&self, progress: f64, total: Option<f64>, message: Option<String>) {
        // ProgressNotificationParam is #[non_exhaustive] since rmcp 2.0 — build via ctor.
        let mut params = ProgressNotificationParam::new(self.token.clone(), progress);
        if let Some(total) = total {
            params = params.with_total(total);
        }
        if let Some(message) = message {
            params = params.with_message(message);
        }
        let peer = self.peer.clone();
        tokio::spawn(async move {
            if let Err(e) = peer.notify_progress(params).await {
                tracing::debug!("[progress] notify failed: {e}");
            }
        });
    }
}

pub type SharedProgressSender = std::sync::Arc<std::sync::Mutex<Option<ProgressSender>>>;

/// Over a real MCP connection: progress belongs to the call that asked for it.
///
/// A `ctx_shell` call that waits on a long command reports progress every 5 s.
/// The sender used to sit in one server-wide slot that was set by any call
/// with a `progressToken` and never cleared, so a later call without a token
/// sent its progress under the token of a call that had already returned.
/// The MCP spec forbids that, and a strict host answers it by closing the
/// connection.
///
/// The host here speaks raw JSON-RPC: the rmcp client puts a token on every
/// request it sends, but real hosts send calls without one.
#[cfg(all(test, unix))]
mod over_mcp {
    use rmcp::ServiceExt;
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use crate::tools::LeanCtxServer;

    struct Host {
        lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
        writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    }

    impl Host {
        async fn send(&mut self, message: Value) {
            let mut line = message.to_string();
            line.push('\n');
            self.writer.write_all(line.as_bytes()).await.unwrap();
        }

        /// Reads until the response to `id`; returns it with the tokens of
        /// every progress notification that came before it.
        async fn answer(&mut self, id: u64) -> (Value, Vec<Value>) {
            let mut tokens = Vec::new();
            loop {
                let line = tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    self.lines.next_line(),
                )
                .await
                .expect("the server answers within a minute")
                .expect("readable")
                .expect("the connection stays open");
                let message: Value = serde_json::from_str(&line).expect("JSON-RPC");
                if message["method"] == "notifications/progress" {
                    tokens.push(message["params"]["progressToken"].clone());
                } else if message["id"] == id {
                    return (message, tokens);
                }
            }
        }

        async fn shell(
            &mut self,
            id: u64,
            command: &str,
            token: Option<u64>,
        ) -> (String, Vec<Value>) {
            let mut params = json!({ "name": "ctx_shell", "arguments": { "command": command } });
            if let Some(token) = token {
                params["_meta"] = json!({ "progressToken": token });
            }
            self.send(
                json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params }),
            )
            .await;
            let (response, tokens) = self.answer(id).await;
            assert!(
                response.get("error").is_none(),
                "a JSON-RPC answer, no error: {response}"
            );
            assert_ne!(
                response["result"]["isError"],
                json!(true),
                "no tool error: {response}"
            );
            let text = response["result"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|block| block["text"].as_str())
                .collect::<String>();
            (text, tokens)
        }
    }

    async fn connect(root: &str) -> Host {
        let server = LeanCtxServer::new_with_project_root(Some(root));
        let (server_io, client_io) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            if let Ok(running) = server.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        let (reader, writer) = tokio::io::split(client_io);
        let mut host = Host {
            lines: BufReader::new(reader).lines(),
            writer,
        };
        host.send(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test-host", "version": "1.0.0" }
            }
        }))
        .await;
        host.answer(1).await;
        host.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        host
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn progress_goes_only_to_the_call_that_asked_for_it() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        // Past one 5 s progress tick, short of the 6 s commands below: each
        // command reports once and then detaches at the cap.
        let _cap = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_FG_CAP_MS", "5600");
        let _security = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_SECURITY", "off");
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join(".git")).unwrap();
        let mut host = connect(&project.path().to_string_lossy()).await;

        let (text, tokens) = host.shell(2, "sleep 6; echo with_token", Some(9)).await;
        assert!(
            text.contains("auto-background"),
            "detached at the cap, cleanly: {text}"
        );
        assert!(
            !tokens.is_empty(),
            "the call with a token hears its progress"
        );
        assert!(
            tokens.iter().all(|t| *t == json!(9)),
            "only under its own token: {tokens:?}"
        );

        let (text, tokens) = host.shell(3, "sleep 6; echo without_token", None).await;
        assert!(
            text.contains("auto-background"),
            "detached at the cap, cleanly: {text}"
        );
        assert!(
            tokens.is_empty(),
            "a call without a token sends no progress, least of all under the \
             finished call's token: {tokens:?}"
        );

        let (text, _) = host.shell(4, "echo still_connected", None).await;
        assert!(text.contains("still_connected"), "{text}");
    }

    #[test]
    fn the_default_foreground_cap_returns_before_hosts_abort_a_call() {
        // Hosts closed the connection at 105–108 s on `sleep 110`; the detach
        // result has to be on its way well before that.
        const { assert!(crate::tools::registered::ctx_shell::DEFAULT_FOREGROUND_SOFT_CAP_MS <= 90_000) };
    }
}
