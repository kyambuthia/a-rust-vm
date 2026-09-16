//! Local HTTP host for the browser terminal and server-side guest agent.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::agent::{Agent, AgentEvent, ModelCapabilities, ModelRouter, RouteRequest};
use crate::anon_session::{
    check_origin, cookie_header, extract_sid, generate_sid, is_valid_sid_format,
};
use crate::apps::{
    AppDescriptor, AppId, DOCS_DOCUMENT_PATH, Document, SHEETS_INPUT_PATH, SheetFormat,
    SheetSummary, app_descriptors, append_document, import_sheet, import_sheet_records, open_docs,
    open_sheets, read_document, replace_document, replace_document_with_metadata,
};
use crate::format_readers::{read_document as read_document_upload, read_spreadsheet};
use crate::guest_tools::guest_coding_tool_registry_shared;
use crate::jobs::{
    JobRecord, JobStore, PdfSummary, TableSummary, inspect_uploaded_pdf, tabulate_uploaded_file,
};
use crate::openrouter::OpenRouterModel;
use crate::runtime::VmInstance;

const MAX_REQUEST_HEADER_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_UPLOAD_BYTES: usize = 1024 * 1024;
const MAX_PROMPT_CHARS: usize = 8 * 1024;
const MAX_FILES_PER_SESSION: usize = 32;
const MAX_TOTAL_BYTES_PER_SESSION: usize = 8 * 1024 * 1024;
const MAX_JOBS_PER_SESSION: usize = 64;
const MAX_GLOBAL_SESSIONS: usize = 128;
const AGENT_TTL_STEP_LIMIT: usize = 8;
const SESSION_TTL: Duration = Duration::from_secs(1800);
const MAX_GLOBAL_CONCURRENT_AGENTS: usize = 32;
const MAX_ACTIVE_CONNECTIONS: usize = 256;
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_AGENT_EVENT_BYTES: usize = 256 * 1024;
const MAX_AGENT_STREAM_BYTES: usize = 2 * 1024 * 1024;

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
struct TabulateRequest {
    path: String,
}

#[derive(Debug, Serialize)]
struct TabulateResponse {
    job: JobRecord,
    table: TableSummary,
}

#[derive(Debug, Deserialize)]
struct PdfInspectionRequest {
    path: String,
}

#[derive(Debug, Serialize)]
struct PdfInspectionResponse {
    job: JobRecord,
    pdf: PdfSummary,
}

#[derive(Debug, Deserialize)]
struct AppOperationRequest {
    app: AppId,
    operation: String,
    text: Option<String>,
    format: Option<SheetFormat>,
    file: Option<String>,
}

#[derive(Debug, Serialize)]
struct AppsResponse {
    apps: [AppDescriptor; 2],
}

#[derive(Debug, Serialize)]
struct AppOperationResponse {
    job: JobRecord,
    app: AppId,
    operation: String,
    document: Option<Document>,
    sheet: Option<SheetSummary>,
}

#[derive(Debug, Serialize)]
struct JobsResponse {
    jobs: Vec<JobRecord>,
}

#[derive(Debug, Serialize)]
struct GuestFile {
    path: String,
    bytes: usize,
}

#[derive(Debug, Serialize)]
struct GuestFilesResponse {
    files: Vec<GuestFile>,
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
    Cancelled,
}

enum ApprovalState {
    Pending,
    Resolved(ApprovalReply),
}

pub struct ApprovalStore {
    states: Mutex<BTreeMap<String, ApprovalState>>,
    changed: Condvar,
}

struct SessionData {
    id: String,
    vm: Arc<Mutex<VmInstance>>,
    jobs: Arc<Mutex<JobStore>>,
    approvals: Arc<ApprovalStore>,
    last_seen: Mutex<Instant>,
    agent_active: Mutex<bool>,
    agent_cancel: Arc<AtomicBool>,
    rate_window: Mutex<Instant>,
    rate_count: Mutex<usize>,
}

struct AnonStore {
    sessions: Mutex<BTreeMap<String, Arc<SessionData>>>,
    secure_cookie: bool,
}

struct ServerState {
    anon: Arc<AnonStore>,
    model: Option<OpenRouterModel>,
    global_agents: Arc<Mutex<usize>>,
    active_connections: Arc<Mutex<usize>>,
    allowed_origin: String,
}

impl Default for ApprovalStore {
    fn default() -> Self {
        Self {
            states: Mutex::new(BTreeMap::new()),
            changed: Condvar::new(),
        }
    }
}

impl ApprovalStore {
    pub fn wait(&self, id: &str) -> crate::agent::PermissionDecision {
        static NEVER_CANCEL: AtomicBool = AtomicBool::new(false);
        self.wait_with_cancel(id, &NEVER_CANCEL)
    }

    fn wait_with_cancel(&self, id: &str, cancel: &AtomicBool) -> crate::agent::PermissionDecision {
        let mut states = self.states.lock().expect("approval poisoned");
        states
            .entry(id.to_owned())
            .or_insert(ApprovalState::Pending);
        loop {
            if cancel.load(Ordering::Relaxed) {
                states.remove(id);
                return crate::agent::PermissionDecision::Deny {
                    reason: "approval cancelled".to_owned(),
                };
            }
            if matches!(states.get(id), Some(ApprovalState::Pending)) {
                let (next, timeout) = self
                    .changed
                    .wait_timeout(states, Duration::from_secs(120))
                    .expect("condvar");
                states = next;
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
                Some(ApprovalState::Resolved(ApprovalReply::Cancelled)) => {
                    return crate::agent::PermissionDecision::Deny {
                        reason: "approval cancelled".to_owned(),
                    };
                }
                _ => unreachable!(),
            }
        }
    }

    fn cancel_pending(&self) {
        let Ok(mut states) = self.states.lock() else {
            return;
        };
        for state in states.values_mut() {
            if matches!(state, ApprovalState::Pending) {
                *state = ApprovalState::Resolved(ApprovalReply::Cancelled);
            }
        }
        self.changed.notify_all();
    }

