//! Local HTTP host for the browser terminal and server-side guest agent.

use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentEvent, ModelCapabilities, ModelRouter, ProcessModel, RouteRequest};
use crate::guest_tools::{SharedGuestVm, guest_coding_tool_registry_shared};
use crate::runtime::VmInstance;

const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_UPLOAD_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
struct AgentRequest {
    prompt: String,
}

#[derive(Debug, Deserialize)]
struct UploadRequest {
    name: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Serialize)]
struct UploadResponse {
    path: String,
    bytes: usize,
}

#[derive(Debug, Deserialize)]
struct ApprovalRequest {
    id: String,
    decision: String,
}

#[derive(Debug, Clone, Copy)]
enum ApprovalReply {
    Allow,
    Deny,
}

enum ApprovalState {
    Pending,
    Resolved(ApprovalReply),
}

struct ApprovalStore {
    states: Mutex<std::collections::BTreeMap<String, ApprovalState>>,
    changed: Condvar,
}

#[derive(Clone)]
struct ServerState {
    approvals: Arc<ApprovalStore>,
    guest_vm: SharedGuestVm,
    model_directory: PathBuf,
}

impl Default for ApprovalStore {
    fn default() -> Self {
        Self {
            states: Mutex::new(std::collections::BTreeMap::new()),
            changed: Condvar::new(),
        }
    }
}

impl ApprovalStore {
    fn wait(&self, id: &str) -> crate::agent::PermissionDecision {
        let mut states = self
            .states
            .lock()
            .expect("approval store lock should not poison");
        states
            .entry(id.to_owned())
            .or_insert(ApprovalState::Pending);

        loop {
            if matches!(states.get(id), Some(ApprovalState::Pending)) {
                let (next_states, timeout) = self
                    .changed
                    .wait_timeout(states, Duration::from_secs(120))
                    .expect("approval store lock should not poison");
                states = next_states;
                if timeout.timed_out() {
                    states.remove(id);
                    return crate::agent::PermissionDecision::Deny {
                        reason: "approval timed out".to_owned(),
                    };
                }
                continue;
            }

            match states.remove(id) {
                Some(ApprovalState::Resolved(ApprovalReply::Allow)) => {
                    return crate::agent::PermissionDecision::Allow;
                }
                Some(ApprovalState::Resolved(ApprovalReply::Deny)) => {
                    return crate::agent::PermissionDecision::Deny {
                        reason: "approval denied in browser".to_owned(),
                    };
                }
                Some(ApprovalState::Pending) | None => {
                    unreachable!("approval state changed unexpectedly")
                }
            }
        }
    }

    fn resolve(&self, id: String, reply: ApprovalReply) -> Result<(), String> {
        if id.trim().is_empty() {
            return Err("approval id is empty".to_owned());
        }
        let mut states = self
            .states
            .lock()
            .expect("approval store lock should not poison");
        match states.get_mut(&id) {
            Some(state @ ApprovalState::Pending) => *state = ApprovalState::Resolved(reply),
            Some(ApprovalState::Resolved(_)) => {
                return Err("approval was already resolved".to_owned());
            }
            None => {
                states.insert(id, ApprovalState::Resolved(reply));
            }
        }
        self.changed.notify_all();
        Ok(())
    }
}

/// Serve the browser terminal and a local server-side agent endpoint.
pub fn run(root: PathBuf) {
    let port = env::var("A_RVM_WEB_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8080);
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap_or_else(|error| {
        eprintln!("failed to bind browser host on port {port}: {error}");
        std::process::exit(2);
    });
    let model_directory = create_model_working_directory().unwrap_or_else(|error| {
        eprintln!("failed to create model runner directory: {error}");
        std::process::exit(2);
    });
    println!("A/RVM browser host: http://127.0.0.1:{port}/web/");
    println!("Agent endpoint: http://127.0.0.1:{port}/api/agent");
    println!("Upload endpoint: http://127.0.0.1:{port}/api/upload");
    println!("Approval endpoint: http://127.0.0.1:{port}/api/approval");
    let state = ServerState {
        approvals: Arc::new(ApprovalStore::default()),
        guest_vm: Arc::new(Mutex::new(VmInstance::new("browser"))),
        model_directory,
    };

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let root = root.clone();
                let state = state.clone();
                thread::spawn(move || handle_connection(stream, &root, &state));
            }
            Err(error) => eprintln!("browser connection failed: {error}"),
        }
    }
}

