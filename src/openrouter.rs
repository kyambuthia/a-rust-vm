use std::env;
use std::io::{BufRead, Read};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde::Deserialize;

use crate::agent::{
    ConversationMessage, Model, ModelError, ModelRequest, ModelResponse, ModelStreamEvent, ToolCall,
};

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_MODEL: &str = "~openai/gpt-latest";
const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const MAX_RETRIES: u32 = 2;
const MAX_TOOL_ARGS_BYTES: usize = 32 * 1024;
/// Enough output for a model to write a medium-sized file in one call.
const DEFAULT_MAX_TOKENS: u32 = 8192;
const MAX_MAX_TOKENS: u32 = 32_768;
const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_TIMEOUT_SECS: u64 = 600;
/// Safety caps on what one request carries. The agent bounds its context;
/// these only stop a runaway value, so they sit well above normal use.
const MAX_SYSTEM_CHARS: usize = 64 * 1024;
const MAX_MESSAGE_CHARS: usize = 32 * 1024;
const MAX_TOOL_DESCRIPTION_CHARS: usize = 4 * 1024;

#[derive(Debug, Clone)]
pub struct OpenRouterConfig {
    api_key: String,
    pub model: String,
    pub site_url: Option<String>,
    pub site_title: Option<String>,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub max_tokens: u32,
    /// Reasoning effort sent to the provider; `None` leaves the provider's
    /// default. Silent reasoning delays the first visible token, so the
    /// default is `low`.
    pub reasoning: Option<ReasoningEffort>,
    /// How OpenRouter orders the providers serving the model: `price`,
    /// `throughput`, or `latency`. `None` uses OpenRouter's default.
    pub provider_sort: Option<String>,
    /// OpenAI-compatible API base; requests go to `<base>/chat/completions`.
    pub base_url: String,
}

/// Parse `OPENROUTER_PROVIDER_SORT`; `default` sends no preference.
pub fn parse_provider_sort(value: &str) -> Result<Option<String>, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "default" => Ok(None),
        sort @ ("price" | "throughput" | "latency") => Ok(Some(sort.to_owned())),
        other => Err(format!(
            "OPENROUTER_PROVIDER_SORT must be price, throughput, latency, or default, not '{other}'"
        )),
    }
}

/// OpenRouter's unified `reasoning` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningEffort {
    Disabled,
    Minimal,
    Low,
    Medium,
    High,
}

impl ReasoningEffort {
    /// Parse `OPENROUTER_REASONING`: `default` means "send nothing".
    pub fn parse(value: &str) -> Result<Option<Self>, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "default" => Ok(None),
            "none" | "off" => Ok(Some(Self::Disabled)),
            "minimal" => Ok(Some(Self::Minimal)),
            "low" => Ok(Some(Self::Low)),
            "medium" => Ok(Some(Self::Medium)),
            "high" => Ok(Some(Self::High)),
            other => Err(format!(
                "OPENROUTER_REASONING must be none, minimal, low, medium, high, or default, not '{other}'"
            )),
        }
    }

    fn request_value(self) -> serde_json::Value {
        match self {
            Self::Disabled => serde_json::json!({"enabled": false}),
            Self::Minimal => serde_json::json!({"effort": "minimal"}),
            Self::Low => serde_json::json!({"effort": "low"}),
            Self::Medium => serde_json::json!({"effort": "medium"}),
            Self::High => serde_json::json!({"effort": "high"}),
        }
    }
}

/// Model settings from the user configuration file (`"model": {...}`).
/// Environment variables override each field.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelSettings {
    /// Default model (`OPENROUTER_MODEL`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Model for `/fast` (`OPENROUTER_FAST_MODEL`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast: Option<String>,
    /// Model for `/strong` (`OPENROUTER_STRONG_MODEL`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strong: Option<String>,
    /// OpenAI-compatible API base (`OPENROUTER_BASE_URL`), for example a
    /// local server at `http://127.0.0.1:8000/v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_sort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl ModelSettings {
    /// Check every set field, so a bad file fails when it is loaded.
    pub fn validate(&self) -> Result<(), String> {
        OpenRouterConfig::resolve(
            &|name| (name == "OPENROUTER_API_KEY").then(|| "validation".to_owned()),
            self,
        )?;
        let names = [&self.name, &self.fast, &self.strong];
        if names
            .into_iter()
            .flatten()
            .any(|model| model.trim().is_empty())
        {
            return Err("model names must not be empty".to_owned());
        }
        Ok(())
    }
}