    fn resolve(&self, id: String, reply: ApprovalReply) -> Result<(), String> {
        if id.trim().is_empty() {
            return Err("approval id is empty".to_owned());
        }
        let mut states = self.states.lock().expect("approval poisoned");
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

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct AgentActivityGuard<'a> {
    session: &'a SessionData,
    global_agents: &'a Arc<Mutex<usize>>,
}

struct ConnectionGuard {
    active_connections: Arc<Mutex<usize>>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let mut active = lock_recover(&self.active_connections);
        *active = active.saturating_sub(1);
    }
}

impl Drop for AgentActivityGuard<'_> {
    fn drop(&mut self) {
        *lock_recover(&self.session.agent_active) = false;
        let mut global = lock_recover(self.global_agents);
        *global = global.saturating_sub(1);
    }
}

impl AnonStore {
    #[cfg(test)]
    fn new() -> Self {
        Self::with_secure_cookie(false)
    }

    fn with_secure_cookie(secure_cookie: bool) -> Self {
        Self {
            sessions: Mutex::new(BTreeMap::new()),
            secure_cookie,
        }
    }
    fn resolve(
        &self,
        cookie_header_value: Option<&str>,
    ) -> Result<(Arc<SessionData>, Option<String>), String> {
        let now = Instant::now();
        let mut map = self.sessions.lock().expect("anon store poisoned");
        let expired: Vec<String> = map
            .iter()
            .filter(|(_, s)| now.duration_since(*s.last_seen.lock().unwrap()) > SESSION_TTL)
            .map(|(k, _)| k.clone())
            .collect();
        for k in expired {
            map.remove(&k);
        }
        #[allow(clippy::collapsible_if)]
        if let Some(raw) = cookie_header_value.and_then(extract_sid) {
            if is_valid_sid_format(&raw) {
                if let Some(sess) = map.get(&raw).cloned() {
                    if now.duration_since(*sess.last_seen.lock().unwrap()) <= SESSION_TTL {
                        *sess.last_seen.lock().unwrap() = now;
                        return Ok((sess, None));
                    }
                    map.remove(&raw);
                }
            }
        }
        #[allow(clippy::collapsible_if)]
        if map.len() >= MAX_GLOBAL_SESSIONS {
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, s)| *s.last_seen.lock().unwrap())
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest);
            }
        }
        let id = generate_sid()
            .ok_or_else(|| "secure anonymous session generation unavailable".to_owned())?;
        let sess = Arc::new(SessionData {
            id: id.clone(),
            vm: Arc::new(Mutex::new(VmInstance::new(id.clone()))),
            jobs: Arc::new(Mutex::new(JobStore::default())),
            approvals: Arc::new(ApprovalStore::default()),
            last_seen: Mutex::new(now),
            agent_active: Mutex::new(false),
            agent_cancel: Arc::new(AtomicBool::new(false)),
            rate_window: Mutex::new(now),
            rate_count: Mutex::new(0),
        });
        map.insert(id.clone(), sess.clone());
        Ok((sess, Some(cookie_header(&id, self.secure_cookie))))
    }
}

pub fn run(root: PathBuf) {
    let port = env::var("A_RVM_WEB_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(8080);
    let allowed_origin = env::var("A_RVM_ALLOWED_ORIGIN")
        .ok()
        .filter(|origin| !origin.trim().is_empty())
        .unwrap_or_else(|| format!("http://127.0.0.1:{port}"));
    let secure_cookie = env::var("A_RVM_SECURE_COOKIES")
        .ok()
        .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or_else(|| allowed_origin.starts_with("https://"));
    let bind_address = env::var("A_RVM_BIND_ADDRESS").unwrap_or_else(|_| "127.0.0.1".to_owned());
    let listener = TcpListener::bind((bind_address.as_str(), port)).unwrap_or_else(|e| {
        eprintln!("failed to bind browser host on port {port}: {e}");
        std::process::exit(2);
    });
    println!("A/RVM browser host: http://127.0.0.1:{port}/web/");
    let model = match OpenRouterModel::from_env() {
        Ok(model) => {
            println!("A/RVM model: {} via OpenRouter", model.model_name());
            Some(model)
        }
        Err(error) => {
            eprintln!("A/RVM model unavailable: {error}; /api/agent will return 503");
            None
        }
    };
    let state = Arc::new(ServerState {
        anon: Arc::new(AnonStore::with_secure_cookie(secure_cookie)),
        model,
        global_agents: Arc::new(Mutex::new(0)),
        active_connections: Arc::new(Mutex::new(0)),
        allowed_origin,
    });
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let active_connections = state.active_connections.clone();
                let connection_reserved = {
                    let mut active = lock_recover(&active_connections);
                    if *active >= MAX_ACTIVE_CONNECTIONS {
                        false
                    } else {
                        *active += 1;
                        true
                    }
                };
                if !connection_reserved {
                    let mut stream = stream;
                    write_error(&mut stream, 503, "server is busy", None);
                    continue;
                }
                let root = root.clone();
                let state = state.clone();
                let failed_counter = active_connections.clone();
                let result = thread::Builder::new()
                    .name("arvm-http".to_owned())
                    .spawn(move || {
                        let _connection = ConnectionGuard { active_connections };
                        handle_connection(stream, &root, &state);
                    });
                if let Err(error) = result {
                    eprintln!("failed to start browser connection handler: {error}");
                    let mut active = lock_recover(&failed_counter);
                    *active = active.saturating_sub(1);
                }
            }
            Err(e) => eprintln!("browser connection failed: {e}"),
        }
    }
}

