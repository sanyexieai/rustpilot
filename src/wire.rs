use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireEnvelope<T> {
    pub kind: String,
    pub payload: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireRequest {
    ChatSend {
        input: String,
        focus: Option<String>,
    },
    ChatAbort,
    SessionCreate {
        label: Option<String>,
    },
    SessionList,
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
    },
    SessionList {
        sessions: Vec<WireSessionSummary>,
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
}

impl<T> WireEnvelope<T> {
    pub fn new(kind: impl Into<String>, payload: T) -> Self {
        Self {
            kind: kind.into(),
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{WireEnvelope, WireEvent, WireRequest};

    #[test]
    fn request_serializes_with_tagged_type() {
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
        };
        let envelope = WireEnvelope::new("event", event);
        let text = serde_json::to_string(&envelope).expect("serialize envelope");
        assert!(text.contains("\"kind\":\"event\""));
        assert!(text.contains("\"type\":\"session_updated\""));
    }
}
