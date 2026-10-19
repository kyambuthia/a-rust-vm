//! `arvm session`: saved agent transcripts.

use a_rust_vm::session::{Session, SessionStore};
use a_rust_vm::task_store::{TaskStore, TaskStoreError};

use super::paths::{session_directory, task_directory};

fn session_usage() {
    eprintln!("usage: arvm session list|show <id>|save <id> <role> <content>|export <id> [--json]");
}

fn format_session_text(session: &Session) -> String {
    if session.messages.is_empty() {
        return format!("(empty session {})", session.id);
    }
    session
        .messages
        .iter()
        .map(|message| format!("[{}] {}", message.role, message.content))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_session_json(session: &Session) -> Result<String, String> {
    serde_json::to_string_pretty(session)
        .map_err(|error| format!("cannot encode session '{}': {error}", session.id))
}

fn export_session(store: &SessionStore, id: &str, json: bool) -> Result<String, String> {
    let session = store.load(id).map_err(|error| error.to_string())?;
    if json {
        format_session_json(&session)
    } else {
        Ok(format_session_text(&session))
    }
}

fn parse_session_export_args(arguments: &[String]) -> Result<(String, bool), String> {
    match arguments {
        [id] => Ok((id.clone(), false)),
        [id, flag] if flag == "--json" => Ok((id.clone(), true)),
        _ => Err("usage: arvm session export <id> [--json]".to_owned()),
    }
}

pub(crate) fn run_session_command(arguments: &[String]) {
    let store = SessionStore::new(session_directory());
    let Some(command) = arguments.first().map(String::as_str) else {
        session_usage();
        std::process::exit(2);
    };
    match command {
        "list" => match store.list() {
            Ok(ids) => {
                if ids.is_empty() {
                    println!("(no sessions)");
                } else {
                    for id in ids {
                        println!("{id}");
                    }
                }
            }
            Err(error) => {
                eprintln!("[session] cannot list: {error}");
                std::process::exit(1);
            }
        },
        "show" => {
            let Some(id) = arguments.get(1) else {
                session_usage();
                std::process::exit(2);
            };
            match store.load(id) {
                Ok(session) => {
                    if session.messages.is_empty() {
                        println!("(empty session {id})");
                    }
                    for message in &session.messages {
                        println!("[{}] {}", message.role, message.content);
                    }
                }
                Err(error) => {
                    eprintln!("[session] {error}");
                    std::process::exit(1);
                }
            }
        }
        "save" => {
            if arguments.len() < 4 {
                session_usage();
                std::process::exit(2);
            }
            let id = &arguments[1];
            let role = &arguments[2];
            let content = &arguments[3];
            let task_store = TaskStore::new(task_directory());
            let _lease = task_store.lease(id).unwrap_or_else(|error| {
                eprintln!("[session] {error}");
                std::process::exit(2);
            });
            match task_store.load(id) {
                Ok(_) => {
                    eprintln!("[session] session '{id}' belongs to a task");
                    std::process::exit(2);
                }
                Err(TaskStoreError::NotFound { .. }) => {}
                Err(error) => {
                    eprintln!("[session] {error}");
                    std::process::exit(2);
                }
            }
            let mut session = match store.load(id) {
                Ok(existing) => existing,
                Err(error) if error.is_not_found() => match Session::new(id.clone()) {
                    Ok(created) => created,
                    Err(error) => {
                        eprintln!("[session] {error}");
                        std::process::exit(1);
                    }
                },
                Err(error) => {
                    eprintln!("[session] cannot resume {id}: {error}");
                    std::process::exit(1);
                }
            };
            session.push(role.clone(), content.clone());
            if let Err(error) = store.save(&session) {
                eprintln!("[session] cannot save: {error}");
                std::process::exit(1);
            }
            println!("saved {id} ({} messages)", session.messages.len());
        }
        "export" => {
            let (id, json) = match parse_session_export_args(&arguments[1..]) {
                Ok(parsed) => parsed,
                Err(error) => {
                    eprintln!("[session] {error}");
                    std::process::exit(2);
                }
            };
            match export_session(&store, &id, json) {
                Ok(output) => println!("{output}"),
                Err(error) => {
                    eprintln!("[session] {error}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            session_usage();
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_export_formats_text_json_and_usage() {
        let mut session = a_rust_vm::session::Session::new("export-1").unwrap();
        session.push("user", "inspect this workspace");
        session.push("assistant", "I will inspect it.");

        assert_eq!(
            format_session_text(&session),
            "[user] inspect this workspace\n[assistant] I will inspect it."
        );
        let json = format_session_json(&session).unwrap();
        let decoded: a_rust_vm::session::Session = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, session);

        let empty = a_rust_vm::session::Session::new("empty-export").unwrap();
        assert_eq!(format_session_text(&empty), "(empty session empty-export)");

        let directory = std::env::temp_dir().join(format!(
            "a-rvm-session-export-{}-{}.json",
            std::process::id(),
            "helpers"
        ));
        let directory = directory.with_extension("");
        let _ = std::fs::remove_dir_all(&directory);
        let store = a_rust_vm::session::SessionStore::new(&directory);
        store.save(&session).unwrap();
        assert_eq!(
            export_session(&store, "export-1", false).unwrap(),
            format_session_text(&session)
        );
        assert_eq!(
            export_session(&store, "export-1", true).unwrap(),
            format_session_json(&session).unwrap()
        );
        assert!(export_session(&store, "missing", false).is_err());
        let _ = std::fs::remove_dir_all(&directory);

        assert_eq!(
            parse_session_export_args(&["export-1".to_owned()]).unwrap(),
            ("export-1".to_owned(), false)
        );
        assert_eq!(
            parse_session_export_args(&["export-1".to_owned(), "--json".to_owned()]).unwrap(),
            ("export-1".to_owned(), true)
        );
        assert!(parse_session_export_args(&[]).is_err());
        assert!(parse_session_export_args(&["a".to_owned(), "b".to_owned()]).is_err());
        assert!(parse_session_export_args(&["a".to_owned(), "--text".to_owned()]).is_err());
    }
}
