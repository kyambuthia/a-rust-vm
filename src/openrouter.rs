use std::env;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;

use crate::agent::{Model, ModelError, ModelRequest, ModelResponse, ToolCall};

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const DEFAULT_MODEL: &str = "~openai/gpt-latest";
const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_TOOL_ARGS_BYTES: usize = 32 * 1024;
const DEFAULT_MAX_TOKENS: u32 = 2048;
const MAX_MAX_TOKENS: u32 = 4096;

#[derive(Debug, Clone)]
pub struct OpenRouterConfig {
    api_key: String,
    pub model: String,
    pub site_url: Option<String>,
    pub site_title: Option<String>,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub max_tokens: u32,
}

impl OpenRouterConfig {
    pub fn from_env() -> Result<Self, String> {
        let api_key = env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "OPENROUTER_API_KEY is not set".to_owned())?;
        let model = env::var("OPENROUTER_MODEL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned());
        let site_url = env::var("OPENROUTER_SITE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| Some("https://asiliano.online".to_owned()));
        let site_title = env::var("OPENROUTER_SITE_TITLE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| Some("A/RVM".to_owned()));
        let timeout = env::var("OPENROUTER_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| (1..=120).contains(value))
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(30));
        let max_tokens = env::var("OPENROUTER_MAX_TOKENS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| (128..=MAX_MAX_TOKENS).contains(value))
            .unwrap_or(DEFAULT_MAX_TOKENS);
        Ok(Self {
            api_key,
            model,
            site_url,
            site_title,
            timeout,
            max_response_bytes: MAX_RESPONSE_BYTES,
            max_tokens,
        })
    }

    pub fn with_api_key_for_test(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            site_url: Some("https://asiliano.online".to_owned()),
            site_title: Some("A/RVM".to_owned()),
            timeout: Duration::from_secs(30),
            max_response_bytes: MAX_RESPONSE_BYTES,
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }
}

#[derive(Clone)]
pub struct OpenRouterModel {
    inner: Arc<Inner>,
}

struct Inner {
    client: reqwest::blocking::Client,
    config: OpenRouterConfig,
}

impl OpenRouterModel {
    pub fn from_env() -> Result<Self, String> {
        Self::from_config(OpenRouterConfig::from_env()?)
    }

    pub fn from_config(config: OpenRouterConfig) -> Result<Self, String> {
        if config.api_key.trim().is_empty() {
            return Err("OPENROUTER_API_KEY is not set".to_owned());
        }
        if config.model.trim().is_empty() {
            return Err("OPENROUTER_MODEL is empty".to_owned());
        }
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(config.timeout)
            .build()
            .map_err(|error| sanitize_error(&format!("failed to build http client: {error}")))?;
        Ok(Self {
            inner: Arc::new(Inner { client, config }),
        })
    }

    pub fn model_name(&self) -> &str {
        &self.inner.config.model
    }

    pub fn build_request_body(&self, request: &ModelRequest) -> serde_json::Value {
        build_request_body(&self.inner.config, request)
    }
}

impl Model for OpenRouterModel {
    fn respond(&mut self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        let body = self.build_request_body(request);
        let mut http_request = self
            .inner
            .client
            .post(OPENROUTER_URL)
            .header(
                "Authorization",
                format!("Bearer {}", self.inner.config.api_key),
            )
            .header("Content-Type", "application/json");
        if let Some(url) = &self.inner.config.site_url {
            http_request = http_request.header("HTTP-Referer", url.as_str());
        }
        if let Some(title) = &self.inner.config.site_title {
            http_request = http_request.header("X-OpenRouter-Title", title.as_str());
        }
        let response = http_request.json(&body).send().map_err(|error| {
            ModelError::new(sanitize_error(&format!("provider request failed: {error}")))
        })?;
        let status = response.status();
        let bytes = read_response_body(response, self.inner.config.max_response_bytes)
            .map_err(ModelError::new)?;
        if bytes.len() > self.inner.config.max_response_bytes {
            return Err(ModelError::new("provider response is too large".to_owned()));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| ModelError::new("provider response was not UTF-8".to_owned()))?;
        let text = redact_secret(&text, &self.inner.config.api_key);
        if !status.is_success() {
            return Err(ModelError::new(sanitize_provider_error(
                status.as_u16(),
                &text,
            )));
        }
        parse_chat_response(&text).map_err(ModelError::new)
    }
}

fn build_request_body(config: &OpenRouterConfig, request: &ModelRequest) -> serde_json::Value {
    let mut messages = Vec::new();
    let system = bounded_text(&request.system_prompt, 4096);
    if !system.trim().is_empty() {
        messages.push(serde_json::json!({"role":"system","content":system}));
    }
    for entry in &request.conversation {
        let role = normalize_role(&entry.role);
        let content = bounded_text(&entry.content, 4096);
        if role == "tool" {
            messages.push(serde_json::json!({
                "role":"user",
                "content": format!("Previous tool result:\n{content}")
            }));
        } else {
            messages.push(serde_json::json!({"role":role,"content":content}));
        }
    }
    messages.push(serde_json::json!({
        "role":"user",
        "content": bounded_text(&request.prompt, 8192)
    }));
    if !request.tool_results.is_empty() {
        let results = request
            .tool_results
            .iter()
            .map(|result| {
                let status = if result.is_error { "error" } else { "ok" };
                format!(
                    "id={} name={} status={status}\n{}",
                    result.id,
                    result.name,
                    bounded_text(&result.content, 4096)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        messages.push(serde_json::json!({
            "role": "user",
            "content": format!("Tool results from the previous step:\n{results}")
        }));
    }
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            let parameters = parse_tool_schema(&tool.input_schema);
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": bounded_text(&tool.description, 1024),
                    "parameters": parameters
                }
            })
        })
        .collect::<Vec<_>>();
    let mut body = serde_json::json!({
        "model": config.model,
        "messages": messages,
        "max_tokens": config.max_tokens
    });
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools);
        body["tool_choice"] = serde_json::json!("auto");
    }
    body
}

