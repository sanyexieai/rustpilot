use crate::openai_compat::{Message, Tool, ToolCall, ToolCallFunction, ToolChoice};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

const DEFAULT_MAX_TOKENS: u32 = 4096;

#[derive(Debug, Serialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Serialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: Vec<AnthropicContentBlock>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize)]
pub struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Deserialize)]
pub struct AnthropicResponse {
    #[serde(default)]
    pub content: Vec<AnthropicResponseBlock>,
}

#[derive(Debug, Clone)]
pub enum AnthropicResponseBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Unknown,
}

impl<'de> Deserialize<'de> for AnthropicResponseBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let Some(object) = value.as_object() else {
            return Ok(Self::Unknown);
        };
        let block_type = object
            .get("type")
            .and_then(|item| item.as_str())
            .unwrap_or_default();

        match block_type {
            "text" => Ok(Self::Text {
                text: extract_text_field(&value),
            }),
            "tool_use" => Ok(Self::ToolUse {
                id: object
                    .get("id")
                    .and_then(|item| item.as_str())
                    .unwrap_or_default()
                    .to_string(),
                name: object
                    .get("name")
                    .and_then(|item| item.as_str())
                    .unwrap_or_default()
                    .to_string(),
                input: object.get("input").cloned().unwrap_or_else(|| json!({})),
            }),
            _ => Ok(Self::Unknown),
        }
    }
}

fn extract_text_field(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return String::new();
    };

    if let Some(text) = object.get("text").and_then(|item| item.as_str()) {
        return text.to_string();
    }
    if let Some(text) = object.get("content").and_then(|item| item.as_str()) {
        return text.to_string();
    }
    if let Some(text) = object.get("thinking").and_then(|item| item.as_str()) {
        return text.to_string();
    }
    if let Some(text) = object
        .get("data")
        .and_then(|item| item.get("text"))
        .and_then(|item| item.as_str())
    {
        return text.to_string();
    }
    String::new()
}

pub fn build_request(
    model: &str,
    messages: &[Message],
    tools: Option<&[Tool]>,
    tool_choice: Option<&ToolChoice>,
    temperature: Option<f32>,
) -> AnthropicRequest {
    let system = extract_system_prompt(messages);
    let anthropic_messages = messages
        .iter()
        .filter_map(convert_message)
        .collect::<Vec<_>>();

    AnthropicRequest {
        model: model.to_string(),
        max_tokens: DEFAULT_MAX_TOKENS,
        system,
        messages: anthropic_messages,
        tools: tools.map(convert_tools).filter(|items| !items.is_empty()),
        tool_choice: tool_choice.and_then(convert_tool_choice),
        temperature,
    }
}

pub fn parse_response(response: AnthropicResponse) -> Message {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in response.content {
        match block {
            AnthropicResponseBlock::Text { text } => {
                if !text.is_empty() {
                    text_parts.push(text);
                }
            }
            AnthropicResponseBlock::ToolUse { id, name, input } => {
                tool_calls.push(ToolCall {
                    id,
                    r#type: "function".to_string(),
                    function: ToolCallFunction {
                        name,
                        arguments: serde_json::to_string(&input)
                            .unwrap_or_else(|_| "{}".to_string()),
                    },
                });
            }
            AnthropicResponseBlock::Unknown => {}
        }
    }

    Message {
        role: "assistant".to_string(),
        content: if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join("\n"))
        },
        tool_call_id: None,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
    }
}

fn extract_system_prompt(messages: &[Message]) -> Option<String> {
    let system_parts = messages
        .iter()
        .filter(|message| message.role == "system")
        .filter_map(|message| message.content.clone())
        .filter(|content| !content.trim().is_empty())
        .collect::<Vec<_>>();

    if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    }
}

fn convert_message(message: &Message) -> Option<AnthropicMessage> {
    match message.role.as_str() {
        "system" => None,
        "user" => Some(AnthropicMessage {
            role: "user".to_string(),
            content: vec![AnthropicContentBlock::Text {
                text: message.content.clone().unwrap_or_default(),
            }],
        }),
        "assistant" => {
            let mut content = Vec::new();
            if let Some(text) = message.content.clone().filter(|text| !text.is_empty()) {
                content.push(AnthropicContentBlock::Text { text });
            }
            for tool_call in message.tool_calls.clone().unwrap_or_default() {
                let input = serde_json::from_str::<Value>(&tool_call.function.arguments)
                    .unwrap_or_else(|_| json!({}));
                content.push(AnthropicContentBlock::ToolUse {
                    id: tool_call.id,
                    name: tool_call.function.name,
                    input,
                });
            }
            if content.is_empty() {
                None
            } else {
                Some(AnthropicMessage {
                    role: "assistant".to_string(),
                    content,
                })
            }
        }
        "tool" => Some(AnthropicMessage {
            role: "user".to_string(),
            content: vec![AnthropicContentBlock::ToolResult {
                tool_use_id: message.tool_call_id.clone().unwrap_or_default(),
                content: message.content.clone().unwrap_or_default(),
            }],
        }),
        _ => message.content.clone().map(|text| AnthropicMessage {
            role: "user".to_string(),
            content: vec![AnthropicContentBlock::Text { text }],
        }),
    }
}

fn convert_tools(tools: &[Tool]) -> Vec<AnthropicTool> {
    tools
        .iter()
        .map(|tool| AnthropicTool {
            name: tool.function.name.clone(),
            description: tool.function.description.clone(),
            input_schema: tool.function.parameters.clone(),
        })
        .collect()
}

fn convert_tool_choice(tool_choice: &ToolChoice) -> Option<Value> {
    match tool_choice {
        ToolChoice::Auto(_) => Some(json!({ "type": "auto" })),
        ToolChoice::Named { function, .. } => {
            Some(json!({ "type": "tool", "name": function.name }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AnthropicResponse, parse_response};

    #[test]
    fn parse_response_ignores_unknown_blocks() {
        let response: AnthropicResponse = serde_json::from_str(
            r#"{
                "content": [
                    { "type": "thinking", "thinking": "internal" },
                    { "type": "text", "text": "OK" }
                ]
            }"#,
        )
        .expect("deserialize anthropic response");

        let message = parse_response(response);
        assert_eq!(message.content.as_deref(), Some("OK"));
    }

    #[test]
    fn parse_response_accepts_text_in_content_field() {
        let response: AnthropicResponse = serde_json::from_str(
            r#"{
                "content": [
                    { "type": "text", "content": "Compatible text payload" }
                ]
            }"#,
        )
        .expect("deserialize anthropic response");

        let message = parse_response(response);
        assert_eq!(message.content.as_deref(), Some("Compatible text payload"));
    }
}
