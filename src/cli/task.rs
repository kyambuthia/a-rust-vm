//! `arvm task`: inspect replay-validated local task records.

use a_rust_vm::task::TaskRecord;
use a_rust_vm::task_store::TaskStore;

use super::paths::task_directory;

fn usage() {
    eprintln!("usage: arvm task list|show <id> [--json]");
}

fn show(task: &TaskRecord, json: bool) -> Result<String, String> {
    if json {
        return serde_json::to_string_pretty(task).map_err(|error| error.to_string());
    }
    let checkpoint = task
        .checkpoint()
        .map(|checkpoint| {
            format!(
                "session {} after {} turns",
                checkpoint.session_id, checkpoint.completed_turns
            )
        })
        .unwrap_or_else(|| "none".to_owned());
    Ok(format!(
        "id: {}\nstatus: {:?}\nowner: {}\nworkspace: {}\ncheckpoint: {}\nevents: {}",
        task.spec().id,
        task.status(),
        task.spec().owner,
        task.spec().workspace_id,
        checkpoint,
        task.events().len()
    ))
}

pub(crate) fn run_task_command(arguments: &[String]) {
    let store = TaskStore::new(task_directory());
    match arguments {
        [command] if command == "list" => match store.list() {
            Ok(ids) => {
                if ids.is_empty() {
                    println!("(no tasks)");
                } else {
                    for id in ids {
                        println!("{id}");
                    }
                }
            }
            Err(error) => {
                eprintln!("[task] cannot list: {error}");
                std::process::exit(1);
            }
        },
        [command, id] if command == "show" => print_task(&store, id, false),
        [command, id, flag] if command == "show" && flag == "--json" => {
            print_task(&store, id, true);
        }
        _ => {
            usage();
            std::process::exit(2);
        }
    }
}

fn print_task(store: &TaskStore, id: &str, json: bool) {
    let result = store
        .load(id)
        .map_err(|error| error.to_string())
        .and_then(|task| show(&task, json));
    match result {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("[task] {error}");
            std::process::exit(1);
        }
    }
}
