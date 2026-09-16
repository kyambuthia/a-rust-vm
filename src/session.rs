//! Private local session persistence for prompts and agent responses.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

use serde::{Deserialize, Serialize};

/// One message retained in a local session transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String,
    pub content: String,
}

/// A resumable local conversation record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub messages: Vec<SessionMessage>,
}

impl Session {
    pub fn new(id: impl Into<String>) -> Result<Self, SessionError> {
        let id = id.into();
        validate_id(&id)?;
        let now = unix_timestamp();
        Ok(Self {
            id,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
        })
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
        fs::create_dir_all(&self.directory).map_err(|error| {
            SessionError::new(format!(
                "cannot create session directory '{}': {error}",
                self.directory.display()
            ))
        })?;
        let content = serde_json::to_vec_pretty(session)
            .map_err(|error| SessionError::new(format!("cannot encode session: {error}")))?;
        let target = self.path_for(&session.id);
        let temporary = self.directory.join(format!(
            ".{}.{}.{}.tmp",
            session.id,
            std::process::id(),
            NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&temporary, content).map_err(|error| {
            SessionError::new(format!("cannot write temporary session: {error}"))
        })?;
        fs::rename(&temporary, &target).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            SessionError::new(format!("cannot commit session '{}': {error}", session.id))
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
        serde_json::from_slice(&bytes)
            .map_err(|error| SessionError::new(format!("cannot decode session '{id}': {error}")))
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
    use super::{Session, SessionStore};

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