/// Accept `https://` bases, and plain `http://` only for this machine, so the
/// API key never crosses the network unencrypted.
pub fn validate_base_url(value: &str) -> Result<String, String> {
    let trimmed = value.trim().trim_end_matches('/');
    let local = ["http://localhost", "http://127.0.0.1", "http://[::1]"]
        .iter()
        .any(|prefix| {
            trimmed == *prefix
                || trimmed
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with(':') || rest.starts_with('/'))
        });
    let secure = trimmed
        .strip_prefix("https://")
        .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'));
    if secure || local {
        Ok(trimmed.to_owned())
    } else {
        Err(format!(
            "model base URL must use https:// (or http:// on localhost), not '{value}'"
        ))
    }
}

impl OpenRouterConfig {
    pub fn from_env() -> Result<Self, String> {
        Self::from_env_and(&ModelSettings::default())
    }

    /// Environment variables first, then `settings`, then defaults.
    pub fn from_env_and(settings: &ModelSettings) -> Result<Self, String> {
        Self::resolve(&|name| env::var(name).ok(), settings)
    }

    /// Resolve every field from `var` (an environment lookup), `settings`,
    /// and defaults. Set but invalid values are errors, not silent defaults.
    pub fn resolve(
        var: &dyn Fn(&str) -> Option<String>,
        settings: &ModelSettings,
    ) -> Result<Self, String> {
        let get = |name: &str| var(name).filter(|value| !value.trim().is_empty());
        let api_key =
            get("OPENROUTER_API_KEY").ok_or_else(|| "OPENROUTER_API_KEY is not set".to_owned())?;
        let model = get("OPENROUTER_MODEL")
            .or_else(|| settings.name.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_owned());
        let site_url =
            get("OPENROUTER_SITE_URL").or_else(|| Some("https://asiliano.online".to_owned()));
        let site_title = get("OPENROUTER_SITE_TITLE").or_else(|| Some("A/RVM".to_owned()));
        let timeout_secs = match get("OPENROUTER_TIMEOUT_SECS") {
            Some(value) => Some(value.trim().parse::<u64>().map_err(|_| {
                format!("OPENROUTER_TIMEOUT_SECS must be a number of seconds, not '{value}'")
            })?),
            None => settings.timeout_secs,
        }
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
        if !(1..=MAX_TIMEOUT_SECS).contains(&timeout_secs) {
            return Err(format!(
                "model timeout must be 1-{MAX_TIMEOUT_SECS} seconds"
            ));
        }
        let max_tokens =
            match get("OPENROUTER_MAX_TOKENS") {
                Some(value) => Some(value.trim().parse::<u32>().map_err(|_| {
                    format!("OPENROUTER_MAX_TOKENS must be a number, not '{value}'")
                })?),
                None => settings.max_tokens,
            }
            .unwrap_or(DEFAULT_MAX_TOKENS);
        if !(128..=MAX_MAX_TOKENS).contains(&max_tokens) {
            return Err(format!("model max tokens must be 128-{MAX_MAX_TOKENS}"));
        }
        let reasoning = match get("OPENROUTER_REASONING").or_else(|| settings.reasoning.clone()) {
            Some(value) => ReasoningEffort::parse(&value)?,
            None => Some(ReasoningEffort::Low),
        };
        // OpenRouter's default favours price; for interactive use, measured
        // throughput routing finished turns about twice as fast.
        let provider_sort =
            match get("OPENROUTER_PROVIDER_SORT").or_else(|| settings.provider_sort.clone()) {
                Some(value) => parse_provider_sort(&value)?,
                None => Some("throughput".to_owned()),
            };
        let base_url = match get("OPENROUTER_BASE_URL").or_else(|| settings.base_url.clone()) {
            Some(value) => validate_base_url(&value)?,
            None => DEFAULT_BASE_URL.to_owned(),
        };
        Ok(Self {
            api_key,
            model,
            site_url,
            site_title,
            timeout: Duration::from_secs(timeout_secs),
            max_response_bytes: MAX_RESPONSE_BYTES,
            max_tokens,
            reasoning,
            provider_sort,
            base_url,
        })
    }