fn handle_connection(mut stream: TcpStream, root: &Path, state: &ServerState) {
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            write_response(
                &mut stream,
                400,
                "text/plain; charset=utf-8",
                error.as_bytes(),
            );
            return;
        }
    };

    match (request.method.as_str(), request.path.as_str()) {
        ("GET" | "HEAD", "/") => write_redirect(&mut stream, "/web/"),
        ("GET" | "HEAD", "/web/") | ("GET" | "HEAD", "/web/index.html") => serve_file(
            &mut stream,
            root,
            "web/index.html",
            "text/html; charset=utf-8",
        ),
        ("GET" | "HEAD", "/web/main.js") => serve_file(
            &mut stream,
            root,
            "web/main.js",
            "text/javascript; charset=utf-8",
        ),
        ("GET" | "HEAD", "/web/styles.css") => serve_file(
            &mut stream,
            root,
            "web/styles.css",
            "text/css; charset=utf-8",
        ),
        ("GET" | "HEAD", "/target/wasm32-unknown-unknown/debug/a_rust_vm.wasm") => serve_file(
            &mut stream,
            root,
            "target/wasm32-unknown-unknown/debug/a_rust_vm.wasm",
            "application/wasm",
        ),
        ("POST", "/api/agent") => handle_agent(&mut stream, root, &request.body, state),
        ("POST", "/api/upload") => handle_upload(&mut stream, &request.body, state),
        ("POST", "/api/approval") => handle_approval(&mut stream, &request.body, &state.approvals),
        _ => write_response(&mut stream, 404, "text/plain; charset=utf-8", b"not found"),
    }
}

struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, String> {
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|error| format!("failed to clone connection: {error}"))?,
    );
    let mut header_bytes = Vec::new();
    loop {
        let mut line = Vec::new();
        reader
            .read_until(b'\n', &mut line)
            .map_err(|error| format!("failed to read request: {error}"))?;
        if line.is_empty() {
            return Err("request ended before headers".to_owned());
        }
        header_bytes.extend_from_slice(&line);
        if header_bytes.len() > MAX_REQUEST_BYTES {
            return Err("request headers are too large".to_owned());
        }
        if header_bytes.ends_with(b"\r\n\r\n") || header_bytes.ends_with(b"\n\n") {
            break;
        }
    }

    let header_text = String::from_utf8(header_bytes)
        .map_err(|_| "request headers are not valid UTF-8".to_owned())?;
    let mut lines = header_text.lines();
    let request_line = lines
        .next()
        .ok_or_else(|| "missing request line".to_owned())?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| "missing HTTP method".to_owned())?
        .to_owned();
    let path = request_parts
        .next()
        .ok_or_else(|| "missing HTTP path".to_owned())?
        .split('?')
        .next()
        .unwrap_or_default()
        .to_owned();
    let content_length = lines
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name.eq_ignore_ascii_case("content-length"))
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    if content_length > MAX_REQUEST_BYTES {
        return Err("request body is too large".to_owned());
    }

    let mut body = vec![0; content_length];
    reader
        .read_exact(&mut body)
        .map_err(|error| format!("failed to read request body: {error}"))?;
    Ok(HttpRequest { method, path, body })
}

