use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub tool_calls: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    #[serde(default)]
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub functions: Option<Vec<Value>>,
    #[serde(default)]
    pub function_call: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicRequest {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub system: Option<Value>,
    pub messages: Vec<AnthropicMessage>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String, // "function"
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerationRequest {
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub response_format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdsMediaRequest {
    pub kind: String,
    pub request_id: String,
    pub slug: String,
    pub prompt: String,
    pub orientation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdsMediaResult {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub data_base64: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub ext: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdsMediaResponse {
    pub ok: bool,
    #[serde(default)]
    pub result: Option<UdsMediaResult>,
    #[serde(default)]
    pub error: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdsInferenceRequest {
    pub kind: String,
    pub request_id: String,
    pub slug: String,
    pub prompt: String,
    pub timeout_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdsUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdsResult {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub content: Option<Value>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Option<UdsUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdsResponse {
    pub ok: bool,
    #[serde(default)]
    pub result: Option<UdsResult>,
    #[serde(default)]
    pub error: Option<Value>,
}