    /// The same settings for another model, for fast and strong routes.
    pub fn for_model(&self, model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..self.clone()
        }
    }

    pub fn with_api_key_for_test(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            site_url: Some("https://asiliano.online".to_owned()),
            site_title: Some("A/RVM".to_owned()),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            max_response_bytes: MAX_RESPONSE_BYTES,
            max_tokens: DEFAULT_MAX_TOKENS,
            reasoning: Some(ReasoningEffort::Low),
            provider_sort: None,
            base_url: DEFAULT_BASE_URL.to_owned(),
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

fn is_transient_status(status: u16) -> bool {
    matches!(status, 408 | 429) || (500..=599).contains(&status)
}

impl OpenRouterModel {
    /// POST `body`, retrying transient failures, and return the first
    /// successful response. Error bodies are read, redacted, and returned.
    fn send(&self, body: &serde_json::Value) -> Result<reqwest::blocking::Response, ModelError> {
        let mut last_error: Option<ModelError> = None;
        for attempt in 0..=MAX_RETRIES {
            let mut http_request = self
                .inner
                .client
                .post(format!("{}/chat/completions", self.inner.config.base_url))
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
            let response = match http_request.json(body).send() {
                Ok(response) => response,
                Err(error) => {
                    let err = ModelError::new(sanitize_error(&format!(
                        "provider request failed: {error}"
                    )));
                    if attempt < MAX_RETRIES {
                        last_error = Some(err);
                        thread::sleep(Duration::from_millis(250 * (attempt as u64 + 1)));
                        continue;
                    }
                    return Err(err);
                }
            };
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            let bytes = read_response_body(response, self.inner.config.max_response_bytes)
                .map_err(ModelError::new)?;
            let text = redact_secret(&String::from_utf8_lossy(&bytes), &self.inner.config.api_key);
            let error = ModelError::new(sanitize_provider_error(status.as_u16(), &text));
            if is_transient_status(status.as_u16()) && attempt < MAX_RETRIES {
                last_error = Some(error);
                thread::sleep(Duration::from_millis(250 * (attempt as u64 + 1)));
                continue;
            }
            return Err(error);
        }
        Err(last_error.unwrap_or_else(|| ModelError::new("provider request failed".to_owned())))
    }
}

impl Model for OpenRouterModel {
    fn respond(&mut self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        let body = self.build_request_body(request);
        let response = self.send(&body)?;
        let bytes = read_response_body(response, self.inner.config.max_response_bytes)
            .map_err(ModelError::new)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| ModelError::new("provider response was not UTF-8".to_owned()))?;
        let text = redact_secret(&text, &self.inner.config.api_key);
        parse_chat_response(&text).map_err(ModelError::new)
    }

    fn respond_stream(
        &mut self,
        request: &ModelRequest,
        emit: &mut dyn FnMut(ModelStreamEvent),
    ) -> Result<ModelResponse, ModelError> {
        let mut body = self.build_request_body(request);
        body["stream"] = serde_json::json!(true);
        body["stream_options"] = serde_json::json!({"include_usage": true});
        let response = self.send(&body)?;
        let api_key = self.inner.config.api_key.clone();
        parse_sse_stream(
            std::io::BufReader::new(response),
            MAX_STREAM_BYTES,
            &mut |event| match event {
                ModelStreamEvent::TextDelta { content } => emit(ModelStreamEvent::TextDelta {
                    content: redact_secret(&content, &api_key),
                }),
                usage @ ModelStreamEvent::Usage(_) => emit(usage),
            },
        )
        .map_err(ModelError::new)
    }
}

/// Upper bound on one streamed response, including SSE framing.
const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_STREAM_LINE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default)]
struct PartialToolCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