fn handle_connection(mut stream: TcpStream, root: &Path, state: &ServerState) {
    let _ = stream.set_read_timeout(Some(HTTP_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HTTP_WRITE_TIMEOUT));
    let request = match read_request(&mut stream) {
        Ok(r) => r,
        Err(e) => {
            write_error(&mut stream, 400, &e, None);
            return;
        }
    };
    let cookie_val = request.headers.get("cookie").map(|s| s.as_str());
    let is_state_changing = matches!(request.method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE");
    if is_state_changing && !check_origin(&request.headers, &state.allowed_origin) {
        write_error(&mut stream, 403, "origin not allowed", None);
        return;
    }
    let has_valid_session = cookie_val
        .and_then(extract_sid)
        .is_some_and(|sid| is_valid_sid_format(&sid));
    if is_state_changing && !has_valid_session {
        write_error(&mut stream, 401, "anonymous session required", None);
        return;
    }
    if request.method == "GET" && request.path == "/healthz" {
        write_response(&mut stream, 200, "text/plain; charset=utf-8", b"ok\n", None);
        return;
    }
    let (session, set_cookie) = match state.anon.resolve(cookie_val) {
        Ok(value) => value,
        Err(_) => {
            write_error(&mut stream, 503, "anonymous sessions unavailable", None);
            return;
        }
    };
    let rate_hit = {
        let now = Instant::now();
        let mut w = session.rate_window.lock().unwrap();
        let mut c = session.rate_count.lock().unwrap();
        if now.duration_since(*w) > Duration::from_secs(60) {
            *w = now;
            *c = 1;
            false
        } else {
            *c += 1;
            *c > 60
        }
    };
    if rate_hit {
        write_error(
            &mut stream,
            429,
            "rate limit exceeded",
            set_cookie.as_deref(),
        );
        return;
    }
    *session.last_seen.lock().unwrap() = Instant::now();
    match (request.method.as_str(), request.path.as_str()) {
        ("GET" | "HEAD", "/") => write_redirect(&mut stream, "/web/", set_cookie.as_deref()),
        ("GET" | "HEAD", "/web/") | ("GET" | "HEAD", "/web/index.html") => serve_file(
            &mut stream,
            root,
            "web/index.html",
            "text/html; charset=utf-8",
            set_cookie.as_deref(),
        ),
        ("GET" | "HEAD", "/web/main.js") => serve_file(
            &mut stream,
            root,
            "web/main.js",
            "text/javascript; charset=utf-8",
            set_cookie.as_deref(),
        ),
        ("GET" | "HEAD", "/web/styles.css") => serve_file(
            &mut stream,
            root,
            "web/styles.css",
            "text/css; charset=utf-8",
            set_cookie.as_deref(),
        ),
        ("GET" | "HEAD", "/target/wasm32-unknown-unknown/debug/a_rust_vm.wasm") => serve_file(
            &mut stream,
            root,
            "target/wasm32-unknown-unknown/debug/a_rust_vm.wasm",
            "application/wasm",
            set_cookie.as_deref(),
        ),
        ("GET" | "HEAD", "/api/wasm") => serve_wasm(&mut stream, root, set_cookie.as_deref()),
        ("GET", "/api/v1/system") => handle_system_info(&mut stream, state, set_cookie.as_deref()),
        ("POST", "/api/agent") => handle_agent(
            &mut stream,
            &request.body,
            &session,
            state,
            set_cookie.as_deref(),
        ),
        ("POST", "/api/agent/cancel") => {
            handle_agent_cancel(&mut stream, &session, set_cookie.as_deref())
        }
        ("POST", "/api/upload") => {
            handle_upload(&mut stream, &request.body, &session, set_cookie.as_deref())
        }
        ("GET", "/api/files") => handle_files(&mut stream, &session, set_cookie.as_deref()),
        ("GET", "/api/apps") => handle_apps(&mut stream, set_cookie.as_deref()),
        ("POST", "/api/apps/operate") => {
            handle_app_operation(&mut stream, &request.body, &session, set_cookie.as_deref())
        }
        ("POST", "/api/tabulate") => {
            handle_tabulate(&mut stream, &request.body, &session, set_cookie.as_deref())
        }
        ("POST", "/api/pdf/inspect") => {
            handle_pdf_inspection(&mut stream, &request.body, &session, set_cookie.as_deref())
        }
        ("GET", "/api/jobs") => handle_jobs(&mut stream, &session, set_cookie.as_deref()),
        ("POST", "/api/approval") => {
            handle_approval(&mut stream, &request.body, &session, set_cookie.as_deref())
        }
        _ => write_error(&mut stream, 404, "not found", set_cookie.as_deref()),
    }
}

fn handle_system_info(stream: &mut TcpStream, state: &ServerState, set_cookie: Option<&str>) {
    match system_info_payload(state.model.is_some()) {
        Ok(body) => write_response(stream, 200, "application/json", &body, set_cookie),
        Err(_) => write_error(stream, 500, "internal error", set_cookie),
    }
}

fn system_info_payload(model_configured: bool) -> Result<Vec<u8>, serde_json::Error> {
    let mut info = serde_json::to_value(crate::protocol::SystemInfo::current())?;
    info["model_gateway"] = serde_json::json!({
        "configured": model_configured,
    });
    serde_json::to_vec(&info)
}

struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
    headers: BTreeMap<String, String>,
}

fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, String> {
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| format!("failed to clone connection: {e}"))?,
    );
    let mut header_bytes = Vec::new();
    loop {
        let mut line = Vec::new();
        reader
            .read_until(b'\n', &mut line)
            .map_err(|e| format!("failed to read request: {e}"))?;
        if line.is_empty() {
            return Err("request ended before headers".to_owned());
        }
        header_bytes.extend_from_slice(&line);
        if header_bytes.len() > MAX_REQUEST_HEADER_BYTES {
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
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| "missing HTTP method".to_owned())?
        .to_owned();
    let path = parts
        .next()
        .ok_or_else(|| "missing HTTP path".to_owned())?
        .split('?')
        .next()
        .unwrap_or_default()
        .to_owned();
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let content_length = match headers.get("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| "content-length is invalid".to_owned())?,
        None => 0,
    };
    if content_length > MAX_REQUEST_BODY_BYTES {
        return Err("request body is too large".to_owned());
    }
    let mut body = vec![0; content_length];
    reader
        .read_exact(&mut body)
        .map_err(|e| format!("failed to read request body: {e}"))?;
    Ok(HttpRequest {
        method,
        path,
        body,
        headers,
    })
}

