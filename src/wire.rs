use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireEnvelope<T> {
    pub kind: String,
    pub payload: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireRequest {
    ConsoleInput {
        input: String,
    },
    ChatSend {
        input: String,
        focus: Option<String>,
    },
    ChatAbort,
    SessionCreate {
        label: Option<String>,
        focus: Option<String>,
    },
    SessionUse {
        session_id: String,
    },
    SessionList,
    ApprovalStatus,
    ApprovalHistory {
        limit: Option<usize>,
        reason: Option<String>,
    },
    ApprovalSet {
        mode: String,
    },
    ToolList,
    ToolCall {
        name: String,
        arguments_json: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireResponse {
    Ack {
        message: String,
    },
    SessionCreated {
        session_id: String,
        label: Option<String>,
        session: Option<WireSessionSummary>,
    },
    SessionList {
        sessions: Vec<WireSessionSummary>,
    },
    ApprovalStatus {
        mode: String,
        summary: String,
        allowed_tools: Vec<String>,
        last_block: Option<WireApprovalBlock>,
    },
    ApprovalHistory {
        items: Vec<WireApprovalBlock>,
    },
    ToolList {
        tools: Vec<WireToolSummary>,
    },
    ToolResult {
        name: String,
        output: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireEvent {
    MessageDelta {
        role: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    ToolStarted {
        name: String,
    },
    ToolFinished {
        name: String,
        ok: bool,
    },
    TaskUpdated {
        task_id: Option<u64>,
        status: String,
        summary: String,
    },
    SessionUpdated {
        focus: String,
        status: String,
        abortable: Option<bool>,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireSessionSummary {
    pub session_id: String,
    pub label: Option<String>,
    pub focus: Option<String>,
    pub status: Option<String>,
    pub abortable: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireToolSummary {
    pub name: String,
    pub source: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub capability_level: Option<String>,
    pub runtime_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireApprovalBlock {
    pub ts: u64,
    pub actor_id: String,
    pub tool_name: String,
    pub command: String,
    pub reason_code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "frame", rename_all = "snake_case")]
pub enum WireFrame {
    Response {
        response: WireEnvelope<WireResponse>,
    },
    Event {
        event: WireEnvelope<WireEvent>,
    },
}

impl<T> WireEnvelope<T> {
    pub fn new(kind: impl Into<String>, payload: T) -> Self {
        Self {
            kind: kind.into(),
            payload,
        }
    }
}

impl WireFrame {
    pub fn ack(message: impl Into<String>) -> Self {
        Self::Response {
            response: WireEnvelope::new(
                "response",
                WireResponse::Ack {
                    message: message.into(),
                },
            ),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Event {
            event: WireEnvelope::new(
                "event",
                WireEvent::Error {
                    message: message.into(),
                },
            ),
        }
    }

    pub fn session_updated(focus: impl Into<String>, status: impl Into<String>) -> Self {
        Self::session_updated_with_abortable(focus, status, None)
    }

    pub fn session_updated_with_abortable(
        focus: impl Into<String>,
        status: impl Into<String>,
        abortable: Option<bool>,
    ) -> Self {
        Self::Event {
            event: WireEnvelope::new(
                "event",
                WireEvent::SessionUpdated {
                    focus: focus.into(),
                    status: status.into(),
                    abortable,
                },
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{WireEnvelope, WireEvent, WireRequest, WireResponse, WireToolSummary};

    #[test]
    fn request_serializes_with_tagged_type() {
        let request = WireRequest::ConsoleInput {
            input: "hello".to_string(),
        };
        let text = serde_json::to_string(&request).expect("serialize request");
        assert!(text.contains("\"type\":\"console_input\""));
        assert!(text.contains("\"input\":\"hello\""));
    }

    #[test]
    fn chat_request_serializes_with_tagged_type() {
        let request = WireRequest::ChatSend {
            input: "hello".to_string(),
            focus: Some("lead".to_string()),
        };
        let text = serde_json::to_string(&request).expect("serialize request");
        assert!(text.contains("\"type\":\"chat_send\""));
        assert!(text.contains("\"input\":\"hello\""));
    }

    #[test]
    fn envelope_wraps_event_payload() {
        let event = WireEvent::SessionUpdated {
            focus: "lead".to_string(),
            status: "idle".to_string(),
            abortable: None,
        };
        let envelope = WireEnvelope::new("event", event);
        let text = serde_json::to_string(&envelope).expect("serialize envelope");
        assert!(text.contains("\"kind\":\"event\""));
        assert!(text.contains("\"type\":\"session_updated\""));
    }

    #[test]
    fn tool_list_response_serializes_with_tools() {
        let response = WireResponse::ToolList {
            tools: vec![WireToolSummary {
                name: "read_file".to_string(),
                source: "builtin".to_string(),
                description: "read a file".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    }
                }),
                capability_level: Some("kernel".to_string()),
                runtime_kind: Some("rust_binary".to_string()),
            }],
        };
        let text = serde_json::to_string(&response).expect("serialize tool list");
        assert!(text.contains("\"type\":\"tool_list\""));
        assert!(text.contains("\"name\":\"read_file\""));
        assert!(text.contains("\"source\":\"builtin\""));
        assert!(text.contains("\"capability_level\":\"kernel\""));
    }
}