/// Read an OpenAI-style server-sent event stream, emitting text deltas as
/// they arrive and assembling tool calls from their fragments.
fn parse_sse_stream(
    mut reader: impl BufRead,
    max_bytes: usize,
    emit: &mut dyn FnMut(ModelStreamEvent),
) -> Result<ModelResponse, String> {
    let mut text = String::new();
    let mut calls: std::collections::BTreeMap<u64, PartialToolCall> = Default::default();
    let mut usage: Option<crate::agent::Usage> = None;
    let mut provider: Option<String> = None;
    let mut total = 0usize;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut reader)
            .take(MAX_STREAM_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|error| sanitize_error(&format!("failed to read provider stream: {error}")))?;
        if read == 0 {
            break;
        }
        if line.len() > MAX_STREAM_LINE_BYTES {
            return Err("provider stream line is too large".to_owned());
        }
        total += read;
        if total > max_bytes {
            return Err("provider response is too large".to_owned());
        }
        let line = std::str::from_utf8(&line)
            .map_err(|_| "provider stream was not UTF-8".to_owned())?
            .trim();
        // Blank lines end events; lines starting with ':' are keep-alive comments.
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            break;
        }
        let chunk: serde_json::Value = serde_json::from_str(data)
            .map_err(|error| sanitize_error(&format!("invalid provider stream chunk: {error}")))?;
        if let Some(error) = chunk.get("error") {
            let message = error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("provider stream error");
            return Err(sanitize_provider_error(400, message));
        }
        if let Some(name) = chunk.get("provider").and_then(serde_json::Value::as_str) {
            provider = Some(name.to_owned());
        }
        if let Some(reported) = chunk.get("usage").filter(|value| value.is_object()) {
            usage = Some(parse_usage(reported));
        }
        let Some(delta) = chunk.pointer("/choices/0/delta") else {
            continue;
        };
        if let Some(content) = delta.get("content").and_then(serde_json::Value::as_str)
            && !content.is_empty()
        {
            text.push_str(content);
            emit(ModelStreamEvent::TextDelta {
                content: content.to_owned(),
            });
        }
        for fragment in delta
            .get("tool_calls")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let index = fragment
                .get("index")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let call = calls.entry(index).or_default();
            if let Some(id) = fragment.get("id").and_then(serde_json::Value::as_str)
                && !id.is_empty()
            {
                call.id = Some(id.to_owned());
            }
            if let Some(name) = fragment
                .pointer("/function/name")
                .and_then(serde_json::Value::as_str)
            {
                call.name.push_str(name);
            }
            if let Some(arguments) = fragment
                .pointer("/function/arguments")
                .and_then(serde_json::Value::as_str)
            {
                if call.arguments.len() + arguments.len() > MAX_TOOL_ARGS_BYTES {
                    return Err("provider tool arguments are too large".to_owned());
                }
                call.arguments.push_str(arguments);
            }
        }
    }
    if let Some(mut usage) = usage {
        usage.provider = provider;
        emit(ModelStreamEvent::Usage(usage));
    }
    if !calls.is_empty() {
        if calls.len() > 1 {
            return Err(
                "provider returned multiple tool calls; one call per turn is required".to_owned(),
            );
        }
        let call = calls.into_values().next().expect("one call is present");
        return tool_call_response(call.id, Some(call.name), Some(call.arguments));
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("provider returned an empty response".to_owned());
    }
    Ok(ModelResponse::Text(trimmed.to_owned()))
}