fn handle_agent(
    stream: &mut TcpStream,
    body: &[u8],
    session: &Arc<SessionData>,
    state: &ServerState,
    set_cookie: Option<&str>,
) {
    let request = match serde_json::from_slice::<AgentRequest>(body) {
        Ok(r) if !r.prompt.trim().is_empty() => r,
        Ok(_) => {
            write_error(stream, 400, "prompt is empty", set_cookie);
            return;
        }
        Err(_) => {
            write_error(stream, 400, "invalid request", set_cookie);
            return;
        }
    };
    if request.prompt.chars().count() > MAX_PROMPT_CHARS {
        write_error(stream, 400, "prompt exceeds limit", set_cookie);
        return;
    }
    {
        let mut active = lock_recover(&session.agent_active);
        if *active {
            write_error(
                stream,
                429,
                "agent already running for this session",
                set_cookie,
            );
            return;
        }
        *active = true;
    }
    session.agent_cancel.store(false, Ordering::SeqCst);
    {
        let mut global = lock_recover(&state.global_agents);
        if *global >= MAX_GLOBAL_CONCURRENT_AGENTS {
            *lock_recover(&session.agent_active) = false;
            write_error(stream, 429, "too many concurrent agents", set_cookie);
            return;
        }
        *global += 1;
    }
    let _activity = AgentActivityGuard {
        session,
        global_agents: &state.global_agents,
    };
    let Some(model) = state.model.clone() else {
        write_error(stream, 503, "model is not configured", set_cookie);
        return;
    };
    let mut router = ModelRouter::new();
    if let Err(e) = router
        .register_model("guest", ModelCapabilities::STREAMING_TOOLS, model)
        .and_then(|_| router.set_default("guest"))
    {
        write_error(stream, 500, "internal error", set_cookie);
        let _ = e;
        return;
    }
    let vm_clone = session.vm.clone();
    let mut agent = Agent::new(
        router,
        match guest_coding_tool_registry_shared(vm_clone.clone()) {
            Ok(t) => t,
            Err(_) => {
                write_error(stream, 500, "internal error", set_cookie);
                return;
            }
        },
    )
    .with_system_prompt(guest_system_prompt(&vm_clone))
    .with_route_request(RouteRequest {
        requires_tools: true,
        ..RouteRequest::default()
    })
    .with_max_steps(AGENT_TTL_STEP_LIMIT)
    .with_cancel_token(session.agent_cancel.clone());
    write_stream_headers(stream, set_cookie);
    let approvals = session.approvals.clone();
    let mut stream_bytes = 0;
    let result = agent.run_streaming_with_approval(
        request.prompt,
        |p| approvals.wait_with_cancel(&p.id, session.agent_cancel.as_ref()),
        |event| {
            if !write_event(stream, &event, &mut stream_bytes) {
                session.agent_cancel.store(true, Ordering::SeqCst);
            }
        },
    );
    if let Err(e) = result {
        write_event(
            stream,
            &AgentEvent::Error {
                message: format!("agent error: {e}"),
            },
            &mut stream_bytes,
        );
        write_event(stream, &AgentEvent::Done, &mut stream_bytes);
    }
}

fn handle_agent_cancel(
    stream: &mut TcpStream,
    session: &Arc<SessionData>,
    set_cookie: Option<&str>,
) {
    if !*lock_recover(&session.agent_active) {
        write_error(stream, 409, "agent is not running", set_cookie);
        return;
    }
    session.agent_cancel.store(true, Ordering::SeqCst);
    session.approvals.cancel_pending();
    write_response(
        stream,
        200,
        "application/json",
        br#"{"ok":true}"#,
        set_cookie,
    );
}

fn handle_upload(
    stream: &mut TcpStream,
    body: &[u8],
    session: &Arc<SessionData>,
    set_cookie: Option<&str>,
) {
    let request = match serde_json::from_slice::<UploadRequest>(body) {
        Ok(r) => r,
        Err(_) => {
            write_error(stream, 400, "invalid upload request", set_cookie);
            return;
        }
    };
    if request.bytes.len() > MAX_UPLOAD_BYTES {
        write_error(stream, 400, "upload exceeds limit", set_cookie);
        return;
    }
    let path = match guest_upload_path(&request.name) {
        Ok(p) => p,
        Err(e) => {
            write_error(stream, 400, &e, set_cookie);
            return;
        }
    };
    let mut vm = match session.vm.lock() {
        Ok(v) => v,
        Err(_) => {
            write_error(stream, 500, "internal error", set_cookie);
            return;
        }
    };
    let existing_files = vm
        .list_dir("/workspace/uploads")
        .unwrap_or_default()
        .iter()
        .filter(|e| matches!(e.kind, crate::runtime::EntryKind::File))
        .count();
    if existing_files >= MAX_FILES_PER_SESSION && vm.read_file(&path).is_err() {
        write_error(stream, 400, "file limit exceeded", set_cookie);
        return;
    }
    let previous_bytes = vm.read_file(&path).map(|bytes| bytes.len()).unwrap_or(0);
    let projected_bytes = vm
        .filesystem()
        .byte_count()
        .checked_sub(previous_bytes)
        .and_then(|bytes| bytes.checked_add(request.bytes.len()));
    if projected_bytes.is_none_or(|bytes| bytes > MAX_TOTAL_BYTES_PER_SESSION) {
        write_error(stream, 400, "session storage limit exceeded", set_cookie);
        return;
    }
    if let Err(e) = vm
        .mkdir("/workspace/uploads", true)
        .and_then(|_| vm.write_file(&path, &request.bytes))
    {
        write_error(stream, 400, "failed to store upload", set_cookie);
        let _ = e;
        return;
    }
    let resp = UploadResponse {
        path,
        bytes: request.bytes.len(),
    };
    match serde_json::to_vec(&resp) {
        Ok(b) => write_response(stream, 200, "application/json", &b, set_cookie),
        Err(_) => write_error(stream, 500, "internal error", set_cookie),
    }
}

fn handle_files(stream: &mut TcpStream, session: &Arc<SessionData>, set_cookie: Option<&str>) {
    let vm = match session.vm.lock() {
        Ok(v) => v,
        Err(_) => {
            write_error(stream, 500, "internal error", set_cookie);
            return;
        }
    };
    let files = vm
        .list_dir("/workspace/uploads")
        .unwrap_or_default()
        .into_iter()
        .filter(|e| matches!(e.kind, crate::runtime::EntryKind::File))
        .map(|e| GuestFile {
            path: format!("/workspace/uploads/{}", e.name),
            bytes: e.size,
        })
        .collect();
    match serde_json::to_vec(&GuestFilesResponse { files }) {
        Ok(b) => write_response(stream, 200, "application/json", &b, set_cookie),
        Err(_) => write_error(stream, 500, "internal error", set_cookie),
    }
}

fn handle_apps(stream: &mut TcpStream, set_cookie: Option<&str>) {
    match serde_json::to_vec(&AppsResponse {
        apps: app_descriptors(),
    }) {
        Ok(body) => write_response(stream, 200, "application/json", &body, set_cookie),
        Err(_) => write_error(stream, 500, "internal error", set_cookie),
    }
}

