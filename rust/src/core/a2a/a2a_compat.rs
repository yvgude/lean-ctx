use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

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

    pub fn server_error(id: Value, message: &str) -> Self {
        Self::error(id, -32603, message)
    }
}

/// The originating agent id of every task created over `/a2a` (#1913).
///
/// The endpoint authenticates one principal — the bearer-token holder — and
/// has no per-agent identity. `message.role` is the A2A `"user" | "agent"`
/// enum chosen by the caller, so it can never name the sender.
pub const A2A_CALLER: &str = "a2a-client";

/// Handle a JSON-RPC 2.0 A2A protocol request.
/// Supported methods: message/send (and its legacy name tasks/send),
/// tasks/get, tasks/cancel.
pub fn handle_a2a_jsonrpc(req: &JsonRpcRequest) -> JsonRpcResponse {
    let path = match TaskStore::default_path() {
        Ok(path) => path,
        Err(error) => return storage_error(req, &error),
    };
    handle_a2a_jsonrpc_at_path(req, &path)
}

pub fn handle_a2a_jsonrpc_at_path(req: &JsonRpcRequest, path: &Path) -> JsonRpcResponse {
    if req.jsonrpc != "2.0" {
        return JsonRpcResponse::error(req.id.clone(), -32600, "invalid jsonrpc version");
    }

    match req.method.as_str() {
        "message/send" | "tasks/send" => handle_send_message(req, path),
        "tasks/get" => handle_get_task(req, path),
        "tasks/cancel" => handle_cancel_task(req, path),
        _ => JsonRpcResponse::error(
            req.id.clone(),
            -32601,
            &format!("method not found: {}", req.method),
        ),
    }
}

/// A task this unauthenticated legacy route may read or mutate: created over
/// `/a2a` (not local `ctx_task` work, #1913) and not bound to a signed remote
/// principal. Unknown and foreign ids answer identically so the endpoint
/// cannot enumerate either kind.
fn reachable_over_a2a(task: &Task) -> bool {
    task.from_agent == A2A_CALLER && task.authority_key_id.is_none()
}

fn task_not_found(req: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::error(req.id.clone(), -32602, "task not found")
}

