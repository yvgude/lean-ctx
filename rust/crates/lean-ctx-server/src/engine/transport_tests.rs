// SPDX-License-Identifier: Apache-2.0

use rmcp::{
    RoleServer,
    service::TxJsonRpcMessage,
    transport::{OneshotTransport, Transport},
};
use serde_json::json;
use std::time::Duration;

async fn check_terminal_delivery(message: serde_json::Value, disconnect: bool) {
    let request = serde_json::from_value(json!({
        "jsonrpc": "2.0", "id": 1, "method": "ping"
    }))
    .unwrap();
    let (mut transport, receiver) = OneshotTransport::<RoleServer>::new(request);
    assert!(transport.receive().await.is_some());
    let mut receiver = Some(receiver);
    if disconnect {
        receiver.take();
    }
    let terminal: TxJsonRpcMessage<RoleServer> = serde_json::from_value(message).unwrap();
    let expected = serde_json::to_value(&terminal).unwrap();
    let result = transport.send(terminal).await;
    if disconnect {
        assert_eq!(
            serde_json::to_value(result.unwrap_err().0).unwrap(),
            expected
        );
    } else {
        result.unwrap();
        let delivered = receiver.as_mut().unwrap().recv().await.unwrap();
        assert_eq!(serde_json::to_value(delivered).unwrap(), expected);
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(1), transport.receive())
            .await
            .expect("terminal send must close one-shot receive even on delivery failure")
            .is_none()
    );
}

#[tokio::test]
async fn terminal_response_closes_with_or_without_receiver() {
    for disconnect in [false, true] {
        check_terminal_delivery(json!({"jsonrpc": "2.0", "id": 1, "result": {}}), disconnect).await;
    }
}

#[tokio::test]
async fn terminal_error_closes_with_or_without_receiver() {
    for disconnect in [false, true] {
        check_terminal_delivery(
            json!({"jsonrpc": "2.0", "id": 1,
                "error": {"code": -32603, "message": "test failure"}}),
            disconnect,
        )
        .await;
    }
}