fn handle_app_operation(
    stream: &mut TcpStream,
    body: &[u8],
    session: &Arc<SessionData>,
    set_cookie: Option<&str>,
) {
    let request = match serde_json::from_slice::<AppOperationRequest>(body) {
        Ok(request) if !request.operation.trim().is_empty() => request,
        _ => {
            write_error(stream, 400, "invalid app operation", set_cookie);
            return;
        }
    };
    let operation = request.operation.to_ascii_lowercase();
    let (executor, input_path) = match request.app {
        AppId::Docs => ("docs", DOCS_DOCUMENT_PATH),
        AppId::Sheets => ("sheets", SHEETS_INPUT_PATH),
    };
    let job = match session.jobs.lock() {
        Ok(mut jobs) => {
            let Some(job) =
                jobs.try_start_builtin_app(&session.id, executor, input_path, MAX_JOBS_PER_SESSION)
            else {
                drop(jobs);
                write_error(stream, 400, "job limit exceeded", set_cookie);
                return;
            };
            jobs.mark_running(&job.id);
            job
        }
        Err(_) => {
            write_error(stream, 500, "internal error", set_cookie);
            return;
        }
    };

    let result = match (request.app, operation.as_str()) {
        (AppId::Docs, "open_upload") => request
            .file
            .as_deref()
            .ok_or_else(|| "uploaded document name is required".to_owned())
            .and_then(|file| import_uploaded_document(session, file))
            .map(|document| (Some(document), None)),
        (AppId::Sheets, "open_upload") => request
            .file
            .as_deref()
            .ok_or_else(|| "uploaded spreadsheet name is required".to_owned())
            .and_then(|file| import_uploaded_sheet(session, file))
            .map(|sheet| (None, Some(sheet))),
        _ => match session.vm.lock() {
            Ok(mut vm) => match (request.app, operation.as_str()) {
                (AppId::Docs, "open") => open_docs(&mut vm).map(|document| (Some(document), None)),
                (AppId::Docs, "read") => read_document(&vm).map(|document| (Some(document), None)),
                (AppId::Docs, "replace") => request
                    .text
                    .as_deref()
                    .ok_or_else(|| "document text is required".to_owned())
                    .and_then(|text| replace_document(&mut vm, text))
                    .map(|document| (Some(document), None)),
                (AppId::Docs, "append") => request
                    .text
                    .as_deref()
                    .ok_or_else(|| "document text is required".to_owned())
                    .and_then(|text| append_document(&mut vm, text))
                    .map(|document| (Some(document), None)),
                (AppId::Sheets, "open") => open_sheets(&mut vm).map(|sheet| (None, Some(sheet))),
                (AppId::Sheets, "import") => match (request.text.as_deref(), request.format) {
                    (Some(text), Some(format)) => {
                        import_sheet(&mut vm, text, format).map(|sheet| (None, Some(sheet)))
                    }
                    (None, _) => Err("sheet text is required".to_owned()),
                    (_, None) => Err("sheet format must be 'csv' or 'tsv'".to_owned()),
                },
                (AppId::Docs, _) => Err("unsupported Docs operation".to_owned()),
                (AppId::Sheets, _) => Err("unsupported Sheets operation".to_owned()),
            },
            Err(_) => Err("internal error".to_owned()),
        },
    };
    match result {
        Ok((document, sheet)) => {
            let output_path = document
                .as_ref()
                .map(|document| document.path)
                .or_else(|| sheet.as_ref().map(|sheet| sheet.output_path));
            let completed = session.jobs.lock().ok().and_then(|mut jobs| {
                jobs.finish(&job.id, output_path.ok_or("missing output path"))
            });
            match completed {
                Some(job) => match serde_json::to_vec(&AppOperationResponse {
                    job,
                    app: request.app,
                    operation,
                    document,
                    sheet,
                }) {
                    Ok(body) => write_response(stream, 200, "application/json", &body, set_cookie),
                    Err(_) => write_error(stream, 500, "internal error", set_cookie),
                },
                None => write_error(stream, 500, "internal error", set_cookie),
            }
        }
        Err(error) => {
            if let Ok(mut jobs) = session.jobs.lock() {
                jobs.finish(&job.id, Err(&error));
            }
            write_error(stream, 400, &error, set_cookie);
        }
    }
}

fn import_uploaded_document(session: &Arc<SessionData>, name: &str) -> Result<Document, String> {
    let path = guest_upload_path(name)?;
    let bytes = session
        .vm
        .lock()
        .map_err(|_| "internal error".to_owned())?
        .read_file(&path)
        .map_err(|error| error.to_string())?;
    let content = read_document_upload(name, &bytes)?;
    let mut vm = session.vm.lock().map_err(|_| "internal error".to_owned())?;
    replace_document_with_metadata(
        &mut vm,
        &content.text,
        (content.format, Some(name.to_owned()), content.warnings),
    )
}

fn import_uploaded_sheet(session: &Arc<SessionData>, name: &str) -> Result<SheetSummary, String> {
    let path = guest_upload_path(name)?;
    let bytes = session
        .vm
        .lock()
        .map_err(|_| "internal error".to_owned())?
        .read_file(&path)
        .map_err(|error| error.to_string())?;
    let content = read_spreadsheet(name, &bytes)?;
    let mut vm = session.vm.lock().map_err(|_| "internal error".to_owned())?;
    import_sheet_records(
        &mut vm,
        content.headers,
        content.rows,
        &content.format,
        content.sheet_name,
        content.warnings,
        &path,
    )
}

