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
    #[must_use]
    pub fn new(peer: Peer<RoleServer>, token: ProgressToken) -> Self {
        Self { peer, token }
    }

    pub fn send(&self, progress: f64, total: Option<f64>, message: Option<String>) {
        let params = ProgressNotificationParam {
            progress_token: self.token.clone(),
            progress,
            total,
            message,
        };
        let peer = self.peer.clone();
        tokio::spawn(async move {
            if let Err(e) = peer.notify_progress(params).await {
                tracing::debug!("[progress] notify failed: {e}");
            }
        });
    }
}

pub type SharedProgressSender = std::sync::Arc<std::sync::Mutex<Option<ProgressSender>>>;