fn handle_agent(stream: &mut TcpStream, _root: &Path, body: &[u8], state: &ServerState) {
    let request = match serde_json::from_slice::<AgentRequest>(body) {
        Ok(request) if !request.prompt.trim().is_empty() => request,
        Ok(_) => {
            write_response(
                stream,
                400,
                "application/json",
                br#"{"error":"prompt is empty"}"#,
            );
            return;
        }
        Err(error) => {
            let body = format!(r#"{{"error":"invalid request: {error}"}}"#);
            write_response(stream, 400, "application/json", body.as_bytes());
            return;
        }
    };

    let model_program = env::current_exe().unwrap_or_else(|error| {
        let body = format!(r#"{{"error":"failed to resolve model bridge: {error}"}}"#);
        write_response(stream, 500, "application/json", body.as_bytes());
        std::process::exit(1);
    });
    let model_name = env::var("A_RVM_MODEL_NAME").unwrap_or_else(|_| "configured".to_owned());
    let model = ProcessModel::new(model_program)
        .with_arguments(["model-bridge"])
        .with_working_directory(state.model_directory.clone());
    let mut router = ModelRouter::new();
    if let Err(error) = router
        .register_model(&model_name, ModelCapabilities::STREAMING_TOOLS, model)
        .and_then(|_| router.set_default(&model_name))
    {
        let body = format!(r#"{{"error":"failed to configure model route: {error}"}}"#);
        write_response(stream, 500, "application/json", body.as_bytes());
        return;
    }

    let mut agent = Agent::new(
        router,
        match guest_coding_tool_registry_shared(state.guest_vm.clone()) {
            Ok(tools) => tools,
            Err(error) => {
                let body = format!(r#"{{"error":"failed to configure tools: {error}"}}"#);
                write_response(stream, 500, "application/json", body.as_bytes());
                return;
            }
        },
    )
    .with_system_prompt(guest_system_prompt())
    .with_route_request(RouteRequest {
        requires_tools: true,
        ..RouteRequest::default()
    });

    write_stream_headers(stream);
    let result = agent.run_streaming_with_approval(
        request.prompt,
        |permission| state.approvals.wait(&permission.id),
        |event| write_event(stream, &event),
    );
    if let Err(error) = result {
        write_event(
            stream,
            &AgentEvent::Error {
                message: format!("agent error: {error}"),
            },
        );
    }
}

fn handle_upload(stream: &mut TcpStream, body: &[u8], state: &ServerState) {
    let request = match serde_json::from_slice::<UploadRequest>(body) {
        Ok(request) => request,
        Err(error) => {
            let body = format!(r#"{{"error":"invalid upload request: {error}"}}"#);
            write_response(stream, 400, "application/json", body.as_bytes());
            return;
        }
    };
    if request.bytes.len() > MAX_UPLOAD_BYTES {
        let body = format!(
            r#"{{"error":"upload exceeds the {} byte limit"}}"#,
            MAX_UPLOAD_BYTES
        );
        write_response(stream, 400, "application/json", body.as_bytes());
        return;
    }
    let path = match guest_upload_path(&request.name) {
        Ok(path) => path,
        Err(error) => {
            let body = format!(r#"{{"error":"{error}"}}"#);
            write_response(stream, 400, "application/json", body.as_bytes());
            return;
        }
    };
    let mut vm = match state.guest_vm.lock() {
        Ok(vm) => vm,
        Err(_) => {
            write_response(
                stream,
                500,
                "application/json",
                br#"{"error":"guest VM lock is poisoned"}"#,
            );
            return;
        }
    };
    if let Err(error) = vm
        .mkdir("/workspace/uploads", true)
        .and_then(|()| vm.write_file(&path, &request.bytes))
    {
        let body = format!(r#"{{"error":"failed to store upload: {error}"}}"#);
        write_response(stream, 400, "application/json", body.as_bytes());
        return;
    }
    let response = UploadResponse {
        path,
        bytes: request.bytes.len(),
    };
    match serde_json::to_vec(&response) {
        Ok(body) => write_response(stream, 200, "application/json", &body),
        Err(error) => {
            let body = format!(r#"{{"error":"failed to encode upload response: {error}"}}"#);
            write_response(stream, 500, "application/json", body.as_bytes());
        }
    }
}

fn guest_upload_path(name: &str) -> Result<String, String> {
    if name.trim().is_empty() {
        return Err("file name is empty".to_owned());
    }
    if name.len() > 255 {
        return Err("file name is too long".to_owned());
    }
    if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err("file name must be a single path component".to_owned());
    }
    if name
        .chars()
        .any(|character| character == '\0' || character.is_control())
    {
        return Err("file name contains a control character".to_owned());
    }
    Ok(format!("/workspace/uploads/{name}"))
}

fn create_model_working_directory() -> Result<PathBuf, String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before the Unix epoch: {error}"))?
        .as_nanos();
    let base = env::temp_dir();
    let process_id = std::process::id();

    for attempt in 0..16 {
        let path = base.join(format!(
            "a-rust-vm-model-{process_id}-{timestamp}-{attempt}"
        ));
        match fs::create_dir(&path) {
            Ok(()) => {
                let output = Command::new("git")
                    .args(["init", "--quiet"])
                    .current_dir(&path)
                    .output()
                    .map_err(|error| {
                        format!("failed to initialize model runner project: {error}")
                    })?;
                if !output.status.success() {
                    let detail = String::from_utf8_lossy(&output.stderr);
                    return Err(format!(
                        "failed to initialize model runner project: {}",
                        detail.trim()
                    ));
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!("failed to create model runner directory: {error}"));
            }
        }
    }

    Err("could not allocate a unique model runner directory".to_owned())
}

fn handle_approval(stream: &mut TcpStream, body: &[u8], approvals: &ApprovalStore) {
    let request = match serde_json::from_slice::<ApprovalRequest>(body) {
        Ok(request) => request,
        Err(error) => {
            let body = format!(r#"{{"error":"invalid approval request: {error}"}}"#);
            write_response(stream, 400, "application/json", body.as_bytes());
            return;
        }
    };
    let reply = match request.decision.as_str() {
        "allow" => ApprovalReply::Allow,
        "deny" => ApprovalReply::Deny,
        _ => {
            write_response(
                stream,
                400,
                "application/json",
                br#"{"error":"decision must be 'allow' or 'deny'"}"#,
            );
            return;
        }
    };
    match approvals.resolve(request.id, reply) {
        Ok(()) => write_response(stream, 200, "application/json", br#"{"ok":true}"#),
        Err(error) => {
            let body = format!(r#"{{"error":"{error}"}}"#);
            write_response(stream, 409, "application/json", body.as_bytes());
        }
    }
}

fn write_stream_headers(stream: &mut TcpStream) {
    let header = "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.flush();
}

fn write_event(stream: &mut TcpStream, event: &AgentEvent) {
    let Ok(mut body) = serde_json::to_vec(event) else {
        return;
    };
    body.push(b'\n');
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

fn guest_system_prompt() -> String {
    format!(
        "{}\n\nYou are operating inside an isolated guest VM. Use guest-prefixed tools for all files and processes. Guest files are not host files; do not claim host workspace changes.",
        crate::agent::DEFAULT_SYSTEM_PROMPT
    )
}

fn serve_file(stream: &mut TcpStream, root: &Path, relative_path: &str, content_type: &str) {
    let path = root.join(relative_path);
    match fs::read(path) {
        Ok(body) => write_response(stream, 200, content_type, &body),
        Err(_) => write_response(stream, 404, "text/plain; charset=utf-8", b"not found"),
    }
}

fn write_redirect(stream: &mut TcpStream, location: &str) {
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let _ = stream.write_all(response.as_bytes());
}

fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
}

#[cfg(test)]
mod tests {
    use super::{ApprovalReply, ApprovalStore, guest_upload_path};
    use crate::agent::PermissionDecision;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn approval_store_releases_a_waiting_agent() {
        let store = Arc::new(ApprovalStore::default());
        let waiting = store.clone();
        let handle = thread::spawn(move || waiting.wait("permission-1"));

        thread::sleep(Duration::from_millis(10));
        store
            .resolve("permission-1".to_owned(), ApprovalReply::Allow)
            .unwrap();

        assert_eq!(handle.join().unwrap(), PermissionDecision::Allow);
    }

    #[test]
    fn approval_store_accepts_a_decision_that_races_registration() {
        let store = ApprovalStore::default();
        store
            .resolve("permission-2".to_owned(), ApprovalReply::Deny)
            .unwrap();

        assert_eq!(
            store.wait("permission-2"),
            PermissionDecision::Deny {
                reason: "approval denied in browser".to_owned()
            }
        );
    }

    #[test]
    fn upload_names_are_contained_in_the_guest_upload_directory() {
        assert_eq!(
            guest_upload_path("notes.txt").unwrap(),
            "/workspace/uploads/notes.txt"
        );
        assert!(guest_upload_path("../notes.txt").is_err());
        assert!(guest_upload_path("nested/notes.txt").is_err());
        assert!(guest_upload_path("\\notes.txt").is_err());
    }
}
