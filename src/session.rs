//! Private local session persistence for prompts and agent responses.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// One message retained in a local session transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String,
    pub content: String,
}

/// The session format written by this version. Version 1 files (before
/// recordings) have no `schema` field and load with an empty recording.
pub const SESSION_SCHEMA: u32 = 2;

fn legacy_schema() -> u32 {
    1
}

/// Everything that happened in one turn, in order: the prompt, each model
/// answer, tool call, tool result, permission request, and usage figure.
/// Streamed text deltas are merged into one `assistant_text` per step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRecord {
    pub prompt: String,
    pub started_at: u64,
    pub events: Vec<crate::agent::AgentEvent>,
}

impl TurnRecord {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            started_at: unix_timestamp(),
            events: Vec::new(),
        }
    }

    /// Record one event, folding consecutive text deltas together.
    pub fn record(&mut self, event: &crate::agent::AgentEvent) {
        use crate::agent::AgentEvent;
        match event {
            AgentEvent::AssistantDelta { content } => match self.events.last_mut() {
                Some(AgentEvent::AssistantText { content: text }) => text.push_str(content),
                _ => self.events.push(AgentEvent::AssistantText {
                    content: content.clone(),
                }),
            },
            other => self.events.push(other.clone()),
        }
    }
}

/// A resumable local conversation record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default = "legacy_schema")]
    pub schema: u32,
    pub id: String,
    pub created_at: u64,
    pub updated_at: u64,
    /// Human-readable transcript for `session show` and `export`.
    pub messages: Vec<SessionMessage>,
    /// The exact model context after the last turn, including tool calls and
    /// their ids, so a resumed session continues where it stopped.
    #[serde(default)]
    pub conversation: Vec<crate::agent::ConversationMessage>,
    /// One record per turn.
    #[serde(default)]
    pub turns: Vec<TurnRecord>,
}

impl Session {
    pub fn new(id: impl Into<String>) -> Result<Self, SessionError> {
        let id = id.into();
        validate_id(&id)?;
        let now = unix_timestamp();
        Ok(Self {
            schema: SESSION_SCHEMA,
            id,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
            conversation: Vec::new(),
            turns: Vec::new(),
        })
    }

    /// The model context to resume from. Version 1 files only kept text, so
    /// their tool results come back as user notes: a `tool` message without
    /// the id of its call is rejected by OpenAI-style providers.
    pub fn resume_conversation(&self) -> Vec<crate::agent::ConversationMessage> {
        if !self.conversation.is_empty() {
            return self.conversation.clone();
        }
        self.messages
            .iter()
            .map(|message| match message.role.as_str() {
                "tool" => crate::agent::ConversationMessage::new(
                    "user",
                    format!("[earlier tool result] {}", message.content),
                ),
                role => crate::agent::ConversationMessage::new(role, &message.content),
            })
            .collect()
    }

    /// Store a finished turn and the model context that followed it.
    pub fn finish_turn(
        &mut self,
        record: TurnRecord,
        conversation: &[crate::agent::ConversationMessage],
    ) {
        self.schema = SESSION_SCHEMA;
        self.turns.push(record);
        self.conversation = conversation.to_vec();
        self.updated_at = unix_timestamp();
    }

    pub fn push(&mut self, role: impl Into<String>, content: impl Into<String>) {
        self.messages.push(SessionMessage {
            role: role.into(),
            content: content.into(),
        });
        self.updated_at = unix_timestamp();
    }
}

/// Errors from local session storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError {
    pub message: String,
    kind: SessionErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionErrorKind {
    Generic,
    NotFound,
}

impl SessionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: SessionErrorKind::Generic,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: SessionErrorKind::NotFound,
        }
    }

    pub fn is_not_found(&self) -> bool {
        self.kind == SessionErrorKind::NotFound
    }
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SessionError {}

/// A filesystem-backed session store rooted at a caller-selected private path.
#[derive(Debug, Clone)]
pub struct SessionStore {
    directory: PathBuf,
}

