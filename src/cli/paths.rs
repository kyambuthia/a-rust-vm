//! Where the CLI keeps state and user configuration on this machine.

use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

const APP_DIRECTORY: &str = "a-rust-vm";

pub(crate) fn control_plane_path() -> PathBuf {
    state_directory().join("control-plane.json")
}

pub(crate) fn session_directory() -> PathBuf {
    state_directory().join("sessions")
}

pub(crate) fn permission_rules_path() -> PathBuf {
    state_directory().join("rules.json")
}

pub(crate) fn mcp_trust_path() -> PathBuf {
    state_directory().join("mcp-trust.json")
}

pub(crate) fn user_config_root() -> PathBuf {
    config_directory_with(|name| env::var_os(name))
}

pub(crate) fn working_directory() -> PathBuf {
    env::current_dir().unwrap_or_else(|error| {
        eprintln!("failed to resolve the working directory: {error}");
        std::process::exit(2);
    })
}

fn state_directory() -> PathBuf {
    state_directory_with(|name| env::var_os(name))
}

/// `A_RVM_STATE_DIR` is used as given; otherwise the XDG state home (or
/// `~/.local/state`) gets an `a-rust-vm` subdirectory.
fn state_directory_with(var: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    app_directory_with(&var, "A_RVM_STATE_DIR", "XDG_STATE_HOME", ".local/state")
}

/// `A_RVM_CONFIG_DIR` is used as given; otherwise the XDG config home (or
/// `~/.config`) gets an `a-rust-vm` subdirectory.
fn config_directory_with(var: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    app_directory_with(&var, "A_RVM_CONFIG_DIR", "XDG_CONFIG_HOME", ".config")
}

fn app_directory_with(
    var: &impl Fn(&str) -> Option<OsString>,
    override_name: &str,
    xdg_name: &str,
    home_relative: &str,
) -> PathBuf {
    let set = |name: &str| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if let Some(directory) = set(override_name) {
        return directory;
    }
    // The XDG base directory spec says relative values are invalid and must be ignored.
    if let Some(base) = set(xdg_name).filter(|path| path.is_absolute()) {
        return base.join(APP_DIRECTORY);
    }
    set("HOME")
        .unwrap_or_else(env::temp_dir)
        .join(home_relative)
        .join(APP_DIRECTORY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), OsString::from(value)))
            .collect::<Vec<_>>();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn xdg_config_home_gets_the_app_subdirectory() {
        let var = environment(&[("XDG_CONFIG_HOME", "/xdg/config"), ("HOME", "/home/u")]);
        assert_eq!(
            config_directory_with(var),
            PathBuf::from("/xdg/config/a-rust-vm")
        );
    }

    #[test]
    fn explicit_overrides_are_used_as_given() {
        let var = environment(&[
            ("A_RVM_CONFIG_DIR", "/custom/config"),
            ("A_RVM_STATE_DIR", "/custom/state"),
            ("XDG_CONFIG_HOME", "/xdg/config"),
            ("XDG_STATE_HOME", "/xdg/state"),
        ]);
        assert_eq!(config_directory_with(&var), PathBuf::from("/custom/config"));
        assert_eq!(state_directory_with(&var), PathBuf::from("/custom/state"));
    }

    #[test]
    fn home_fallbacks_match_the_xdg_defaults() {
        let var = environment(&[("HOME", "/home/u")]);
        assert_eq!(
            config_directory_with(&var),
            PathBuf::from("/home/u/.config/a-rust-vm")
        );
        assert_eq!(
            state_directory_with(&var),
            PathBuf::from("/home/u/.local/state/a-rust-vm")
        );
        let var = environment(&[("XDG_STATE_HOME", "/xdg/state"), ("HOME", "/home/u")]);
        assert_eq!(
            state_directory_with(var),
            PathBuf::from("/xdg/state/a-rust-vm")
        );
    }

    #[test]
    fn empty_and_relative_values_are_ignored() {
        let var = environment(&[
            ("A_RVM_STATE_DIR", ""),
            ("XDG_STATE_HOME", "relative/state"),
            ("XDG_CONFIG_HOME", ""),
            ("HOME", "/home/u"),
        ]);
        assert_eq!(
            state_directory_with(&var),
            PathBuf::from("/home/u/.local/state/a-rust-vm")
        );
        assert_eq!(
            config_directory_with(&var),
            PathBuf::from("/home/u/.config/a-rust-vm")
        );
    }

    #[test]
    fn missing_home_falls_back_to_a_namespaced_temp_directory() {
        let var = environment(&[]);
        assert_eq!(
            config_directory_with(&var),
            env::temp_dir().join(".config").join("a-rust-vm")
        );
    }
}
