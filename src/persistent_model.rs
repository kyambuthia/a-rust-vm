//! Warm local model service used by the browser guest agent.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Deserialize;

use crate::agent::{Model, ModelError, ModelRequest, ModelResponse, ToolArguments, ToolCall};

const FIXED_PROVIDER: &str = "openrouter";
const FIXED_MODEL: &str = "deepseek/deepseek-v4-flash";
// Local model runners can take several seconds to initialize their plugin and
// credential state. Keep the browser host's startup deterministic while giving
// them a bounded readiness window.
const STARTUP_RETRIES: usize = 150;

#[derive(Clone)]
pub struct PersistentModel {
    state: Arc<Mutex<ModelState>>,
}

pub struct PersistentModelService {
    pub model: PersistentModel,
    _process: Child,
}

struct ModelState {
    address: String,
    session_id: String,
    directory: String,
}

#[derive(Deserialize)]
struct SessionResponse {
    id: String,
}

#[derive(Deserialize)]
struct ModelResponseBody {
    parts: Vec<ModelPart>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ModelPart {
    Text {
        text: String,
    },
    #[serde(other)]
    Ignored,
}

impl PersistentModelService {
    pub fn start(directory: &Path) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("failed to reserve model service port: {error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("failed to inspect model service port: {error}"))?
            .port();
        drop(listener);

        let mut process = Command::new("opencode")
            .args([
                "serve",
                "--pure",
                "--hostname",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .current_dir(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("failed to start model service: {error}"))?;
        let address = format!("127.0.0.1:{port}");

        for _ in 0..STARTUP_RETRIES {
            if request_json_with_timeout(
                "GET",
                &address,
                "/global/health",
                None,
                Duration::from_millis(250),
            )
            .is_ok()
            {
                let session = request_json(
                    "POST",
                    &address,
                    &format!(
                        "/session?directory={}",
                        url_encode(&directory.display().to_string())
                    ),
                    Some(
                        r#"{"title":"A/RVM guest model","permission":[{"permission":"*","pattern":"*","action":"deny"}]}"#,
                    ),
                )?;
                let session: SessionResponse = serde_json::from_str(&session)
                    .map_err(|error| format!("invalid model service session response: {error}"))?;
                return Ok(Self {
                    model: PersistentModel {
                        state: Arc::new(Mutex::new(ModelState {
                            address,
                            session_id: session.id,
                            directory: directory.display().to_string(),
                        })),
                    },
                    _process: process,
                });
            }
            thread::sleep(Duration::from_millis(100));
        }

        let _ = process.kill();
        let _ = process.wait();
        Err("model service did not become ready".to_owned())
    }
}

impl Model for PersistentModel {
    fn respond(&mut self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        let state = self
            .state
            .lock()
            .map_err(|_| ModelError::new("model service state is poisoned"))?;
        let instruction = bridge_instruction(request)
            .map_err(|error| ModelError::new(format!("failed to encode model request: {error}")))?;
        let body = serde_json::json!({
            "model": { "providerID": FIXED_PROVIDER, "modelID": FIXED_MODEL },
            "parts": [{ "type": "text", "text": instruction }],
        });
        let response = request_json(
            "POST",
            &state.address,
            &format!(
                "/session/{}/message?directory={}",
                state.session_id,
                url_encode(&state.directory)
            ),
            Some(&body.to_string()),
        )
        .map_err(ModelError::new)?;
        parse_response(&response).map_err(ModelError::new)
    }
}

fn bridge_instruction(request: &ModelRequest) -> Result<String, serde_json::Error> {
    Ok(format!(
        "You are the language model inside A/RVM. Do not execute tools yourself. Decide whether to answer the user or request exactly one tool from the supplied list. Reply with exactly one JSON object and no markdown. Use one of these shapes: {{\"kind\":\"text\",\"text\":\"...\"}} or {{\"kind\":\"tool_call\",\"id\":\"call-1\",\"name\":\"tool_name\",\"arguments\":{{}}}}. Return valid JSON only.\n\nRequest JSON:\n{}",
        serde_json::to_string(request)?
    ))
}

fn parse_response(body: &str) -> Result<ModelResponse, String> {
    let response: ModelResponseBody = serde_json::from_str(body)
        .or_else(|_| {
            decode_chunked_text(body)
                .ok_or_else(|| serde_json::Error::io(std::io::Error::other("not chunked")))
                .and_then(|decoded| serde_json::from_str(&decoded))
        })
        .map_err(|error| format!("invalid model service response: {error}"))?;
    let text = response
        .parts
        .into_iter()
        .filter_map(|part| match part {
            ModelPart::Text { text } => Some(text),
            ModelPart::Ignored => None,
        })
        .collect::<String>();
    parse_model_text(&text)
}

fn decode_chunked_text(body: &str) -> Option<String> {
    let bytes = body.as_bytes();
    let mut position = 0;
    let mut decoded = Vec::new();
    loop {
        let ending = bytes[position..]
            .windows(2)
            .position(|window| window == b"\r\n")?;
        let line_end = position + ending;
        let size = std::str::from_utf8(&bytes[position..line_end])
            .ok()?
            .split(';')
            .next()?
            .trim();
        let size = usize::from_str_radix(size, 16).ok()?;
        position = line_end + 2;
        if size == 0 {
            return String::from_utf8(decoded).ok();
        }
        let chunk_end = position.checked_add(size)?;
        if bytes.get(position..chunk_end).is_none()
            || bytes.get(chunk_end..chunk_end + 2) != Some(b"\r\n")
        {
            return None;
        }
        decoded.extend_from_slice(&bytes[position..chunk_end]);
        position = chunk_end + 2;
    }
}

fn parse_model_text(text: &str) -> Result<ModelResponse, String> {
    let trimmed = text.trim();
    let fenced = trimmed
        .strip_prefix("```")
        .and_then(|value| value.strip_suffix("```"))
        .map(|value| value.strip_prefix("json").unwrap_or(value).trim())
        .unwrap_or(trimmed);
    let candidate = extract_json_object(fenced).unwrap_or(fenced);
    let value = serde_json::from_str::<serde_json::Value>(candidate).ok();

    if let Some(value) = value {
        if value.get("kind").and_then(serde_json::Value::as_str) == Some("text")
            && let Some(text) = value.get("text").and_then(serde_json::Value::as_str)
        {
            return Ok(ModelResponse::Text(text.to_owned()));
        }
        if value.get("kind").and_then(serde_json::Value::as_str) == Some("tool_call") {
            let id = value.get("id").and_then(serde_json::Value::as_str);
            let name = value.get("name").and_then(serde_json::Value::as_str);
            if let (Some(id), Some(name)) = (id, name) {
                let arguments = value
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let arguments: ToolArguments = serde_json::from_value(arguments)
                    .map_err(|error| format!("invalid tool arguments: {error}"))?;
                return Ok(ModelResponse::ToolCall(ToolCall::new(id, name, arguments)));
            }
        }
    }
    if trimmed.is_empty() {
        Err("model response was empty".to_owned())
    } else {
        Ok(ModelResponse::Text(trimmed.to_owned()))
    }
}

fn request_json(
    method: &str,
    address: &str,
    path: &str,
    body: Option<&str>,
) -> Result<String, String> {
    request_json_with_timeout(method, address, path, body, Duration::from_secs(90))
}

fn request_json_with_timeout(
    method: &str,
    address: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    let mut stream = TcpStream::connect(address)
        .map_err(|error| format!("failed to connect to model service: {error}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| format!("failed to set model service timeout: {error}"))?;
    let body = body.unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("failed to send model service request: {error}"))?;
    stream
        .flush()
        .map_err(|error| format!("failed to flush model service request: {error}"))?;

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|error| format!("failed to read model service status: {error}"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "model service returned no HTTP status".to_owned())?
        .parse::<u16>()
        .map_err(|_| "model service returned an invalid HTTP status".to_owned())?;
    let mut content_length = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|error| format!("failed to read model service headers: {error}"))?;
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse::<usize>().ok();
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        }
    }
    let mut response = String::new();
    if chunked {
        response = read_chunked_body(&mut reader)?;
    } else if let Some(length) = content_length {
        let mut bytes = vec![0; length];
        reader
            .read_exact(&mut bytes)
            .map_err(|error| format!("failed to read model service body: {error}"))?;
        response =
            String::from_utf8(bytes).map_err(|_| "model service body was not UTF-8".to_owned())?;
    } else {
        reader
            .read_to_string(&mut response)
            .map_err(|error| format!("failed to read model service body: {error}"))?;
    }
    if !(200..300).contains(&status) {
        return Err(format!("model service returned HTTP {status}: {response}"));
    }
    Ok(response)
}

fn read_chunked_body(reader: &mut BufReader<TcpStream>) -> Result<String, String> {
    let mut response = Vec::new();
    loop {
        let mut size_line = String::new();
        reader
            .read_line(&mut size_line)
            .map_err(|error| format!("failed to read model service chunk size: {error}"))?;
        let size = size_line
            .trim()
            .split(';')
            .next()
            .ok_or_else(|| "model service returned an empty chunk size".to_owned())
            .and_then(|value| {
                usize::from_str_radix(value, 16)
                    .map_err(|_| "model service returned an invalid chunk size".to_owned())
            })?;
        if size == 0 {
            loop {
                let mut trailer = String::new();
                reader
                    .read_line(&mut trailer)
                    .map_err(|error| format!("failed to read model service trailer: {error}"))?;
                if trailer == "\r\n" || trailer == "\n" || trailer.is_empty() {
                    break;
                }
            }
            break;
        }
        let start = response.len();
        response.resize(start + size, 0);
        reader
            .read_exact(&mut response[start..])
            .map_err(|error| format!("failed to read model service chunk: {error}"))?;
        let mut ending = [0; 2];
        reader
            .read_exact(&mut ending)
            .map_err(|error| format!("failed to read model service chunk ending: {error}"))?;
        if ending != *b"\r\n" {
            return Err("model service returned an invalid chunk ending".to_owned());
        }
    }
    String::from_utf8(response).map_err(|_| "model service body was not UTF-8".to_owned())
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (start < end).then(|| &text[start..=end])
}

fn url_encode(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{decode_chunked_text, parse_model_text, url_encode};
    use crate::agent::{ModelResponse, ToolValue};

    #[test]
    fn parses_text_model_response() {
        assert!(matches!(
            parse_model_text(r#"{"kind":"text","text":"hello"}"#),
            Ok(ModelResponse::Text(text)) if text == "hello"
        ));
    }

    #[test]
    fn parses_tool_model_response() {
        assert!(matches!(
            parse_model_text(r#"{"kind":"tool_call","id":"call-1","name":"guest_read_file","arguments":{"path":"/workspace/a.txt"}}"#),
            Ok(ModelResponse::ToolCall(call))
                if call.name == "guest_read_file"
                && call.arguments.get("path") == Some(&ToolValue::Text("/workspace/a.txt".to_owned()))
        ));
    }

    #[test]
    fn encodes_query_values() {
        assert_eq!(url_encode("/tmp/a rust"), "/tmp/a%20rust");
    }

    #[test]
    fn decodes_chunked_model_responses() {
        assert_eq!(
            decode_chunked_text("5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"),
            Some("hello world".to_owned())
        );
    }
}
