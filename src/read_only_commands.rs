//! Commands that only read the workspace and can run without an approval
//! prompt.
//!
//! The check is deliberately narrow. A command qualifies only when all of
//! these hold:
//!
//! - It is one simple command from a fixed list, with no shell control,
//!   quoting, globbing, or expansion characters.
//! - It uses no flag known to run programs or write files (`rg --pre`,
//!   `git diff --output`, `tail -f`, ...).
//! - Every argument that names an existing path resolves inside the
//!   workspace; absolute paths, `~`, and `..` never qualify.
//! - For `git`, the repository's own config sets nothing that can run a
//!   program, so an approved file write cannot turn a later `git status`
//!   into command execution.
//!
//! Anything else falls back to the normal approval prompt. Permission rules
//! are applied before this check, so `ask run_command` restores prompts.

use std::fs;
use std::path::Path;

/// Characters that make the shell do more than run one plain command.
const SHELL_SPECIAL: &[char] = &[
    ';', '&', '|', '\n', '\r', '`', '$', '<', '>', '(', ')', '\'', '"', '\\', '*', '?', '[', ']',
    '{', '}', '#', '!',
];

/// Read-only git subcommands.
const GIT_SUBCOMMANDS: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "blame",
    "ls-files",
    "rev-parse",
    "grep",
];

/// Flags (or flag prefixes) that let a git subcommand run a program or
/// write a file.
const GIT_BANNED_FLAGS: &[&str] = &[
    "--output",
    "--ext-diff",
    "--textconv",
    "-O",
    "--open-files-in-pager",
    "--exec",
    "--upload-pack",
    "--receive-pack",
];

/// Words in a repository config that can make git run a program.
const RISKY_GIT_CONFIG: &[&str] = &[
    "fsmonitor",
    "textconv",
    "external",
    "pager",
    "include",
    "alias",
    "command",
    "helper",
    "program",
    "editor",
    "hookspath",
    "askpass",
    "gpg",
    "filter",
    "!",
];

/// Whether `command` may run in `root` without asking.
pub fn is_read_only_command(command: &str, root: &Path) -> bool {
    if command.contains(SHELL_SPECIAL) {
        return false;
    }
    let tokens = command.split_whitespace().collect::<Vec<_>>();
    let Some((&program, arguments)) = tokens.split_first() else {
        return false;
    };
    let flags_allowed = match program {
        "pwd" => return arguments.is_empty(),
        "ls" | "cat" | "wc" | "stat" | "du" | "diff" => true,
        "head" | "tail" => !arguments.iter().any(|argument| {
            argument.starts_with("--follow") || short_flags_contain(argument, &['f', 'F'])
        }),
        // `-R` follows symlinks out of the workspace.
        "grep" => !arguments.iter().any(|argument| {
            argument.starts_with("--dereference-recursive") || short_flags_contain(argument, &['R'])
        }),
        // `--pre` runs a program per file, `-z` runs decompressors, and
        // `-L` follows symlinks.
        "rg" => !arguments.iter().any(|argument| {
            argument.starts_with("--pre")
                || argument.starts_with("--search-zip")
                || argument.starts_with("--follow")
                || short_flags_contain(argument, &['z', 'L'])
        }),
        "git" => {
            let Some((subcommand, rest)) = arguments.split_first() else {
                return false;
            };
            if !GIT_SUBCOMMANDS.contains(subcommand) {
                return false;
            }
            let banned = rest.iter().any(|argument| {
                GIT_BANNED_FLAGS
                    .iter()
                    .any(|flag| argument.starts_with(flag))
            });
            if banned || !git_config_is_plain(root) {
                return false;
            }
            return rest
                .iter()
                .all(|argument| argument_stays_inside(argument, root));
        }
        _ => return false,
    };
    flags_allowed
        && arguments
            .iter()
            .all(|argument| argument_stays_inside(argument, root))
}

/// Whether a short-flag cluster such as `-nf` contains one of `letters`.
fn short_flags_contain(argument: &str, letters: &[char]) -> bool {
    argument
        .strip_prefix('-')
        .filter(|flags| !flags.starts_with('-'))
        .is_some_and(|flags| flags.chars().any(|flag| letters.contains(&flag)))
}

