//! Where the CLI keeps state and user configuration on this machine.

use std::env;

pub(crate) fn control_plane_path() -> std::path::PathBuf {
    if let Some(directory) = env::var_os("A_RVM_STATE_DIR") {
        return std::path::PathBuf::from(directory).join("control-plane.json");
    }
    if let Some(directory) = env::var_os("XDG_STATE_HOME") {
        return std::path::PathBuf::from(directory).join("a-rust-vm/control-plane.json");
    }
    env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".local/state/a-rust-vm/control-plane.json")
}

pub(crate) fn session_directory() -> std::path::PathBuf {
    if let Some(directory) = env::var_os("A_RVM_STATE_DIR") {
        return std::path::PathBuf::from(directory).join("sessions");
    }
    if let Some(directory) = env::var_os("XDG_STATE_HOME") {
        return std::path::PathBuf::from(directory).join("a-rust-vm/sessions");
    }
    env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".local/state/a-rust-vm/sessions")
}

pub(crate) fn permission_rules_path() -> std::path::PathBuf {
    if let Some(directory) = env::var_os("A_RVM_STATE_DIR") {
        return std::path::PathBuf::from(directory).join("rules.json");
    }
    if let Some(directory) = env::var_os("XDG_STATE_HOME") {
        return std::path::PathBuf::from(directory).join("a-rust-vm/rules.json");
    }
    env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".local/state/a-rust-vm/rules.json")
}

pub(crate) fn user_config_root() -> std::path::PathBuf {
    env::var_os("A_RVM_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from))
        .or_else(|| {
            env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|home| home.join(".config/a-rust-vm"))
        })
        .unwrap_or_else(std::env::temp_dir)
}

pub(crate) fn working_directory() -> std::path::PathBuf {
    env::current_dir().unwrap_or_else(|error| {
        eprintln!("failed to resolve the working directory: {error}");
        std::process::exit(2);
    })
}