fn handle_tabulate(
    stream: &mut TcpStream,
    body: &[u8],
    session: &Arc<SessionData>,
    set_cookie: Option<&str>,
) {
    let request = match serde_json::from_slice::<TabulateRequest>(body) {
        Ok(r) if !r.path.trim().is_empty() => r,
        Ok(_) => {
            write_error(stream, 400, "path is empty", set_cookie);
            return;
        }
        Err(_) => {
            write_error(stream, 400, "invalid request", set_cookie);
            return;
        }
    };
    let job = {
        let mut jobs = session.jobs.lock().unwrap();
        let Some(j) = jobs.try_start_tabulation(&session.id, &request.path, MAX_JOBS_PER_SESSION)
        else {
            drop(jobs);
            write_error(stream, 400, "job limit exceeded", set_cookie);
            return;
        };
        jobs.mark_running(&j.id);
        j
    };
    let result = match session.vm.lock() {
        Ok(mut vm) => tabulate_uploaded_file(&mut vm, &request.path),
        Err(_) => Err("internal error".to_owned()),
    };
    match result {
        Ok(table) => {
            let completed = session
                .jobs
                .lock()
                .ok()
                .and_then(|mut j| j.finish(&job.id, Ok(&table.output_path)));
            match completed {
                Some(job) => match serde_json::to_vec(&TabulateResponse { job, table }) {
                    Ok(b) => write_response(stream, 200, "application/json", &b, set_cookie),
                    Err(_) => write_error(stream, 500, "internal error", set_cookie),
                },
                None => write_error(stream, 500, "internal error", set_cookie),
            }
        }
        Err(e) => {
            if let Ok(mut jobs) = session.jobs.lock() {
                jobs.finish(&job.id, Err(&e));
            }
            write_error(stream, 400, &e, set_cookie);
        }
    }
}

fn handle_jobs(stream: &mut TcpStream, session: &Arc<SessionData>, set_cookie: Option<&str>) {
    let jobs = match session.jobs.lock() {
        Ok(j) => j.list(&session.id),
        Err(_) => {
            write_error(stream, 500, "internal error", set_cookie);
            return;
        }
    };
    match serde_json::to_vec(&JobsResponse { jobs }) {
        Ok(b) => write_response(stream, 200, "application/json", &b, set_cookie),
        Err(_) => write_error(stream, 500, "internal error", set_cookie),
    }
}

fn handle_pdf_inspection(
    stream: &mut TcpStream,
    body: &[u8],
    session: &Arc<SessionData>,
    set_cookie: Option<&str>,
) {
    let request = match serde_json::from_slice::<PdfInspectionRequest>(body) {
        Ok(r) if !r.path.trim().is_empty() => r,
        Ok(_) => {
            write_error(stream, 400, "path is empty", set_cookie);
            return;
        }
        Err(_) => {
            write_error(stream, 400, "invalid request", set_cookie);
            return;
        }
    };
    let job = {
        let mut jobs = session.jobs.lock().unwrap();
        let Some(j) =
            jobs.try_start_pdf_inspection(&session.id, &request.path, MAX_JOBS_PER_SESSION)
        else {
            drop(jobs);
            write_error(stream, 400, "job limit exceeded", set_cookie);
            return;
        };
        jobs.mark_running(&j.id);
        j
    };
    let result = match session.vm.lock() {
        Ok(mut vm) => inspect_uploaded_pdf(&mut vm, &request.path),
        Err(_) => Err("internal error".to_owned()),
    };
    match result {
        Ok(pdf) => {
            let completed = session
                .jobs
                .lock()
                .ok()
                .and_then(|mut j| j.finish(&job.id, Ok(&pdf.output_path)));
            match completed {
                Some(job) => match serde_json::to_vec(&PdfInspectionResponse { job, pdf }) {
                    Ok(b) => write_response(stream, 200, "application/json", &b, set_cookie),
                    Err(_) => write_error(stream, 500, "internal error", set_cookie),
                },
                None => write_error(stream, 500, "internal error", set_cookie),
            }
        }
        Err(e) => {
            if let Ok(mut jobs) = session.jobs.lock() {
                jobs.finish(&job.id, Err(&e));
            }
            write_error(stream, 400, &e, set_cookie);
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
    if name.chars().any(|c| c == '\0' || c.is_control()) {
        return Err("file name contains a control character".to_owned());
    }
    Ok(format!("/workspace/uploads/{name}"))
}

fn handle_approval(
    stream: &mut TcpStream,
    body: &[u8],
    session: &Arc<SessionData>,
    set_cookie: Option<&str>,
) {
    let request = match serde_json::from_slice::<ApprovalRequest>(body) {
        Ok(r) => r,
        Err(_) => {
            write_error(stream, 400, "invalid approval request", set_cookie);
            return;
        }
    };
    let reply = match request.decision.as_str() {
        "allow" => ApprovalReply::Allow,
        "deny" => ApprovalReply::Deny,
        _ => {
            write_error(
                stream,
                400,
                "decision must be 'allow' or 'deny'",
                set_cookie,
            );
            return;
        }
    };
    match session.approvals.resolve(request.id, reply) {
        Ok(()) => write_response(
            stream,
            200,
            "application/json",
            br#"{"ok":true}"#,
            set_cookie,
        ),
        Err(e) => write_error_with_status(stream, 409, &e, set_cookie),
    }
}

fn write_stream_headers(stream: &mut TcpStream, set_cookie: Option<&str>) {
    let mut header = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\n",
    );
    append_security_headers(&mut header);
    if let Some(c) = set_cookie {
        header.push_str(&format!("Set-Cookie: {c}\r\n"));
    }
    header.push_str("\r\n");
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.flush();
}

fn write_event(stream: &mut TcpStream, event: &AgentEvent, bytes_written: &mut usize) -> bool {
    let Ok(mut body) = serde_json::to_vec(event) else {
        return false;
    };
    if body.len() > MAX_AGENT_EVENT_BYTES
        || bytes_written
            .checked_add(body.len() + 1)
            .is_none_or(|bytes| bytes > MAX_AGENT_STREAM_BYTES)
    {
        return false;
    }
    body.push(b'\n');
    *bytes_written += body.len();
    stream.write_all(&body).is_ok() && stream.flush().is_ok()
}

fn guest_system_prompt(vm: &Arc<Mutex<VmInstance>>) -> String {
    let mut prompt = format!(
        "{}\n\nYou are operating inside an isolated guest VM. Use guest-prefixed tools for all files and processes. Guest files are not host files; do not claim host workspace changes.",
        crate::agent::DEFAULT_SYSTEM_PROMPT
    );
    #[allow(clippy::collapsible_if)]
    if let Ok(vm) = vm.lock() {
        if let Ok(entries) = vm.list_dir("/workspace/uploads") {
            if !entries.is_empty() {
                prompt.push_str("\n\nUploaded guest files currently available (use guest_read_file with the exact path when asked about their contents):");
                for entry in entries {
                    if matches!(entry.kind, crate::runtime::EntryKind::File) {
                        prompt.push_str(&format!(
                            "\n- /workspace/uploads/{} ({} bytes)",
                            entry.name, entry.size
                        ));
                    }
                }
            }
        }
    }
    prompt
}

fn serve_file(
    stream: &mut TcpStream,
    root: &Path,
    relative_path: &str,
    content_type: &str,
    set_cookie: Option<&str>,
) {
    let path = root.join(relative_path);
    match fs::read(path) {
        Ok(body) => write_response(stream, 200, content_type, &body, set_cookie),
        Err(_) => write_error(stream, 404, "not found", set_cookie),
    }
}

fn wasm_candidates() -> [&'static str; 2] {
    [
        "target/wasm32-unknown-unknown/release/a_rust_vm.wasm",
        "target/wasm32-unknown-unknown/debug/a_rust_vm.wasm",
    ]
}

fn serve_wasm(stream: &mut TcpStream, root: &Path, set_cookie: Option<&str>) {
    for candidate in wasm_candidates() {
        let path = root.join(candidate);
        if let Ok(body) = fs::read(path) {
            write_response(stream, 200, "application/wasm", &body, set_cookie);
            return;
        }
    }
    write_error(stream, 404, "not found", set_cookie);
}

fn write_redirect(stream: &mut TcpStream, location: &str, set_cookie: Option<&str>) {
    let mut response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n"
    );
    append_security_headers(&mut response);
    if let Some(c) = set_cookie {
        response.push_str(&format!("Set-Cookie: {c}\r\n"));
    }
    response.push_str("\r\n");
    let _ = stream.write_all(response.as_bytes());
}

