// SPDX-License-Identifier: Apache-2.0
use rmcp::model::{
    Notification, NotificationNoParam, ResourceUpdatedNotificationParam, ServerNotification,
    ToolListChangedNotificationMethod,
};
use rmcp::service::{Peer, RoleServer};

pub async fn send_resource_updated(peer: &Peer<RoleServer>, uri: &str) {
    let notif = Notification::new(ResourceUpdatedNotificationParam::new(uri));
    let server_notif = ServerNotification::ResourceUpdatedNotification(notif);
    if let Err(e) = peer.send_notification(server_notif).await {
        tracing::debug!("Failed to send resource updated notification: {e}");
    }
}

pub async fn send_tools_list_changed(peer: &Peer<RoleServer>) {
    let notif = NotificationNoParam {
        method: ToolListChangedNotificationMethod,
        extensions: rmcp::model::Extensions::default(),
    };
    let server_notif = ServerNotification::ToolListChangedNotification(notif);
    if let Err(e) = peer.send_notification(server_notif).await {
        tracing::debug!("Failed to send tools list changed notification: {e}");
    }
}

pub const RESOURCE_URI_SUMMARY: &str = "lean-ctx://context/summary";
pub const RESOURCE_URI_PRESSURE: &str = "lean-ctx://context/pressure";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_types_construct_correctly() {
        let notif = Notification::new(ResourceUpdatedNotificationParam::new(RESOURCE_URI_SUMMARY));
        let sn = ServerNotification::ResourceUpdatedNotification(notif);
        assert!(matches!(
            sn,
            ServerNotification::ResourceUpdatedNotification(_)
        ));

        let notif = NotificationNoParam {
            method: ToolListChangedNotificationMethod,
            extensions: rmcp::model::Extensions::default(),
        };
        let sn = ServerNotification::ToolListChangedNotification(notif);
        assert!(matches!(
            sn,
            ServerNotification::ToolListChangedNotification(_)
        ));
    }
}