fn normalize_role(role: &str) -> &str {
    match role.trim().to_ascii_lowercase().as_str() {
        "assistant" => "assistant",
        "system" => "system",
        "tool" => "tool",
        _ => "user",
    }
}

fn bounded_text(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.to_owned()
    } else {
        value.chars().take(limit).collect()
    }
}

fn parse_tool_schema(raw: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(raw)
        .unwrap_or_else(|_| serde_json::json!({"type":"object","properties":{}}))
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Option<Vec<Choice>>,
    error: Option<ProviderError>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Option<ChatMessage>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<serde_json::Value>,
    tool_calls: Option<Vec<ToolCallEntry>>,
}

#[derive(Debug, Deserialize)]
struct ToolCallEntry {
    id: Option<String>,
    #[allow(dead_code)]
    #[serde(rename = "type")]
    call_type: Option<String>,
    function: Option<FunctionEntry>,
}

#[derive(Debug, Deserialize)]
struct FunctionEntry {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProviderError {
    message: Option<String>,
}

fn parse_chat_response(text: &str) -> Result<ModelResponse, String> {
    if text.len() > MAX_RESPONSE_BYTES {
        return Err("provider response is too large".to_owned());
    }
    let response: ChatResponse = serde_json::from_str(text)
        .map_err(|error| sanitize_error(&format!("invalid provider response: {error}")))?;
    if let Some(error) = response.error.and_then(|value| value.message) {
        return Err(sanitize_provider_error(400, &error));
    }
    let choices = response
        .choices
        .ok_or_else(|| "provider response has no choices".to_owned())?;
    let choice = choices
        .into_iter()
        .next()
        .ok_or_else(|| "provider response has no choices".to_owned())?;
    let message = choice
        .message
        .ok_or_else(|| "provider response has no message".to_owned())?;
    if let Some(calls) = message.tool_calls
        && let Some(entry) = calls.into_iter().next()
    {
        let id = entry.id.unwrap_or_else(|| "call-1".to_owned());
        let function = entry
            .function
            .ok_or_else(|| "provider tool call has no function".to_owned())?;
        let name = function
            .name
            .ok_or_else(|| "provider tool call has no name".to_owned())?;
        if name.trim().is_empty() {
            return Err("provider tool call has no name".to_owned());
        }
        let raw_args = function.arguments.unwrap_or_else(|| "{}".to_owned());
        if raw_args.len() > MAX_TOOL_ARGS_BYTES {
            return Err("provider tool arguments are too large".to_owned());
        }
        let value: serde_json::Value = if raw_args.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&raw_args)
                .map_err(|error| format!("invalid tool arguments: {error}"))?
        };
        if !value.is_object() {
            return Err("tool arguments must be a JSON object".to_owned());
        }
        let arguments = serde_json::from_value(value)
            .map_err(|error| format!("invalid tool arguments: {error}"))?;
        return Ok(ModelResponse::ToolCall(ToolCall::new(id, name, arguments)));
    }
    let content = match message.content {
        Some(serde_json::Value::String(text)) => text,
        Some(serde_json::Value::Array(parts)) => parts
            .into_iter()
            .filter_map(|part| match part {
                serde_json::Value::String(text) => Some(text),
                serde_json::Value::Object(map) => map
                    .get("text")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
                    .or_else(|| {
                        map.get("content")
                            .and_then(|value| value.as_str())
                            .map(str::to_owned)
                    }),
                _ => None,
            })
            .collect::<String>(),
        Some(serde_json::Value::Null) | None => String::new(),
        Some(other) => other
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| other.to_string()),
    };
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err("provider returned an empty response".to_owned());
    }
    Ok(ModelResponse::Text(trimmed.to_owned()))
}