impl SessionStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn save(&self, session: &Session) -> Result<(), SessionError> {
        validate_id(&session.id)?;
        let content = serde_json::to_vec_pretty(session)
            .map_err(|error| SessionError::new(format!("cannot encode session: {error}")))?;
        crate::private_fs::write_private(&self.path_for(&session.id), &content).map_err(|error| {
            SessionError::new(format!("cannot save session '{}': {error}", session.id))
        })
    }

    pub fn load(&self, id: &str) -> Result<Session, SessionError> {
        validate_id(id)?;
        let path = self.path_for(id);
        let bytes = fs::read(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SessionError::not_found(format!("session '{id}' does not exist"))
            } else {
                SessionError::new(format!("cannot read session '{id}': {error}"))
            }
        })?;
        let session: Session = serde_json::from_slice(&bytes)
            .map_err(|error| SessionError::new(format!("cannot decode session '{id}': {error}")))?;
        // Saving a newer file with this version would drop fields it cannot
        // read, so refuse it instead of silently losing data.
        if session.schema > SESSION_SCHEMA {
            return Err(SessionError::new(format!(
                "session '{id}' uses format {} but this arvm reads up to {SESSION_SCHEMA}; upgrade arvm",
                session.schema
            )));
        }
        Ok(session)
    }

    pub fn list(&self) -> Result<Vec<String>, SessionError> {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return Ok(Vec::new());
        };
        let mut ids = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                    return None;
                }
                path.file_stem()?.to_str().map(str::to_owned)
            })
            .filter(|id| validate_id(id).is_ok())
            .collect::<Vec<_>>();
        ids.sort();
        Ok(ids)
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }
}

fn validate_id(id: &str) -> Result<(), SessionError> {
    if id.is_empty()
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(SessionError::new(
            "session id must contain only ASCII letters, numbers, '-' or '_'",
        ));
    }
    Ok(())
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{SESSION_SCHEMA, Session, SessionStore, TurnRecord};

    #[test]
    fn version_one_files_load_and_resume_without_orphan_tool_messages() {
        let legacy = r#"{"id":"s-1","created_at":1,"updated_at":2,"messages":[
            {"role":"user","content":"read a"},
            {"role":"tool","content":"read_file: hello"},
            {"role":"assistant","content":"it says hello"}]}"#;
        let session: Session = serde_json::from_str(legacy).unwrap();
        assert_eq!(session.schema, 1);
        assert!(session.turns.is_empty());
        let resumed = session.resume_conversation();
        assert_eq!(resumed[1].role, "user");
        assert!(resumed[1].content.contains("read_file: hello"));
        assert!(resumed.iter().all(|message| message.role != "tool"));
    }

    #[test]
    fn turn_records_fold_deltas_and_round_trip() {
        use crate::agent::{AgentEvent, ConversationMessage, Usage};
        let mut record = TurnRecord::new("hi");
        for event in [
            AgentEvent::UserMessage {
                content: "hi".to_owned(),
            },
            AgentEvent::AssistantDelta {
                content: "Hel".to_owned(),
            },
            AgentEvent::AssistantDelta {
                content: "lo".to_owned(),
            },
            AgentEvent::Usage(Usage {
                input_tokens: 10,
                ..Default::default()
            }),
            AgentEvent::Done,
        ] {
            record.record(&event);
        }
        assert_eq!(
            record.events[1],
            AgentEvent::AssistantText {
                content: "Hello".to_owned()
            }
        );
        assert_eq!(record.events.len(), 4);

        let mut session = Session::new("s-2").unwrap();
        let context = vec![ConversationMessage::new("user", "hi")];
        session.finish_turn(record, &context);
        let json = serde_json::to_string(&session).unwrap();
        let decoded: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, session);
        assert_eq!(decoded.schema, SESSION_SCHEMA);
        assert_eq!(decoded.resume_conversation(), context);
    }

    #[test]
    fn newer_session_formats_are_refused() {
        let directory = test_directory("newer");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("future.json"),
            r#"{"schema":99,"id":"future","created_at":1,"updated_at":1,"messages":[]}"#,
        )
        .unwrap();
        let error = SessionStore::new(&directory).load("future").unwrap_err();
        assert!(error.message.contains("upgrade arvm"), "{error}");
        let _ = std::fs::remove_dir_all(&directory);
    }

    fn test_directory(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("a-rust-vm-session-{label}-{}", std::process::id()))
    }

    #[test]
    fn sessions_round_trip_and_list() {
        let directory = test_directory("round-trip");
        let store = SessionStore::new(&directory);
        let mut session = Session::new("session-1").unwrap();
        session.push("user", "inspect this workspace");
        session.push("assistant", "I will inspect it.");

        store.save(&session).unwrap();

        assert_eq!(store.list().unwrap(), vec!["session-1"]);
        assert_eq!(store.load("session-1").unwrap(), session);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_session_ids_cannot_escape_the_store() {
        assert!(Session::new("../outside").is_err());
        assert!(Session::new("with space").is_err());
    }

    #[test]
    fn missing_sessions_are_distinguished_from_storage_failures() {
        let directory = test_directory("missing");
        let store = SessionStore::new(&directory);

        let error = store.load("missing").unwrap_err();

        assert!(error.is_not_found());
    }
}