/// Read OpenAI-style usage, with OpenRouter's cache, reasoning, and cost fields.
fn parse_usage(value: &serde_json::Value) -> crate::agent::Usage {
    let count = |pointer: &str| {
        value
            .pointer(pointer)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let cost_micro_usd = value
        .get("cost")
        .and_then(serde_json::Value::as_f64)
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
        .map(|cost| (cost * 1_000_000.0).round() as u64);
    crate::agent::Usage {
        input_tokens: count("/prompt_tokens"),
        output_tokens: count("/completion_tokens"),
        cached_tokens: count("/prompt_tokens_details/cached_tokens"),
        reasoning_tokens: count("/completion_tokens_details/reasoning_tokens"),
        cost_micro_usd,
        ..Default::default()
    }
}

/// Validate one tool call's parts into a response.
fn tool_call_response(
    id: Option<String>,
    name: Option<String>,
    raw_args: Option<String>,
) -> Result<ModelResponse, String> {
    let id = id.unwrap_or_else(|| "call-1".to_owned());
    let name = name.ok_or_else(|| "provider tool call has no name".to_owned())?;
    if name.trim().is_empty() {
        return Err("provider tool call has no name".to_owned());
    }
    let raw_args = raw_args.unwrap_or_else(|| "{}".to_owned());
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
    Ok(ModelResponse::ToolCall(ToolCall::new(id, name, arguments)))
}

fn build_request_body(config: &OpenRouterConfig, request: &ModelRequest) -> serde_json::Value {
    let mut messages = Vec::new();
    let system = bounded_text(&request.system_prompt, MAX_SYSTEM_CHARS);
    if !system.trim().is_empty() {
        messages.push(serde_json::json!({"role":"system","content":system}));
    }
    for entry in request.conversation.iter().chain(&request.current_turn) {
        messages.push(conversation_message(entry));
    }
    if request.current_turn.is_empty() {
        messages.push(serde_json::json!({
            "role":"user",
            "content": bounded_text(&request.prompt, MAX_MESSAGE_CHARS)
        }));
    }
    if request.current_turn.is_empty() && !request.tool_results.is_empty() {
        let results = request
            .tool_results
            .iter()
            .map(|result| {
                let status = if result.is_error { "error" } else { "ok" };
                format!(
                    "id={} name={} status={status}\n{}",
                    result.id,
                    result.name,
                    bounded_text(&result.content, MAX_MESSAGE_CHARS)
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
                    "description": bounded_text(&tool.description, MAX_TOOL_DESCRIPTION_CHARS),
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
    if uses_explicit_cache_markers(&config.model) {
        mark_cache_breakpoints(&mut body);
    }
    if let Some(reasoning) = config.reasoning {
        body["reasoning"] = reasoning.request_value();
    }
    if let Some(sort) = &config.provider_sort {
        body["provider"] = serde_json::json!({"sort": sort});
    }
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools);
        body["tool_choice"] = serde_json::json!("auto");
        // The agent loop executes exactly one tool call per model turn and
        // rejects responses carrying several, so ask for one at a time.
        body["parallel_tool_calls"] = serde_json::json!(false);
    }
    body
}

/// Anthropic models cache only up to explicit `cache_control` markers; most
/// other providers cache automatically.
fn uses_explicit_cache_markers(model: &str) -> bool {
    model.starts_with("anthropic/") || model.starts_with("~anthropic/")
}

/// Mark the system prompt and the newest message as cache breakpoints, so
/// the next step of the turn reuses everything before its new messages.
fn mark_cache_breakpoints(body: &mut serde_json::Value) {
    let Some(messages) = body["messages"].as_array_mut() else {
        return;
    };
    let last = messages.len().saturating_sub(1);
    for index in [0, last] {
        let Some(message) = messages.get_mut(index) else {
            continue;
        };
        if let Some(text) = message["content"].as_str().map(str::to_owned)
            && !text.is_empty()
        {
            message["content"] = serde_json::json!([{
                "type": "text",
                "text": text,
                "cache_control": {"type": "ephemeral"}
            }]);
        }
    }
}

fn conversation_message(entry: &ConversationMessage) -> serde_json::Value {
    if let Some(call) = &entry.tool_call {
        let arguments = serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_owned());
        return serde_json::json!({
            "role": "assistant",
            "content": bounded_text(&entry.content, MAX_MESSAGE_CHARS),
            "tool_calls": [{
                "id": call.id,
                "type": "function",
                "function": {
                    "name": call.name,
                    "arguments": arguments
                }
            }]
        });
    }
    if let Some(tool_call_id) = &entry.tool_call_id {
        return serde_json::json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": bounded_text(&entry.content, MAX_MESSAGE_CHARS)
        });
    }
    serde_json::json!({
        "role": normalize_role(&entry.role),
        "content": bounded_text(&entry.content, MAX_MESSAGE_CHARS)
    })
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
    if let Some(calls) = message.tool_calls {
        if calls.len() > 1 {
            return Err(
                "provider returned multiple tool calls; one call per turn is required".to_owned(),
            );
        }
        let Some(entry) = calls.into_iter().next() else {
            return Err("provider returned an empty tool call list".to_owned());
        };
        let function = entry
            .function
            .ok_or_else(|| "provider tool call has no function".to_owned())?;
        return tool_call_response(entry.id, function.name, function.arguments);
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
                tool_call: None,
                tool_call_id: None,
            }],
            current_turn: Vec::new(),
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
        assert_eq!(body["parallel_tool_calls"], false);
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
            current_turn: Vec::new(),
            route: crate::agent::RouteRequest::default(),
        };
        let body = model.build_request_body(&request);
        assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn encodes_structured_tool_turns_as_provider_messages() {
        let config = OpenRouterConfig::with_api_key_for_test("sk-or-test", "openai/gpt-4o-mini");
        let model = OpenRouterModel::from_config(config).unwrap();
        let call = ToolCall::new("call-7", "guest_read_file", Default::default());
        let result = crate::agent::ToolResult {
            id: "call-7".to_owned(),
            name: "guest_read_file".to_owned(),
            content: "contents".to_owned(),
            is_error: false,
        };
        let request = ModelRequest {
            system_prompt: String::new(),
            prompt: "read it".to_owned(),
            tool_results: vec![result.clone()],
            tools: Vec::new(),
            conversation: Vec::new(),
            current_turn: vec![
                ConversationMessage::new("user", "read it"),
                ConversationMessage {
                    role: "assistant".to_owned(),
                    content: String::new(),
                    tool_call: Some(call),
                    tool_call_id: None,
                },
                ConversationMessage {
                    role: "tool".to_owned(),
                    content: result.content.clone(),
                    tool_call: None,
                    tool_call_id: Some(result.id),
                },
            ],
            route: crate::agent::RouteRequest::default(),
        };

        let body = model.build_request_body(&request);
        let messages = body["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["tool_calls"][0]["id"], "call-7");
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["name"],
            "guest_read_file"
        );
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call-7");
        assert_eq!(messages[2]["content"], "contents");
        assert_eq!(messages.len(), 3);
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
    fn rejects_multiple_provider_tool_calls_in_one_turn() {
        let json = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"one","arguments":"{}"}},{"id":"call-2","type":"function","function":{"name":"two","arguments":"{}"}}]}}]}"#;

        assert_eq!(
            parse_chat_response(json).unwrap_err(),
            "provider returned multiple tool calls; one call per turn is required"
        );
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
    fn transient_statuses_are_retryable() {
        assert!(is_transient_status(408));
        assert!(is_transient_status(429));
        assert!(is_transient_status(500));
        assert!(is_transient_status(503));
        assert!(!is_transient_status(400));
        assert!(!is_transient_status(401));
        assert!(!is_transient_status(404));
        assert!(!is_transient_status(422));
    }

    #[test]
    fn stream_reports_usage_cost_and_provider() {
        let body = concat!(
            "data: {\"provider\":\"StreamLake\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
            "data: {\"provider\":\"StreamLake\",\"choices\":[],\"usage\":{\"prompt_tokens\":1200,",
            "\"completion_tokens\":56,\"prompt_tokens_details\":{\"cached_tokens\":800},",
            "\"completion_tokens_details\":{\"reasoning_tokens\":30},\"cost\":0.000123}}\n\n",
            "data: [DONE]\n\n"
        );
        let mut usage = None;
        let result = parse_sse_stream(std::io::Cursor::new(body), MAX_STREAM_BYTES, &mut |event| {
            if let ModelStreamEvent::Usage(reported) = event {
                usage = Some(reported);
            }
        });
        assert_eq!(result.unwrap(), ModelResponse::Text("hi".to_owned()));
        let usage = usage.unwrap();
        assert_eq!(usage.input_tokens, 1200);
        assert_eq!(usage.output_tokens, 56);
        assert_eq!(usage.cached_tokens, 800);
        assert_eq!(usage.reasoning_tokens, 30);
        assert_eq!(usage.cost_micro_usd, Some(123));
        assert_eq!(usage.provider.as_deref(), Some("StreamLake"));
    }

    fn stream(chunks: &[&str]) -> (Result<ModelResponse, String>, Vec<String>) {
        let body = chunks
            .iter()
            .map(|chunk| format!("data: {chunk}\n\n"))
            .collect::<String>();
        let mut deltas = Vec::new();
        let result = parse_sse_stream(
            std::io::Cursor::new(format!(": OPENROUTER PROCESSING\n\n{body}data: [DONE]\n\n")),
            MAX_STREAM_BYTES,
            &mut |event| match event {
                ModelStreamEvent::TextDelta { content } => deltas.push(content),
                ModelStreamEvent::Usage(_) => {}
            },
        );
        (result, deltas)
    }

    #[test]
    fn stream_emits_text_deltas_as_they_arrive() {
        let (result, deltas) = stream(&[
            r#"{"choices":[{"delta":{"role":"assistant","content":""}}]}"#,
            r#"{"choices":[{"delta":{"content":"Hello"}}]}"#,
            r#"{"choices":[{"delta":{"content":", world"}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        ]);
        assert_eq!(deltas, ["Hello", ", world"]);
        assert_eq!(
            result.unwrap(),
            ModelResponse::Text("Hello, world".to_owned())
        );
    }

    #[test]
    fn stream_assembles_tool_call_fragments() {
        let (result, deltas) = stream(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_7","type":"function","function":{"name":"read_file","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a.txt\"}"}}]}}]}"#,
        ]);
        assert!(deltas.is_empty());
        let ModelResponse::ToolCall(call) = result.unwrap() else {
            panic!("expected a tool call");
        };
        assert_eq!(call.id, "call_7");
        assert_eq!(call.name, "read_file");
        assert_eq!(
            call.arguments.get("path"),
            Some(&crate::agent::ToolValue::Text("a.txt".to_owned()))
        );
    }

    #[test]
    fn stream_fails_closed_on_errors_and_limits() {
        let (result, _) = stream(&[r#"{"error":{"message":"rate limited"}}"#]);
        assert!(result.unwrap_err().contains("rate limited"));
        let (result, _) = stream(&[r#"{"choices":[{"delta":{}}]}"#]);
        assert_eq!(result.unwrap_err(), "provider returned an empty response");
        let (result, _) = stream(&["not json"]);
        assert!(
            result
                .unwrap_err()
                .contains("invalid provider stream chunk")
        );
        let (result, _) = stream(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"a"}},{"index":1,"function":{"name":"b"}}]}}]}"#,
        ]);
        assert!(result.unwrap_err().contains("multiple tool calls"));
        let big = format!(
            r#"{{"choices":[{{"delta":{{"content":"{}"}}}}]}}"#,
            "a".repeat(64)
        );
        let result = parse_sse_stream(
            std::io::Cursor::new(format!("data: {big}\n\n")),
            32,
            &mut |_| {},
        );
        assert_eq!(result.unwrap_err(), "provider response is too large");
    }

    #[test]
    fn reasoning_setting_parses_and_reaches_the_body() {
        assert_eq!(ReasoningEffort::parse("default").unwrap(), None);
        assert_eq!(
            ReasoningEffort::parse(" None ").unwrap(),
            Some(ReasoningEffort::Disabled)
        );
        assert!(ReasoningEffort::parse("max").is_err());
        let mut config = OpenRouterConfig::with_api_key_for_test("sk-or-test", "m");
        let request = ModelRequest {
            system_prompt: String::new(),
            prompt: "hi".to_owned(),
            tool_results: Vec::new(),
            tools: Vec::new(),
            conversation: Vec::new(),
            current_turn: Vec::new(),
            route: Default::default(),
        };
        assert_eq!(
            build_request_body(&config, &request)["reasoning"],
            serde_json::json!({"effort": "low"})
        );
        config.reasoning = Some(ReasoningEffort::Disabled);
        assert_eq!(
            build_request_body(&config, &request)["reasoning"],
            serde_json::json!({"enabled": false})
        );
        config.reasoning = None;
        assert!(
            build_request_body(&config, &request)
                .get("reasoning")
                .is_none()
        );
        assert!(
            build_request_body(&config, &request)
                .get("provider")
                .is_none()
        );
        config.provider_sort = parse_provider_sort("Throughput").unwrap();
        assert_eq!(
            build_request_body(&config, &request)["provider"],
            serde_json::json!({"sort": "throughput"})
        );
        assert_eq!(parse_provider_sort("default").unwrap(), None);
        assert!(parse_provider_sort("fastest").is_err());
    }

    #[test]
    fn anthropic_requests_mark_cache_breakpoints() {
        let request = ModelRequest {
            system_prompt: "system rules".to_owned(),
            prompt: "hello".to_owned(),
            tool_results: Vec::new(),
            tools: Vec::new(),
            conversation: vec![ConversationMessage::new("user", "earlier")],
            current_turn: Vec::new(),
            route: Default::default(),
        };
        let config = OpenRouterConfig::with_api_key_for_test("sk-or-test", "anthropic/claude-x");
        let body = build_request_body(&config, &request);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["content"][0]["text"], "system rules");
        assert_eq!(
            messages[0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(messages[1]["content"], "earlier");
        assert_eq!(
            messages[2]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );

        let config = OpenRouterConfig::with_api_key_for_test("sk-or-test", "deepseek/x");
        let body = build_request_body(&config, &request);
        assert_eq!(body["messages"][0]["content"], "system rules");
    }

    fn environment(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect::<Vec<_>>();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn environment_overrides_settings_which_override_defaults() {
        let settings = ModelSettings {
            name: Some("settings/model".to_owned()),
            base_url: Some("http://127.0.0.1:8000/v1/".to_owned()),
            max_tokens: Some(1024),
            reasoning: Some("none".to_owned()),
            ..Default::default()
        };
        let key = environment(&[("OPENROUTER_API_KEY", "sk-or-test")]);
        let config = OpenRouterConfig::resolve(&key, &settings).unwrap();
        assert_eq!(config.model, "settings/model");
        assert_eq!(config.base_url, "http://127.0.0.1:8000/v1");
        assert_eq!(config.max_tokens, 1024);
        assert_eq!(config.reasoning, Some(ReasoningEffort::Disabled));
        assert_eq!(config.provider_sort.as_deref(), Some("throughput"));

        let overridden = environment(&[
            ("OPENROUTER_API_KEY", "sk-or-test"),
            ("OPENROUTER_MODEL", "env/model"),
            ("OPENROUTER_MAX_TOKENS", "2048"),
        ]);
        let config = OpenRouterConfig::resolve(&overridden, &settings).unwrap();
        assert_eq!(config.model, "env/model");
        assert_eq!(config.max_tokens, 2048);
        assert_eq!(config.for_model("fast/model").model, "fast/model");

        let defaults = OpenRouterConfig::resolve(&key, &ModelSettings::default()).unwrap();
        assert_eq!(defaults.base_url, DEFAULT_BASE_URL);
        assert_eq!(defaults.timeout, Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    }

    #[test]
    fn invalid_model_settings_are_errors() {
        let key = environment(&[("OPENROUTER_API_KEY", "sk-or-test")]);
        for (settings, expected) in [
            (
                ModelSettings {
                    base_url: Some("http://example.com/v1".to_owned()),
                    ..Default::default()
                },
                "https://",
            ),
            (
                ModelSettings {
                    timeout_secs: Some(0),
                    ..Default::default()
                },
                "timeout",
            ),
            (
                ModelSettings {
                    max_tokens: Some(64),
                    ..Default::default()
                },
                "max tokens",
            ),
            (
                ModelSettings {
                    reasoning: Some("max".to_owned()),
                    ..Default::default()
                },
                "OPENROUTER_REASONING",
            ),
        ] {
            let error = OpenRouterConfig::resolve(&key, &settings).unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(settings.validate().is_err());
        }
        let bad_env = environment(&[
            ("OPENROUTER_API_KEY", "sk-or-test"),
            ("OPENROUTER_TIMEOUT_SECS", "soon"),
        ]);
        assert!(OpenRouterConfig::resolve(&bad_env, &ModelSettings::default()).is_err());
        assert!(validate_base_url("https://openrouter.ai/api/v1").is_ok());
        assert!(validate_base_url("http://localhost:11434/v1").is_ok());
        assert!(validate_base_url("http://localhost.evil.com/v1").is_err());
        assert!(validate_base_url("https:///path").is_err());
        assert!(
            ModelSettings {
                fast: Some(" ".to_owned()),
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn timeout_is_bounded() {
        let config = OpenRouterConfig::with_api_key_for_test("sk-or-test", "openai/gpt-4o-mini");
        assert!(config.timeout.as_secs() >= 1 && config.timeout.as_secs() <= MAX_TIMEOUT_SECS);
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
            reasoning: None,
            provider_sort: None,
            base_url: DEFAULT_BASE_URL.to_owned(),
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
