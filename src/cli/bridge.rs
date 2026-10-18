//! `arvm model-bridge`: adapts a local model runner to the process model protocol.

use std::env;
use std::io::{self, Read};
use std::process::{Command, Stdio};

use serde::Deserialize;

use a_rust_vm::agent::ToolArguments;

const MODEL_ATTEMPTS: usize = 2;
const DEFAULT_MODEL: &str = "openrouter/deepseek/deepseek-v4-flash";

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BridgeResponse {
    Text {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: ToolArguments,
    },
}

#[derive(Debug, Deserialize)]
struct RunnerEvent {
    #[serde(rename = "type")]
    event_type: String,
    text: Option<String>,
    part: Option<RunnerPart>,
}

#[derive(Debug, Deserialize)]
struct RunnerPart {
    text: Option<String>,
}

pub(crate) fn run_model_bridge() {
    let mut request_json = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut request_json) {
        eprintln!("failed to read model request: {error}");
        std::process::exit(1);
    }
    let request = match serde_json::from_str::<a_rust_vm::agent::ModelRequest>(&request_json) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("invalid model request: {error}");
            std::process::exit(1);
        }
    };

    let instruction = format!(
        "You are the language model inside A/RVM. Do not execute tools yourself. Decide whether to answer the user or request exactly one tool from the supplied list. Reply with exactly one JSON object and no markdown. Use one of these shapes: {{\"kind\":\"text\",\"text\":\"...\"}} or {{\"kind\":\"tool_call\",\"id\":\"call-1\",\"name\":\"tool_name\",\"arguments\":{{}}}}. Return valid JSON only.\n\nRequest JSON:\n{}",
        serde_json::to_string(&request).expect("model request should serialize")
    );

    let binary = env::var_os("A_RVM_OPENCODE_BIN").unwrap_or_else(|| "opencode".into());
    let working_directory = env::current_dir().unwrap_or_else(|error| {
        eprintln!("failed to resolve model runner directory: {error}");
        std::process::exit(1);
    });
    let response = (0..MODEL_ATTEMPTS)
        .find_map(|attempt| {
            let mut command = Command::new(&binary);
            command
                .args(["run", "--pure", "--format", "json", "--dir"])
                .arg(&working_directory)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            command.args(["--model", DEFAULT_MODEL]);
            command.arg(&instruction);

            let output = match command.output() {
                Ok(output) => output,
                Err(error) => {
                    eprintln!("failed to start local model runner: {error}");
                    std::process::exit(1);
                }
            };
            if !output.status.success() {
                let error = String::from_utf8_lossy(&output.stderr);
                eprintln!("local model runner failed: {}", error.trim());
                std::process::exit(1);
            }

            match parse_bridge_response(&collect_runner_text(&output.stdout)) {
                Ok(response) => Some(response),
                Err(error)
                    if error == "model response was empty" && attempt + 1 < MODEL_ATTEMPTS =>
                {
                    eprintln!("local model returned no content; retrying");
                    None
                }
                Err(error) => {
                    eprintln!("local model returned an invalid A/RVM response: {error}");
                    std::process::exit(1);
                }
            }
        })
        .unwrap_or_else(|| {
            eprintln!("local model returned no content after {MODEL_ATTEMPTS} attempts");
            std::process::exit(1);
        });
    match response {
        BridgeResponse::Text { text } => {
            emit_bridge_event(serde_json::json!({"type": "text_delta", "text": text}));
        }
        BridgeResponse::ToolCall {
            id,
            name,
            arguments,
        } => {
            emit_bridge_event(serde_json::json!({
                "type": "tool_call",
                "id": id,
                "name": name,
                "arguments": arguments,
            }));
        }
    }
    emit_bridge_event(serde_json::json!({"type": "done"}));
}

fn collect_runner_text(output: &[u8]) -> String {
    let mut text = String::new();
    for line in String::from_utf8_lossy(output).lines() {
        let Ok(event) = serde_json::from_str::<RunnerEvent>(line) else {
            continue;
        };
        if matches!(event.event_type.as_str(), "text" | "text_delta")
            && let Some(chunk) = event.text.or_else(|| event.part.and_then(|part| part.text))
        {
            text.push_str(&chunk);
        }
    }
    text
}

fn parse_bridge_response(text: &str) -> Result<BridgeResponse, String> {
    let trimmed = text.trim();
    let fenced = trimmed
        .strip_prefix("```")
        .and_then(|value| value.strip_suffix("```"))
        .map(|value| value.strip_prefix("json").unwrap_or(value).trim())
        .unwrap_or(trimmed);
    let json_candidates = [fenced, extract_json_object(fenced).unwrap_or(fenced)];

    for json in json_candidates {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            continue;
        };
        if let Ok(response) = serde_json::from_value::<BridgeResponse>(value.clone()) {
            return Ok(response);
        }
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("text") | Some("text_delta") => {
                if let Some(text) = value.get("text").and_then(serde_json::Value::as_str) {
                    return Ok(BridgeResponse::Text {
                        text: text.to_owned(),
                    });
                }
            }
            Some("tool_call") => {
                let id = value.get("id").and_then(serde_json::Value::as_str);
                let name = value.get("name").and_then(serde_json::Value::as_str);
                if let (Some(id), Some(name)) = (id, name) {
                    let arguments = value
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));
                    if let Ok(arguments) = serde_json::from_value(arguments) {
                        return Ok(BridgeResponse::ToolCall {
                            id: id.to_owned(),
                            name: name.to_owned(),
                            arguments,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    if !trimmed.is_empty() {
        return Ok(BridgeResponse::Text {
            text: trimmed.to_owned(),
        });
    }

    Err("model response was empty".to_owned())
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (start < end).then(|| &text[start..=end])
}

fn emit_bridge_event(event: serde_json::Value) {
    println!("{event}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bridge_text_response() {
        assert!(matches!(
            parse_bridge_response(r#"{"kind":"text","text":"hello"}"#),
            Ok(BridgeResponse::Text { text }) if text == "hello"
        ));
    }

    #[test]
    fn parses_runner_text_event_response() {
        assert!(matches!(
            parse_bridge_response(r#"{"type":"text_delta","text":"hello"}"#),
            Ok(BridgeResponse::Text { text }) if text == "hello"
        ));
    }

    #[test]
    fn collects_text_and_text_delta_runner_events() {
        let output = br#"
            {"type":"text_delta","text":"{\"kind\":\"text\",\"text\":"}
            {"type":"text","part":{"text":"hello"}}
            {"type":"done"}
        "#;

        assert_eq!(
            collect_runner_text(output),
            "{\"kind\":\"text\",\"text\":hello"
        );
    }

    #[test]
    fn accepts_plain_runner_text_when_the_model_skips_the_json_envelope() {
        assert!(matches!(
            parse_bridge_response("4"),
            Ok(BridgeResponse::Text { text }) if text == "4"
        ));
    }
}