/// An argument, or the value of a `--flag=value`, must not name a place
/// outside the workspace. Values that are not paths (patterns, revisions,
/// numbers) pass when, read as a relative path, they stay inside.
fn argument_stays_inside(argument: &str, root: &Path) -> bool {
    let value = if argument.starts_with('-') {
        match argument.split_once('=') {
            Some((_, value)) => value,
            None => return true,
        }
    } else {
        argument
    };
    if value.starts_with('/') || value.starts_with('~') {
        return false;
    }
    if value.split('/').any(|component| component == "..") {
        return false;
    }
    // Resolve the nearest existing ancestor: a missing name below a symlink
    // that leaves the workspace must not count as inside.
    let Ok(root) = fs::canonicalize(root) else {
        return false;
    };
    let candidate = root.join(value);
    let mut existing = candidate.as_path();
    while fs::symlink_metadata(existing).is_err() {
        match existing.parent() {
            Some(parent) => existing = parent,
            None => return false,
        }
    }
    fs::canonicalize(existing).is_ok_and(|resolved| resolved.starts_with(&root))
}

/// Whether the repository containing `root` has a config file that sets
/// nothing able to run a program. A `.git` file (worktree or submodule)
/// points elsewhere, so it does not count as plain.
fn git_config_is_plain(root: &Path) -> bool {
    let mut directory = Some(root);
    while let Some(current) = directory {
        let dot_git = current.join(".git");
        match fs::symlink_metadata(&dot_git) {
            Ok(metadata) if metadata.is_dir() => {
                return ["config", "config.worktree"].iter().all(|name| {
                    match fs::read_to_string(dot_git.join(name)) {
                        Ok(text) => {
                            let text = text.to_ascii_lowercase();
                            !RISKY_GIT_CONFIG.iter().any(|word| text.contains(word))
                        }
                        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
                    }
                });
            }
            Ok(_) => return false,
            Err(_) => directory = current.parent(),
        }
    }
    // No repository: git refuses to run these subcommands.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(label: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("a-rvm-readonly-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        root
    }

    #[test]
    fn plain_reads_inside_the_workspace_qualify() {
        let root = workspace("plain");
        for command in [
            "ls",
            "ls -la src",
            "pwd",
            "cat src/main.rs",
            "head -n 20 src/main.rs",
            "wc -l src/main.rs",
            "grep -rn main src",
            "rg -n fn src",
            "diff src/main.rs src/main.rs",
        ] {
            assert!(is_read_only_command(command, &root), "{command}");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn writes_escapes_and_shell_features_do_not_qualify() {
        let root = workspace("refused");
        let outside =
            std::env::temp_dir().join(format!("a-rvm-readonly-out-{}", std::process::id()));
        fs::create_dir_all(&outside).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        for command in [
            "",
            "rm src/main.rs",
            "cargo test",
            "cat /etc/passwd",
            "cat ~/.ssh/id_rsa",
            "cat ../secret",
            "cat src/../../x",
            "cat link/file",
            "ls; rm -rf src",
            "cat src/main.rs > out",
            "cat $HOME/x",
            "cat 'src/main.rs'",
            "cat src/*.rs",
            "tail -f log",
            "tail -nf log",
            "tail --follow=name log",
            "grep -R x .",
            "rg --pre cat x",
            "rg -z x",
            "rg --file=/etc/passwd",
            "pwd -P",
            "env",
            "find . -delete",
        ] {
            assert!(!is_read_only_command(command, &root), "{command:?}");
        }
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn git_reads_qualify_only_with_a_plain_repository_config() {
        let root = workspace("git");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(".git/config"),
            "[core]\n\trepositoryformatversion = 0\n\tbare = false\n\
             [remote \"origin\"]\n\turl = git@example.com:a/b.git\n",
        )
        .unwrap();
        for command in [
            "git status",
            "git status --short",
            "git diff",
            "git diff HEAD~1 -- src/main.rs",
            "git log --oneline -n 5",
            "git log --format=%h main..feature",
            "git show HEAD:src/main.rs",
        ] {
            assert!(is_read_only_command(command, &root), "{command}");
        }
        for command in [
            "git",
            "git commit -m x",
            "git checkout .",
            "git -c core.pager=sh status",
            "git diff --output=x",
            "git diff --ext-diff",
            "git log -p --textconv",
            "git grep -O x",
            "git diff --no-index /etc/passwd src/main.rs",
        ] {
            assert!(!is_read_only_command(command, &root), "{command:?}");
        }

        fs::write(root.join(".git/config"), "[core]\n\tfsmonitor = ./run-me\n").unwrap();
        assert!(!is_read_only_command("git status", &root));
        fs::write(root.join(".git/config"), "[core]\n\tbare = false\n").unwrap();
        fs::write(
            root.join(".git/config.worktree"),
            "[diff \"x\"]\n\ttextconv = sh\n",
        )
        .unwrap();
        assert!(!is_read_only_command("git diff", &root));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_git_file_pointing_elsewhere_is_not_plain() {
        let root = workspace("git-file");
        fs::write(root.join(".git"), "gitdir: /somewhere/else\n").unwrap();
        assert!(!is_read_only_command("git status", &root));
        fs::remove_dir_all(&root).unwrap();
    }
}