fn handle_send_message(req: &JsonRpcRequest, path: &Path) -> JsonRpcResponse {
    let params = &req.params;
    let message = params.get("message");

    let role = match message.and_then(|m| m.get("role")).and_then(Value::as_str) {
        None => "user",
        Some(role @ ("user" | "agent")) => role,
        Some(_) => {
            return JsonRpcResponse::error(
                req.id.clone(),
                -32602,
                "message.role must be \"user\" or \"agent\"",
            );
        }
    };
    let to_agent = params
        .get("to")
        .and_then(Value::as_str)
        .unwrap_or("lean-ctx");
    let parts = extract_message_parts(params);
    let description = parts
        .iter()
        .find_map(|part| match part {
            TaskPart::Text { text } if !text.is_empty() => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();

    if description.is_empty() {
        return JsonRpcResponse::error(req.id.clone(), -32602, "message text is required");
    }

    // `message/send` names an existing task in `message.taskId`; the legacy
    // `tasks/send` used `params.id`.
    let existing = message
        .and_then(|m| m.get("taskId"))
        .or_else(|| params.get("id"))
        .and_then(Value::as_str);

    match TaskStore::mutate_locked(path, |store| {
        let task_id = if let Some(id) = existing {
            let Some(task) = store
                .get_task_mut(id)
                .filter(|task| reachable_over_a2a(task))
            else {
                return Err(LegacyMutationError::rejected(task_not_found(req)));
            };
            task.add_message(role, parts);
            if task.state == TaskState::InputRequired {
                task.transition(TaskState::Working, Some("input received via A2A"))
                    .map_err(std::io::Error::other)?;
            }
            id.to_string()
        } else {
            let id = store.create_task(A2A_CALLER, to_agent, &description);
            // `Task::new` seeds the opening message with the originator id as
            // its role; an A2A message keeps the caller's role and all parts.
            if let Some(opening) = store
                .get_task_mut(&id)
                .and_then(|task| task.messages.first_mut())
            {
                opening.role = role.to_string();
                opening.parts = parts;
            }
            id
        };
        Ok(JsonRpcResponse::success(
            req.id.clone(),
            task_to_a2a_json(store.get_task(&task_id)),
        ))
    }) {
        Ok(response) => response,
        Err(LegacyMutationError::Rejected(response)) => *response,
        Err(LegacyMutationError::Storage(error)) => storage_error(req, &error),
    }
}

fn handle_get_task(req: &JsonRpcRequest, path: &Path) -> JsonRpcResponse {
    let Some(task_id) = req.params.get("id").and_then(Value::as_str) else {
        return JsonRpcResponse::error(req.id.clone(), -32602, "id is required");
    };

    match TaskStore::read_locked(path, |store| {
        store
            .get_task(task_id)
            .filter(|task| reachable_over_a2a(task))
            .map(|task| task_to_a2a_json(Some(task)))
    }) {
        Ok(Some(task)) => JsonRpcResponse::success(req.id.clone(), task),
        Ok(None) => task_not_found(req),
        Err(error) => storage_error(req, &error),
    }
}

fn handle_cancel_task(req: &JsonRpcRequest, path: &Path) -> JsonRpcResponse {
    let Some(task_id) = req.params.get("id").and_then(Value::as_str) else {
        return JsonRpcResponse::error(req.id.clone(), -32602, "id is required");
    };

    match TaskStore::mutate_locked(path, |store| {
        let Some(task) = store
            .get_task_mut(task_id)
            .filter(|task| reachable_over_a2a(task))
        else {
            return Err(LegacyMutationError::rejected(task_not_found(req)));
        };
        if let Err(error) = task.transition(TaskState::Canceled, Some("canceled via A2A")) {
            return Err(LegacyMutationError::rejected(JsonRpcResponse::error(
                req.id.clone(),
                -32603,
                &error,
            )));
        }
        Ok(JsonRpcResponse::success(
            req.id.clone(),
            task_to_a2a_json(Some(task)),
        ))
    }) {
        Ok(response) => response,
        Err(LegacyMutationError::Rejected(response)) => *response,
        Err(LegacyMutationError::Storage(error)) => storage_error(req, &error),
    }
}

// Reject inside the locked mutation so TaskStore does not save a denial.
enum LegacyMutationError {
    Storage(std::io::Error),
    Rejected(Box<JsonRpcResponse>),
}

impl LegacyMutationError {
    fn rejected(response: JsonRpcResponse) -> Self {
        Self::Rejected(Box::new(response))
    }
}

impl From<std::io::Error> for LegacyMutationError {
    fn from(error: std::io::Error) -> Self {
        Self::Storage(error)
    }
}

fn storage_error(req: &JsonRpcRequest, error: &std::io::Error) -> JsonRpcResponse {
    tracing::warn!("A2A task store error: {error}");
    JsonRpcResponse::error(req.id.clone(), -32603, "task storage unavailable")
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
        "status": serde_json::to_value(task.status_v1()).unwrap_or_default(),
        "messages": messages,
        "artifacts": artifacts,
        "history": history,
        "metadata": task.metadata,
        "tenant_id": task.tenant_id,
        "project_id": task.project_id,
        "action": task.action,
        "idempotency_key": task.idempotency_key,
        "artifact_refs": task.artifact_refs,
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
                    // A2A 0.2+ names the discriminator `kind`; 0.1 used `type`.
                    let ptype = p.get("kind").or_else(|| p.get("type"))?.as_str()?;
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

    fn send_at(path: &Path, method: &str, message: Value) -> JsonRpcResponse {
        let mut params = serde_json::Map::new();
        params.insert("message".to_string(), message);
        handle_a2a_jsonrpc_at_path(&make_request(method, Value::Object(params)), path)
    }

    fn task_id(resp: &JsonRpcResponse) -> String {
        resp.result.as_ref().expect("result")["id"]
            .as_str()
            .expect("task id")
            .to_string()
    }

    fn stored_tasks(path: &Path) -> Vec<Task> {
        TaskStore::read_locked(path, |store| store.tasks.clone()).unwrap()
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
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
        let resp = handle_a2a_jsonrpc_at_path(&req, &path);
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
        let dir = tempfile::tempdir().unwrap();
        let req = make_request(
            "tasks/get",
            serde_json::json!({"id": "nonexistent-task-id"}),
        );
        let resp = handle_a2a_jsonrpc_at_path(&req, &dir.path().join("tasks.json"));
        assert!(resp.error.is_some());
    }

    #[test]
    fn scoped_send_get_cancel_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        let send = make_request(
            "tasks/send",
            serde_json::json!({
                "to": "server-a",
                "message": {
                    "role": "agent",
                    "parts": [{"type": "text", "text": "ship it"}]
                }
            }),
        );
        let task_id = task_id(&handle_a2a_jsonrpc_at_path(&send, &path));

        let get = make_request("tasks/get", serde_json::json!({"id": task_id.clone()}));
        let fetched = handle_a2a_jsonrpc_at_path(&get, &path);
        assert_eq!(
            fetched.result.unwrap()["status"]["state"],
            Value::String("created".to_string())
        );

        let cancel = make_request("tasks/cancel", serde_json::json!({"id": task_id}));
        let canceled = handle_a2a_jsonrpc_at_path(&cancel, &path);
        assert_eq!(
            canceled.result.unwrap()["status"]["state"],
            Value::String("canceled".to_string())
        );
    }

    // #1913: `message.role` is the A2A user/agent enum, never the sender id.
    #[test]
    fn role_never_becomes_the_originating_agent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        let resp = send_at(
            &path,
            "tasks/send",
            serde_json::json!({"role": "agent", "parts": [{"type": "text", "text": "x"}]}),
        );
        let id = task_id(&resp);
        let tasks = stored_tasks(&path);
        let task = tasks
            .iter()
            .find(|task| task.id == id)
            .expect("stored task");
        assert_eq!(task.from_agent, A2A_CALLER);
        assert_eq!(task.messages.len(), 1);
        assert_eq!(task.messages[0].role, "agent");
    }

    #[test]
    fn rejects_role_outside_the_a2a_enum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        let resp = send_at(
            &path,
            "message/send",
            serde_json::json!({"role": "cursor-agent-1", "parts": [{"kind": "text", "text": "x"}]}),
        );
        assert_eq!(resp.error.expect("error").code, -32602);
        assert!(!path.exists() || stored_tasks(&path).is_empty());
    }

    #[test]
    fn message_send_accepts_kind_parts_and_task_id_follow_ups() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        let first = send_at(
            &path,
            "message/send",
            serde_json::json!({"role": "user", "parts": [{"kind": "text", "text": "review"}]}),
        );
        let id = task_id(&first);
        let follow_up = send_at(
            &path,
            "message/send",
            serde_json::json!({
                "role": "user",
                "taskId": id,
                "parts": [{"kind": "text", "text": "also the tests"}]
            }),
        );
        assert_eq!(task_id(&follow_up), id);
        let tasks = stored_tasks(&path);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].messages.len(), 2);
    }

    // #1913: an A2A caller must not read, cancel or append to tasks that
    // local agents created through ctx_task.
    #[test]
    fn local_tasks_are_not_reachable_through_a2a_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        let id = TaskStore::mutate_locked(&path, |store| {
            Ok::<_, std::io::Error>(store.create_task("cursor-agent-1", "codex-agent-2", "local"))
        })
        .unwrap();
        let before = std::fs::read(&path).unwrap();

        let get = make_request("tasks/get", serde_json::json!({"id": id}));
        assert_eq!(
            handle_a2a_jsonrpc_at_path(&get, &path)
                .error
                .expect("error")
                .code,
            -32602
        );
        let cancel = make_request("tasks/cancel", serde_json::json!({"id": id}));
        assert_eq!(
            handle_a2a_jsonrpc_at_path(&cancel, &path)
                .error
                .expect("error")
                .code,
            -32602
        );
        let append = send_at(
            &path,
            "message/send",
            serde_json::json!({
                "role": "user",
                "taskId": id,
                "parts": [{"kind": "text", "text": "hijack"}]
            }),
        );
        assert_eq!(append.error.expect("error").code, -32602);

        assert_eq!(std::fs::read(&path).unwrap(), before, "denials never save");
        let task = stored_tasks(&path).remove(0);
        assert_eq!(task.state, TaskState::Created);
        assert_eq!(task.messages.len(), 1, "no A2A message was appended");
    }

    #[test]
    fn legacy_route_cannot_read_or_mutate_signed_remote_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        // Even a task carrying the A2A caller id stays unreachable once it is
        // bound to a signed remote principal.
        let task_id = TaskStore::mutate_locked(&path, |store| {
            let id = store.create_task(A2A_CALLER, "server-a", "signed work");
            store.get_task_mut(&id).unwrap().authority_key_id = Some("key-a".to_string());
            Ok::<_, std::io::Error>(id)
        })
        .unwrap();

        let before_get = std::fs::read(&path).unwrap();
        let get = make_request("tasks/get", serde_json::json!({"id": task_id}));
        let unknown = make_request("tasks/get", serde_json::json!({"id": "unknown"}));
        let denied = handle_a2a_jsonrpc_at_path(&get, &path);
        let missing = handle_a2a_jsonrpc_at_path(&unknown, &path);
        assert!(denied.result.is_none());
        assert_eq!(
            serde_json::to_value(&denied).unwrap(),
            serde_json::to_value(&missing).unwrap()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before_get);

        let append = make_request(
            "tasks/send",
            serde_json::json!({
                "id": task_id,
                "message": {
                    "role": "agent",
                    "parts": [{"type": "text", "text": "tamper"}]
                }
            }),
        );
        let denied_append = handle_a2a_jsonrpc_at_path(&append, &path);
        let mut missing_params = append.params.clone();
        missing_params["id"] = serde_json::json!("unknown");
        let missing_append =
            handle_a2a_jsonrpc_at_path(&make_request("tasks/send", missing_params), &path);
        assert_eq!(
            serde_json::to_value(&denied_append).unwrap(),
            serde_json::to_value(&missing_append).unwrap()
        );
        assert_eq!(denied_append.error.unwrap().code, -32602);

        let cancel = make_request("tasks/cancel", serde_json::json!({"id": task_id}));
        assert_eq!(std::fs::read(&path).unwrap(), before_get);
        let denied_cancel = handle_a2a_jsonrpc_at_path(&cancel, &path);
        let missing_cancel = handle_a2a_jsonrpc_at_path(
            &make_request("tasks/cancel", serde_json::json!({"id": "unknown"})),
            &path,
        );
        assert_eq!(
            serde_json::to_value(&denied_cancel).unwrap(),
            serde_json::to_value(&missing_cancel).unwrap()
        );
        assert_eq!(denied_cancel.error.unwrap().code, -32602);
        assert_eq!(std::fs::read(&path).unwrap(), before_get);
        TaskStore::read_locked(&path, |store| {
            let task = store.get_task(&task_id).unwrap();
            assert_eq!(task.state, TaskState::Created);
            assert_eq!(task.messages.len(), 1);
        })
        .unwrap();
    }

    #[test]
    fn a2a_caller_can_cancel_its_own_task() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        let resp = send_at(
            &path,
            "message/send",
            serde_json::json!({"parts": [{"kind": "text", "text": "mine"}]}),
        );
        let id = task_id(&resp);
        let cancel = make_request("tasks/cancel", serde_json::json!({"id": id}));
        assert_eq!(
            handle_a2a_jsonrpc_at_path(&cancel, &path)
                .result
                .expect("result")["status"]["state"]
                .as_str(),
            Some("canceled")
        );
    }

    #[test]
    fn corrupt_store_returns_server_error_without_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        std::fs::write(&path, b"broken").unwrap();
        let req = make_request("tasks/get", serde_json::json!({"id": "task-1"}));

        let response = handle_a2a_jsonrpc_at_path(&req, &path);

        assert_eq!(response.error.unwrap().code, -32603);
        assert_eq!(std::fs::read(path).unwrap(), b"broken");
    }
}
