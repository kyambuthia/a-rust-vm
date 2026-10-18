//! The environment and process lifetime of commands the agent starts.
//!
//! A command started for the model must not see the host's secrets (provider
//! keys, cloud credentials, tokens), so it gets a short allow list of
//! variables instead of the full environment. The user configuration can
//! name more with `envPassthrough`.
//!
//! Each command also runs in its own process group, so a timeout or the end
//! of the command kills everything it started, including `cmd &` children.

use std::ffi::OsString;
use std::process::Command;

/// Variables every command receives when they are set: the basics a shell
/// and common toolchains need, and nothing that usually holds a secret.
pub const DEFAULT_ENV_PASSTHROUGH: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "TERM",
    "COLORTERM",
    "TMPDIR",
    "TZ",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "GOPATH",
    "GOROOT",
    "JAVA_HOME",
    "NVM_DIR",
    "PYENV_ROOT",
    "VIRTUAL_ENV",
];

/// Whether `name` may reach a command: a default name, any `LC_*` locale
/// variable, or a name the user listed.
pub fn is_passed_through(name: &str, extra: &[String]) -> bool {
    DEFAULT_ENV_PASSTHROUGH.contains(&name)
        || name.starts_with("LC_")
        || extra.iter().any(|allowed| allowed == name)
}

/// Filter `variables` down to the ones a command may see.
pub fn scrubbed_environment(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
    extra: &[String],
) -> Vec<(OsString, OsString)> {
    variables
        .into_iter()
        .filter(|(name, _)| {
            name.to_str()
                .is_some_and(|name| is_passed_through(name, extra))
        })
        .collect()
}

/// Replace `command`'s environment with the scrubbed host environment.
pub fn apply_scrubbed_environment(command: &mut Command, extra: &[String]) {
    command.env_clear();
    command.envs(scrubbed_environment(std::env::vars_os(), extra));
}

/// Whether `name` is a valid environment variable name for configuration.
pub fn valid_env_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Start `command` as the leader of a new process group.
#[cfg(unix)]
pub fn start_new_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
pub fn start_new_process_group(_command: &mut Command) {}

/// Kill every process in the group led by `leader`. Errors are ignored: the
/// group may already be gone.
#[cfg(unix)]
pub fn kill_process_group(leader: u32) {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    const SIGKILL: i32 = 9;
    let Ok(group) = i32::try_from(leader) else {
        return;
    };
    if group <= 1 {
        return;
    }
    // SAFETY: `kill` has no memory-safety preconditions. A negative pid
    // targets the process group `group`, which this process created with
    // `process_group(0)`, so the signal cannot reach unrelated processes.
    unsafe {
        kill(-group, SIGKILL);
    }
}

#[cfg(not(unix))]
pub fn kill_process_group(_leader: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(names: &[&str]) -> Vec<(OsString, OsString)> {
        names
            .iter()
            .map(|name| (OsString::from(name), OsString::from("value")))
            .collect()
    }

    fn names(variables: Vec<(OsString, OsString)>) -> Vec<String> {
        variables
            .into_iter()
            .map(|(name, _)| name.into_string().unwrap())
            .collect()
    }

    #[test]
    fn keeps_basics_and_drops_secrets() {
        let host = pairs(&[
            "PATH",
            "HOME",
            "LC_ALL",
            "OPENROUTER_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_PROFILE",
            "GITHUB_TOKEN",
            "A_RVM_MODEL_PROGRAM",
            "SSH_AUTH_SOCK",
        ]);
        assert_eq!(
            names(scrubbed_environment(host.clone(), &[])),
            ["PATH", "HOME", "LC_ALL"]
        );
        assert_eq!(
            names(scrubbed_environment(host, &["GITHUB_TOKEN".to_owned()])),
            ["PATH", "HOME", "LC_ALL", "GITHUB_TOKEN"]
        );
    }

    #[test]
    fn validates_env_names() {
        assert!(valid_env_name("GITHUB_TOKEN"));
        assert!(valid_env_name("_x1"));
        assert!(!valid_env_name(""));
        assert!(!valid_env_name("1ABC"));
        assert!(!valid_env_name("A-B"));
        assert!(!valid_env_name("A=B"));
    }
}
