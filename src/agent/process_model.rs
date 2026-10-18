//! A model adapter for a child process that speaks newline-delimited JSON.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::{
    Model, ModelError, ModelRequest, ModelResponse, ModelStreamEvent, ToolArguments, ToolCall,
};

/// A model adapter for a process that speaks A/RVM's newline-delimited JSON
/// protocol over stdin and stdout.
///
/// The executable is launched directly, without a shell. The child receives a
/// serialized [`ModelRequest`] and can emit text, tool calls, errors, and a
/// final `done` event one line at a time.
pub struct ProcessModel {
    program: PathBuf,
    arguments: Vec<String>,
    working_directory: Option<PathBuf>,
    timeout: Duration,
}

const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_PROCESS_STDERR_BYTES: usize = 16 * 1024;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

impl ProcessModel {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            working_directory: None,
            timeout: DEFAULT_PROCESS_TIMEOUT,
        }
    }

    pub fn with_arguments<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.arguments = arguments.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_working_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.working_directory = Some(directory.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout.max(Duration::from_millis(1));
        self
    }

    fn run_stream(
        &self,
        request: &ModelRequest,
        emit: &mut dyn FnMut(ModelStreamEvent),
    ) -> Result<ModelResponse, ModelError> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(directory) = &self.working_directory {
            command.current_dir(directory);
        }

        let child = command.spawn().map_err(|error| {
            ModelError::new(format!(
                "failed to start model process '{}': {error}",
                self.program.display()
            ))
        })?;
        let mut child = ChildGuard::new(child);

        let request_json = serde_json::to_vec(request)
            .map_err(|error| ModelError::new(format!("failed to encode model request: {error}")))?;
        let mut stdin = child
            .child
            .stdin
            .take()
            .ok_or_else(|| ModelError::new("model process stdin is unavailable"))?;
        let stdout = child
            .child
            .stdout
            .take()
            .ok_or_else(|| ModelError::new("model process stdout is unavailable"))?;
        let stderr = child
            .child
            .stderr
            .take()
            .ok_or_else(|| ModelError::new("model process stderr is unavailable"))?;

        let (input_tx, input_rx) = mpsc::channel();
        thread::spawn(move || {
            let result = stdin
                .write_all(&request_json)
                .and_then(|_| stdin.write_all(b"\n"))
                .map_err(|error| format!("failed to send model request: {error}"));
            let _ = input_tx.send(result);
        });

        let (line_tx, line_rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let message = line
                    .map(Some)
                    .map_err(|error| format!("failed to read model output: {error}"));
                let should_stop = message.is_err() || matches!(message, Ok(None));
                if line_tx.send(message).is_err() || should_stop {
                    break;
                }
            }
            let _ = line_tx.send(Ok(None));
        });

        let (stderr_tx, stderr_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut pipe = stderr;
            let mut captured = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(bytes) => {
                        if captured.len() < MAX_PROCESS_STDERR_BYTES {
                            let remaining = MAX_PROCESS_STDERR_BYTES - captured.len();
                            captured.extend_from_slice(&buffer[..bytes.min(remaining)]);
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = stderr_tx.send(String::from_utf8_lossy(&captured).into_owned());
        });

        let deadline = Instant::now() + self.timeout;
        let mut response = None;
        let mut text = String::new();

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ModelError::new("model process timed out"));
            }
            let line = match line_rx.recv_timeout(remaining) {
                Ok(Ok(Some(line))) => line,
                Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(Err(error)) => return Err(ModelError::new(error)),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(ModelError::new("model process timed out"));
                }
            };
            if line.trim().is_empty() {
                continue;
            }

            let event = serde_json::from_str::<ProcessModelEvent>(&line).map_err(|error| {
                ModelError::new(format!("invalid model event: {error}; line={line}"))
            })?;
            match event {
                ProcessModelEvent::Text { text: chunk, part } => {
                    let chunk = chunk.or_else(|| part.and_then(|part| part.text));
                    if let Some(chunk) = chunk
                        && !chunk.is_empty()
                    {
                        text.push_str(&chunk);
                        emit(ModelStreamEvent::TextDelta { content: chunk });
                    }
                }
                ProcessModelEvent::TextDelta { text: chunk } => {
                    if !chunk.is_empty() {
                        text.push_str(&chunk);
                        emit(ModelStreamEvent::TextDelta { content: chunk });
                    }
                }
                ProcessModelEvent::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    if response.is_some() {
                        return Err(ModelError::new("model emitted more than one response"));
                    }
                    response = Some(ModelResponse::ToolCall(ToolCall::new(id, name, arguments)));
                }
                ProcessModelEvent::Error { message } => {
                    return Err(ModelError::new(message));
                }
                ProcessModelEvent::Done => break,
                ProcessModelEvent::Ignored => {}
            }
        }

        let status = child.wait_until(deadline)?;
        let _ = input_rx.recv_timeout(Duration::from_millis(100));
        if !status.success() {
            let stderr = stderr_rx.recv().unwrap_or_default();
            let detail = stderr.trim();
            return Err(ModelError::new(if detail.is_empty() {
                format!("model process exited with {status}")
            } else {
                format!("model process exited with {status}: {detail}")
            }));
        }

        response
            .or_else(|| (!text.is_empty()).then_some(ModelResponse::Text(text)))
            .ok_or_else(|| ModelError::new("model process emitted no response"))
    }
}

struct ChildGuard {
    child: Child,
    waited: bool,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self {
            child,
            waited: false,
        }
    }

    fn wait_until(&mut self, deadline: Instant) -> Result<ExitStatus, ModelError> {
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.waited = true;
                    return Ok(status);
                }
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(PROCESS_POLL_INTERVAL);
                }
                Ok(None) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    self.waited = true;
                    return Err(ModelError::new("model process timed out"));
                }
                Err(error) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    self.waited = true;
                    return Err(ModelError::new(format!(
                        "failed to finish model process: {error}"
                    )));
                }
            }
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.waited {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.waited = true;
        }
    }
}

impl Model for ProcessModel {
    fn respond(&mut self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        let mut ignored = |_event: ModelStreamEvent| {};
        self.run_stream(request, &mut ignored)
    }

    fn respond_stream(
        &mut self,
        request: &ModelRequest,
        emit: &mut dyn FnMut(ModelStreamEvent),
    ) -> Result<ModelResponse, ModelError> {
        self.run_stream(request, emit)
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ProcessModelEvent {
    Text {
        text: Option<String>,
        part: Option<ProcessTextPart>,
    },
    TextDelta {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: ToolArguments,
    },
    Error {
        message: String,
    },
    Done,
    #[serde(other)]
    Ignored,
}

#[derive(Debug, Deserialize)]
struct ProcessTextPart {
    text: Option<String>,
}
