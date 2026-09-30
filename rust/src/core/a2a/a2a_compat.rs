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
    if req.jsonrpc != "2.0" {
        return JsonRpcResponse::error(req.id.clone(), -32600, "invalid jsonrpc version");
    }

    match req.method.as_str() {
        "message/send" | "tasks/send" => handle_send_message(req),
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
            TaskPart::Text { text } if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("");

    if description.is_empty() {
        return JsonRpcResponse::error(req.id.clone(), -32602, "message text is required");
    }

    let mut store = TaskStore::load();

    // `message/send` names an existing task in `message.taskId`; the legacy
    // `tasks/send` used `params.id`.
    let existing = message
        .and_then(|m| m.get("taskId"))
        .or_else(|| params.get("id"))
        .and_then(Value::as_str);
    let task_id = if let Some(id) = existing {
        if let Some(task) = store.get_task_mut(id) {
            if task.from_agent != A2A_CALLER {
                return JsonRpcResponse::error(req.id.clone(), -32602, "task not found");
            }
            task.add_message(role, parts);
            if task.state == TaskState::InputRequired {
                let _ = task.transition(TaskState::Working, Some("input received via A2A"));
            }
            id.to_string()
        } else {
            return JsonRpcResponse::error(req.id.clone(), -32602, "task not found");
        }
    } else {
        let id = store.create_task(A2A_CALLER, to_agent, description);
        // `Task::new` seeds the opening message with the originator id as its
        // role; an A2A message keeps the caller's role and all of its parts.
        if let Some(opening) = store
            .get_task_mut(&id)
            .and_then(|task| task.messages.first_mut())
        {
            opening.role = role.to_string();
            opening.parts = parts;
        }
        id
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
    // Local ctx_task work stays private to the local agents; an unknown and a
    // foreign id answer the same so the endpoint cannot enumerate them.
    match store
        .get_task(task_id)
        .filter(|task| task.from_agent == A2A_CALLER)
    {
        Some(task) => JsonRpcResponse::success(req.id.clone(), task_to_a2a_json(Some(task))),
        None => JsonRpcResponse::error(req.id.clone(), -32602, "task not found"),
    }
}

fn handle_cancel_task(req: &JsonRpcRequest) -> JsonRpcResponse {
    let Some(task_id) = req.params.get("id").and_then(Value::as_str) else {
        return JsonRpcResponse::error(req.id.clone(), -32602, "id is required");
    };

    let mut store = TaskStore::load();
    let Some(task) = store
        .get_task_mut(task_id)
        .filter(|task| task.from_agent == A2A_CALLER)
    else {
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

    fn send(method: &str, message: Value) -> JsonRpcResponse {
        let mut params = serde_json::Map::new();
        params.insert("message".to_string(), message);
        handle_a2a_jsonrpc(&make_request(method, Value::Object(params)))
    }

    fn task_id(resp: &JsonRpcResponse) -> String {
        resp.result.as_ref().expect("result")["id"]
            .as_str()
            .expect("task id")
            .to_string()
    }

    // #1913: `message.role` is the A2A user/agent enum, never the sender id.
    #[test]
    fn role_never_becomes_the_originating_agent() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let resp = send(
            "tasks/send",
            serde_json::json!({"role": "agent", "parts": [{"type": "text", "text": "x"}]}),
        );
        let id = task_id(&resp);
        let store = TaskStore::load();
        let task = store.get_task(&id).expect("stored task");
        assert_eq!(task.from_agent, A2A_CALLER);
        assert_eq!(task.messages.len(), 1);
        assert_eq!(task.messages[0].role, "agent");
    }

    #[test]
    fn rejects_role_outside_the_a2a_enum() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let resp = send(
            "message/send",
            serde_json::json!({"role": "cursor-agent-1", "parts": [{"kind": "text", "text": "x"}]}),
        );
        assert_eq!(resp.error.expect("error").code, -32602);
        assert!(TaskStore::load().tasks.is_empty());
    }

    #[test]
    fn message_send_accepts_kind_parts_and_task_id_follow_ups() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let first = send(
            "message/send",
            serde_json::json!({"role": "user", "parts": [{"kind": "text", "text": "review"}]}),
        );
        let id = task_id(&first);
        let follow_up = send(
            "message/send",
            serde_json::json!({
                "role": "user",
                "taskId": id,
                "parts": [{"kind": "text", "text": "also the tests"}]
            }),
        );
        assert_eq!(task_id(&follow_up), id);
        let store = TaskStore::load();
        assert_eq!(store.tasks.len(), 1);
        assert_eq!(store.get_task(&id).expect("task").messages.len(), 2);
    }

    // #1913: an A2A caller must not cancel or append to tasks that local
    // agents created through ctx_task.
    #[test]
    fn local_tasks_are_not_reachable_through_a2a_mutations() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let mut store = TaskStore::load();
        let id = store.create_task("cursor-agent-1", "codex-agent-2", "local work");
        store.save().expect("save");

        let get = handle_a2a_jsonrpc(&make_request("tasks/get", serde_json::json!({"id": id})));
        assert_eq!(get.error.expect("error").code, -32602);

        let cancel =
            handle_a2a_jsonrpc(&make_request("tasks/cancel", serde_json::json!({"id": id})));
        assert_eq!(cancel.error.expect("error").code, -32602);

        let append = send(
            "message/send",
            serde_json::json!({
                "role": "user",
                "taskId": id,
                "parts": [{"kind": "text", "text": "hijack"}]
            }),
        );
        assert_eq!(append.error.expect("error").code, -32602);

        let task = TaskStore::load().get_task(&id).cloned().expect("task");
        assert_eq!(task.state, TaskState::Created);
        assert_eq!(task.messages.len(), 1, "no A2A message was appended");
    }

    #[test]
    fn a2a_caller_can_cancel_its_own_task() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let resp = send(
            "message/send",
            serde_json::json!({"parts": [{"kind": "text", "text": "mine"}]}),
        );
        let id = task_id(&resp);
        let cancel =
            handle_a2a_jsonrpc(&make_request("tasks/cancel", serde_json::json!({"id": id})));
        assert_eq!(
            cancel.result.expect("result")["status"]["state"].as_str(),
            Some("canceled")
        );
    }
}
