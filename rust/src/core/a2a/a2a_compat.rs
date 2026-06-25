use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::task::{Task, TaskMessage, TaskPart, TaskState, TaskStore};

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
        }
    }
}

/// Handle a JSON-RPC 2.0 A2A protocol request.
/// Supported methods: tasks/send, tasks/get, tasks/cancel
#[must_use]
pub fn handle_a2a_jsonrpc(req: &JsonRpcRequest) -> JsonRpcResponse {
    if req.jsonrpc != "2.0" {
        return JsonRpcResponse::error(req.id.clone(), -32600, "invalid jsonrpc version");
    }

    match req.method.as_str() {
        "tasks/send" => handle_send_message(req),
        "tasks/get" => handle_get_task(req),
        "tasks/cancel" => handle_cancel_task(req),
        _ => JsonRpcResponse::error(
            req.id.clone(),
            -32601,
            &format!("method not found: {}", req.method),
        ),
    }
}

fn handle_send_message(req: &JsonRpcRequest) -> JsonRpcResponse {
    let params = &req.params;

    let from_agent = params
        .get("message")
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        .unwrap_or("anonymous");
    let to_agent = params
        .get("to")
        .and_then(Value::as_str)
        .unwrap_or("lean-ctx");
    let description = params
        .get("message")
        .and_then(|m| m.get("parts"))
        .and_then(|p| p.as_array())
        .and_then(|parts| parts.first())
        .and_then(|p| p.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");

    if description.is_empty() {
        return JsonRpcResponse::error(req.id.clone(), -32602, "message text is required");
    }

    let mut store = TaskStore::load();

    let task_id = if let Some(id) = params.get("id").and_then(Value::as_str) {
        if let Some(task) = store.get_task_mut(id) {
            let parts = extract_message_parts(params);
            task.add_message(from_agent, parts);
            if task.state == TaskState::InputRequired {
                let _ = task.transition(TaskState::Working, Some("input received via A2A"));
            }
            id.to_string()
        } else {
            return JsonRpcResponse::error(req.id.clone(), -32602, "task not found");
        }
    } else {
        store.create_task(from_agent, to_agent, description)
    };

    let _ = store.save();

    let task = store.get_task(&task_id);
    JsonRpcResponse::success(req.id.clone(), task_to_a2a_json(task))
}

fn handle_get_task(req: &JsonRpcRequest) -> JsonRpcResponse {
    let Some(task_id) = req.params.get("id").and_then(Value::as_str) else {
        return JsonRpcResponse::error(req.id.clone(), -32602, "id is required");
    };

    let store = TaskStore::load();
    match store.get_task(task_id) {
        Some(task) => JsonRpcResponse::success(req.id.clone(), task_to_a2a_json(Some(task))),
        None => JsonRpcResponse::error(req.id.clone(), -32602, "task not found"),
    }
}

fn handle_cancel_task(req: &JsonRpcRequest) -> JsonRpcResponse {
    let Some(task_id) = req.params.get("id").and_then(Value::as_str) else {
        return JsonRpcResponse::error(req.id.clone(), -32602, "id is required");
    };

    let mut store = TaskStore::load();
    let Some(task) = store.get_task_mut(task_id) else {
        return JsonRpcResponse::error(req.id.clone(), -32602, "task not found");
    };

    if let Err(e) = task.transition(TaskState::Canceled, Some("canceled via A2A")) {
        return JsonRpcResponse::error(req.id.clone(), -32603, &e);
    }
    let _ = store.save();

    let task = store.get_task(task_id);
    JsonRpcResponse::success(req.id.clone(), task_to_a2a_json(task))
}

fn task_to_a2a_json(task: Option<&Task>) -> Value {
    let Some(task) = task else {
        return Value::Null;
    };

    let messages: Vec<Value> = task.messages.iter().map(message_to_a2a_json).collect();

    let artifacts: Vec<Value> = task.artifacts.iter().map(part_to_a2a_json).collect();

    let history: Vec<Value> = task
        .history
        .iter()
        .map(|h| {
            serde_json::json!({
                "from": h.from.to_string(),
                "to": h.to.to_string(),
                "timestamp": h.timestamp.to_rfc3339(),
                "reason": h.reason,
            })
        })
        .collect();

    serde_json::json!({
        "id": task.id,
        "status": {
            "state": task.state.to_string(),
            "timestamp": task.updated_at.to_rfc3339(),
        },
        "messages": messages,
        "artifacts": artifacts,
        "history": history,
        "metadata": task.metadata,
    })
}

fn message_to_a2a_json(m: &TaskMessage) -> Value {
    let parts: Vec<Value> = m.parts.iter().map(part_to_a2a_json).collect();
    serde_json::json!({
        "role": m.role,
        "parts": parts,
        "timestamp": m.timestamp.to_rfc3339(),
    })
}

fn part_to_a2a_json(p: &TaskPart) -> Value {
    match p {
        TaskPart::Text { text } => serde_json::json!({"type": "text", "text": text}),
        TaskPart::Data { mime_type, data } => {
            serde_json::json!({"type": "data", "mimeType": mime_type, "data": data})
        }
        TaskPart::File {
            name,
            mime_type,
            data,
            uri,
        } => serde_json::json!({
            "type": "file",
            "file": {
                "name": name,
                "mimeType": mime_type,
                "bytes": data,
                "uri": uri,
            }
        }),
    }
}

fn extract_message_parts(params: &Value) -> Vec<TaskPart> {
    params
        .get("message")
        .and_then(|m| m.get("parts"))
        .and_then(|p| p.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| {
                    let ptype = p.get("type")?.as_str()?;
                    match ptype {
                        "text" => Some(TaskPart::Text {
                            text: p.get("text")?.as_str()?.to_string(),
                        }),
                        "data" => Some(TaskPart::Data {
                            mime_type: p
                                .get("mimeType")
                                .and_then(Value::as_str)
                                .unwrap_or("application/octet-stream")
                                .to_string(),
                            data: p.get("data")?.as_str()?.to_string(),
                        }),
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(method: &str, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Value::Number(1.into()),
            method: method.to_string(),
            params,
        }
    }

    #[test]
    fn rejects_unknown_method() {
        let req = make_request("tasks/unknown", serde_json::json!({}));
        let resp = handle_a2a_jsonrpc(&req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[test]
    fn rejects_missing_message_text() {
        let req = make_request(
            "tasks/send",
            serde_json::json!({
                "message": { "role": "user", "parts": [] }
            }),
        );
        let resp = handle_a2a_jsonrpc(&req);
        assert!(resp.error.is_some());
    }

    #[test]
    fn send_creates_task() {
        let req = make_request(
            "tasks/send",
            serde_json::json!({
                "to": "lean-ctx",
                "message": {
                    "role": "user",
                    "parts": [{"type": "text", "text": "Fix the auth bug"}]
                }
            }),
        );
        let resp = handle_a2a_jsonrpc(&req);
        assert!(resp.result.is_some());
        let result = resp.result.unwrap();
        assert!(result.get("id").is_some());
        assert_eq!(
            result.get("status").unwrap().get("state").unwrap().as_str(),
            Some("created")
        );
    }

    #[test]
    fn get_nonexistent_task_returns_error() {
        let req = make_request(
            "tasks/get",
            serde_json::json!({"id": "nonexistent-task-id"}),
        );
        let resp = handle_a2a_jsonrpc(&req);
        assert!(resp.error.is_some());
    }
}