fn read_response_body(
    mut response: reqwest::blocking::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err("provider response is too large".to_owned());
    }
    let mut body = Vec::new();
    response
        .by_ref()
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|error| sanitize_error(&format!("failed to read provider response: {error}")))?;
    if body.len() > max_bytes {
        return Err("provider response is too large".to_owned());
    }
    Ok(body)
}

fn sanitize_provider_error(status: u16, body: &str) -> String {
    let sanitized = sanitize_error(body);
    let preview = sanitized.chars().take(300).collect::<String>();
    if preview.trim().is_empty() {
        format!("provider error: HTTP {status}")
    } else {
        format!("provider error: HTTP {status}: {preview}")
    }
}

fn sanitize_error(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("sk-or-")
        || lower.contains("bearer ")
        || lower.contains("api_key")
        || lower.contains("api-key")
    {
        return "request failed".to_owned();
    }
    let bounded = if message.len() > 500 {
        message.chars().take(500).collect::<String>()
    } else {
        message.to_owned()
    };
    let lower2 = bounded.to_ascii_lowercase();
    if lower2.contains("/home")
        || lower2.contains("/tmp")
        || lower2.contains("credential")
        || lower2.contains("stack trace")
        || lower2.contains("backtrace")
    {
        return "request failed".to_owned();
    }
    bounded
}