fn append_security_headers(header: &mut String) {
    header.push_str("X-Content-Type-Options: nosniff\r\n");
    header.push_str("X-Frame-Options: DENY\r\n");
    header.push_str("Referrer-Policy: no-referrer\r\n");
    header.push_str("Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'\r\n");
    header.push_str("Cross-Origin-Opener-Policy: same-origin\r\n");
    header.push_str("Cross-Origin-Resource-Policy: same-origin\r\n");
    header.push_str("Permissions-Policy: camera=(), microphone=(), geolocation=()\r\n");
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    set_cookie: Option<&str>,
) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let mut header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len()
    );
    append_security_headers(&mut header);
    if let Some(c) = set_cookie {
        header.push_str(&format!("Set-Cookie: {c}\r\n"));
    }
    header.push_str("\r\n");
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
}

fn write_error(stream: &mut TcpStream, status: u16, message: &str, set_cookie: Option<&str>) {
    write_error_with_status(stream, status, message, set_cookie)
}

fn write_error_with_status(
    stream: &mut TcpStream,
    status: u16,
    message: &str,
    set_cookie: Option<&str>,
) {
    let sanitized = sanitize_error(message);
    let body = serde_json::json!({"error": sanitized}).to_string();
    write_response(
        stream,
        status,
        "application/json",
        body.as_bytes(),
        set_cookie,
    );
}