fn redact_secret(message: &str, secret: &str) -> String {
    if secret.is_empty() {
        message.to_owned()
    } else {
        message.replace(secret, "[redacted]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ToolSpec;

    #[test]
    fn builds_request_body_with_messages_and_tools() {
        let config = OpenRouterConfig::with_api_key_for_test("sk-or-test", "openai/gpt-4o-mini");
        let model = OpenRouterModel::from_config(config).unwrap();
        let request = ModelRequest {
            system_prompt: "sys".to_owned(),
            prompt: "hello".to_owned(),
            tool_results: vec![crate::agent::ToolResult {
                id: "call-1".to_owned(),
                name: "guest_read_file".to_owned(),
                content: "ok".to_owned(),
                is_error: false,
            }],
            tools: vec![ToolSpec {
                name: "guest_read_file".to_owned(),
                description: "read".to_owned(),
                input_schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
                    .to_owned(),
            }],
            conversation: vec![crate::agent::ConversationMessage {
                role: "user".to_owned(),
                content: "prev".to_owned(),
            }],
            route: crate::agent::RouteRequest::default(),
        };
        let body = model.build_request_body(&request);
        assert_eq!(body["model"], "openai/gpt-4o-mini");
        let messages = body["messages"].as_array().unwrap();
        assert!(messages.iter().any(|value| value["role"] == "system"));
        assert!(
            messages
                .iter()
                .any(|value| value["role"] == "user" && value["content"] == "hello")
        );
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools[0]["function"]["name"], "guest_read_file");
        assert!(messages.iter().all(|value| {
            matches!(
                value["role"].as_str(),
                Some("system" | "user" | "assistant")
            )
        }));
        assert!(messages.iter().any(|value| {
            value["content"]
                .as_str()
                .is_some_and(|content| content.contains("Tool results from the previous step"))
        }));
    }

    #[test]
    fn falls_back_to_object_schema_on_malformed_input_schema() {
        let config = OpenRouterConfig::with_api_key_for_test("sk-or-test", "openai/gpt-4o-mini");
        let model = OpenRouterModel::from_config(config).unwrap();
        let request = ModelRequest {
            system_prompt: String::new(),
            prompt: "hi".to_owned(),
            tool_results: vec![],
            tools: vec![ToolSpec {
                name: "bad".to_owned(),
                description: "bad".to_owned(),
                input_schema: "not-json".to_owned(),
            }],
            conversation: vec![],
            route: crate::agent::RouteRequest::default(),
        };
        let body = model.build_request_body(&request);
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn parses_assistant_text_response() {
        let json = r#"{"choices":[{"message":{"content":"hello world"}}]}"#;
        assert!(
            matches!(parse_chat_response(json), Ok(ModelResponse::Text(text)) if text == "hello world")
        );
    }

    #[test]
    fn parses_tool_call_response() {
        let json = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"call-42","type":"function","function":{"name":"guest_read_file","arguments":"{\"path\":\"/workspace/a.txt\"}"}}]}}]}"#;
        let response = parse_chat_response(json).unwrap();
        match response {
            ModelResponse::ToolCall(call) => {
                assert_eq!(call.id, "call-42");
                assert_eq!(call.name, "guest_read_file");
            }
            _ => panic!("expected tool call"),
        }
    }

    #[test]
    fn rejects_malformed_provider_response() {
        let json = r#"{"choices": "bad"}"#;
        assert!(parse_chat_response(json).is_err());
    }

    #[test]
    fn rejects_oversized_tool_arguments() {
        let big = "a".repeat(MAX_TOOL_ARGS_BYTES + 1);
        let json = format!(
            r#"{{"choices":[{{"message":{{"tool_calls":[{{"id":"1","type":"function","function":{{"name":"guest_read_file","arguments":"{{\\\"path\\\":\\\"{}\\\"}}"}}}}]}}}}]}}"#,
            big
        );
        assert!(parse_chat_response(&json).is_err());
    }

    #[test]
    fn sanitizes_provider_error_without_leaking_key() {
        let error = sanitize_provider_error(401, "invalid bearer sk-or-v1-secret");
        assert!(!error.to_ascii_lowercase().contains("sk-or-"));
        assert!(error.contains("request failed"));
        let bounded = sanitize_provider_error(500, &"a".repeat(1000));
        assert!(bounded.len() <= 500);
    }

    #[test]
    fn redacts_the_configured_secret_from_provider_text() {
        assert_eq!(
            redact_secret("provider echoed sk-or-test", "sk-or-test"),
            "provider echoed [redacted]"
        );
    }

    #[test]
    fn timeout_is_bounded() {
        let config = OpenRouterConfig::with_api_key_for_test("sk-or-test", "openai/gpt-4o-mini");
        assert!(config.timeout.as_secs() >= 1 && config.timeout.as_secs() <= 120);
    }

    #[test]
    fn missing_key_returns_error() {
        let config = OpenRouterConfig {
            api_key: String::new(),
            model: "openai/gpt-4o-mini".to_owned(),
            site_url: None,
            site_title: None,
            timeout: Duration::from_secs(30),
            max_response_bytes: MAX_RESPONSE_BYTES,
            max_tokens: DEFAULT_MAX_TOKENS,
        };
        assert!(OpenRouterModel::from_config(config).is_err());
    }

    #[test]
    fn oversized_response_is_rejected() {
        let big = format!(
            r#"{{"choices":[{{"message":{{"content":"{}"}}}}]}}"#,
            "a".repeat(MAX_RESPONSE_BYTES + 1)
        );
        assert!(parse_chat_response(&big).is_err());
    }

    #[test]
    fn parses_error_envelope() {
        let json = r#"{"error":{"message":"rate limited"}}"#;
        let error = parse_chat_response(json).unwrap_err();
        assert!(error.contains("rate limited") || error.contains("provider error"));
    }
}