fn sanitize_error(message: &str) -> String {
    let lower = message.to_ascii_lowercase();
    if lower.contains("/home")
        || lower.contains("/tmp")
        || lower.contains("credential")
        || lower.contains("stack trace")
        || lower.contains("backtrace")
    {
        return "request failed".to_owned();
    }
    if message.len() > 500 {
        message.chars().take(500).collect()
    } else {
        message.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{ApprovalReply, ApprovalStore, guest_system_prompt, guest_upload_path};
    use crate::agent::PermissionDecision;
    use crate::runtime::VmInstance;
    use std::io::{Cursor, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;
    use zip::{ZipWriter, write::SimpleFileOptions};

    fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

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
    fn approval_store_releases_a_waiting_agent_when_cancelled() {
        let store = Arc::new(ApprovalStore::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let waiting_store = store.clone();
        let waiting_cancel = cancel.clone();
        let handle = thread::spawn(move || {
            waiting_store.wait_with_cancel("permission-cancel", &waiting_cancel)
        });

        thread::sleep(Duration::from_millis(10));
        cancel.store(true, Ordering::Release);
        store.cancel_pending();

        assert_eq!(
            handle.join().unwrap(),
            PermissionDecision::Deny {
                reason: "approval cancelled".to_owned()
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

    #[test]
    fn guest_prompt_names_uploaded_files_without_exposing_host_paths() {
        let mut initial_vm = VmInstance::new("prompt");
        initial_vm.mkdir("/workspace/uploads", true).unwrap();
        initial_vm
            .write_file("/workspace/uploads/rag_review.md", vec![b'x'; 3])
            .unwrap();
        let vm = Arc::new(Mutex::new(initial_vm));
        let prompt = guest_system_prompt(&vm);
        assert!(prompt.contains("/workspace/uploads/rag_review.md (3 bytes)"));
        assert!(!prompt.contains("/home/"));
    }

    #[test]
    fn uploaded_office_files_are_read_outside_the_guest_vm_as_bounded_artifacts() {
        let store = super::AnonStore::new();
        let (session, _) = store.resolve(None).unwrap();
        let docx = archive(&[(
            "word/document.xml",
            "<w:document><w:body><w:p><w:r><w:t>Imported plan</w:t></w:r></w:p></w:body></w:document>",
        )]);
        {
            let mut vm = session.vm.lock().unwrap();
            vm.mkdir("/workspace/uploads", true).unwrap();
            vm.write_file("/workspace/uploads/plan.docx", docx).unwrap();
            vm.write_file("/workspace/uploads/sales.csv", "name,amount\nAda,12\n")
                .unwrap();
        }
        let document = super::import_uploaded_document(&session, "plan.docx").unwrap();
        assert_eq!(document.text, "Imported plan");
        assert_eq!(document.source_name.as_deref(), Some("plan.docx"));
        let sheet = super::import_uploaded_sheet(&session, "sales.csv").unwrap();
        assert_eq!(sheet.format, "csv");
        assert_eq!(sheet.columns[1].numeric, 1);
        assert!(super::import_uploaded_document(&session, "../plan.docx").is_err());
    }

    #[test]
    fn sanitize_error_hides_paths() {
        assert_eq!(
            super::sanitize_error("failed at /home/user/secret"),
            "request failed"
        );
    }

    #[test]
    fn sid_format_validation() {
        assert!(crate::anon_session::is_valid_sid_format(&"a".repeat(64)));
        assert!(!crate::anon_session::is_valid_sid_format("short"));
        assert_eq!(
            crate::anon_session::extract_sid("arvm_anon=abc; other=1"),
            Some("abc".to_owned())
        );
    }

    #[test]
    fn origin_check_rejects_cross_origin() {
        let mut headers = std::collections::BTreeMap::new();
        assert!(!crate::anon_session::check_origin(
            &headers,
            "https://asiliano.online"
        ));
        headers.insert("origin".to_owned(), "https://evil.com".to_owned());
        assert!(!crate::anon_session::check_origin(
            &headers,
            "https://asiliano.online"
        ));
        headers.insert("origin".to_owned(), "https://asiliano.online".to_owned());
        assert!(crate::anon_session::check_origin(
            &headers,
            "https://asiliano.online"
        ));
    }

    #[test]
    fn production_cookie_can_be_marked_secure() {
        let secure = crate::anon_session::cookie_header("a", true);
        let local = crate::anon_session::cookie_header("a", false);
        assert!(secure.contains("; Secure"));
        assert!(!local.contains("; Secure"));
        assert!(secure.contains("SameSite=Strict"));
    }

    #[test]
    fn anonymous_sessions_are_isolated() {
        let store = super::AnonStore::new();
        let (a, cookie_a) = store.resolve(None).unwrap();
        let cookie_a = cookie_a.expect("first session should set cookie");
        let sid_a = crate::anon_session::extract_sid(&cookie_a).unwrap();
        {
            let mut vm = a.vm.lock().unwrap();
            vm.mkdir("/workspace/uploads", true).unwrap();
            vm.write_file("/workspace/uploads/a.txt", b"secret-a")
                .unwrap();
        }
        a.jobs
            .lock()
            .unwrap()
            .start_tabulation(&a.id, "/workspace/uploads/a.txt");
        a.approvals
            .resolve("perm-a".to_owned(), ApprovalReply::Allow)
            .unwrap();

        let (b, cookie_b) = store.resolve(None).unwrap();
        let cookie_b = cookie_b.expect("second session should set cookie");
        let sid_b = crate::anon_session::extract_sid(&cookie_b).unwrap();
        assert_ne!(sid_a, sid_b);
        assert!(
            b.vm.lock()
                .unwrap()
                .read_file("/workspace/uploads/a.txt")
                .is_err()
        );
        assert!(b.jobs.lock().unwrap().list(&b.id).is_empty());
        assert!(
            b.approvals
                .resolve("perm-a".to_owned(), ApprovalReply::Allow)
                .is_ok()
        );

        let (a2, no_cookie) = store.resolve(Some(&cookie_a)).unwrap();
        assert!(no_cookie.is_none());
        assert_eq!(a2.id, a.id);
        assert_eq!(
            a2.vm
                .lock()
                .unwrap()
                .read_text("/workspace/uploads/a.txt")
                .unwrap(),
            "secret-a"
        );
    }

    #[test]
    fn tampered_cookie_is_rotated() {
        let store = super::AnonStore::new();
        let (a, cookie_a) = store.resolve(None).unwrap();
        let sid_a = crate::anon_session::extract_sid(&cookie_a.unwrap()).unwrap();
        let tampered = format!("arvm_anon={}x; other=1", &sid_a[..63]);
        let (b, cookie_b) = store.resolve(Some(&tampered)).unwrap();
        assert!(cookie_b.is_some());
        let sid_b = crate::anon_session::extract_sid(&cookie_b.unwrap()).unwrap();
        assert_ne!(sid_a, sid_b);
        assert_ne!(a.id, b.id);
        let (c, none) = store.resolve(Some(&format!("arvm_anon={sid_a}"))).unwrap();
        assert!(none.is_none());
        assert_eq!(c.id, a.id);
    }

    #[test]
    fn invalid_sid_formats_are_rotated() {
        let store = super::AnonStore::new();
        for bad in [
            "short",
            "",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            "a/b..",
        ] {
            let (_s, cookie) = store.resolve(Some(&format!("arvm_anon={bad}"))).unwrap();
            assert!(cookie.is_some(), "bad sid {bad} should rotate");
            let sid = crate::anon_session::extract_sid(&cookie.unwrap()).unwrap();
            assert!(crate::anon_session::is_valid_sid_format(&sid));
        }
    }

    #[test]
    fn limits_are_enforced() {
        assert_eq!(super::MAX_PROMPT_CHARS, 8192);
        assert_eq!(super::MAX_UPLOAD_BYTES, 1024 * 1024);
        assert_eq!(super::MAX_FILES_PER_SESSION, 32);
        assert_eq!(super::MAX_JOBS_PER_SESSION, 64);
        assert_eq!(super::AGENT_TTL_STEP_LIMIT, 8);
        assert_eq!(super::MAX_GLOBAL_SESSIONS, 128);
    }

    #[test]
    fn security_headers_present() {
        let mut header = String::new();
        super::append_security_headers(&mut header);
        assert!(header.contains("X-Content-Type-Options: nosniff"));
        assert!(header.contains("X-Frame-Options: DENY"));
        assert!(header.contains("Content-Security-Policy:"));
        assert!(header.contains("img-src 'self' data:"));
        assert!(header.contains("Cross-Origin-Opener-Policy: same-origin"));
    }

    #[test]
    fn system_info_reports_model_configuration_without_secrets() {
        let configured = String::from_utf8(super::system_info_payload(true).unwrap()).unwrap();
        let unavailable = String::from_utf8(super::system_info_payload(false).unwrap()).unwrap();
        assert!(configured.contains(r#""model_gateway":{"configured":true}"#));
        assert!(unavailable.contains(r#""model_gateway":{"configured":false}"#));
        assert!(!configured.contains("OPENROUTER_API_KEY"));
    }

    #[test]
    fn wasm_route_prefers_release_artifact() {
        assert_eq!(
            super::wasm_candidates(),
            [
                "target/wasm32-unknown-unknown/release/a_rust_vm.wasm",
                "target/wasm32-unknown-unknown/debug/a_rust_vm.wasm"
            ]
        );
    }
}
