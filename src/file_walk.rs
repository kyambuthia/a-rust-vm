//! Deterministic workspace walks for the search and find tools: glob
//! matching, a practical subset of `.gitignore`, and sorted traversal that
//! never follows symlinks.

use std::fs;
use std::path::{Path, PathBuf};

/// Directories never walked: version control, build output, dependency
/// caches, and A/RVM's own state.
const SKIPPED_DIRECTORIES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".arvm",
    "target",
    "node_modules",
    "__pycache__",
    ".venv",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
];

pub fn is_skipped_directory(name: &str) -> bool {
    SKIPPED_DIRECTORIES.contains(&name)
}

/// Match `text` against a glob: `*` and `?` stay within one path
/// component, `**` spans components, and everything else is literal.
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    fn matches(pattern: &[u8], text: &[u8]) -> bool {
        match pattern {
            [] => text.is_empty(),
            [b'*', b'*', rest @ ..] => {
                // `**/` also matches zero directories.
                let rest_after_slash = rest.strip_prefix(b"/").unwrap_or(rest);
                if matches(rest_after_slash, text) {
                    return true;
                }
                (0..text.len()).any(|skip| matches(rest, &text[skip + 1..])) || matches(rest, text)
            }
            [b'*', rest @ ..] => {
                let mut index = 0;
                loop {
                    if matches(rest, &text[index..]) {
                        return true;
                    }
                    if index == text.len() || text[index] == b'/' {
                        return false;
                    }
                    index += 1;
                }
            }
            [b'?', rest @ ..] => !text.is_empty() && text[0] != b'/' && matches(rest, &text[1..]),
            [first, rest @ ..] => text.first() == Some(first) && matches(rest, &text[1..]),
        }
    }
    matches(pattern.as_bytes(), text.as_bytes())
}

#[derive(Debug, Clone)]
struct IgnorePattern {
    glob: String,
    negated: bool,
    directory_only: bool,
    /// Anchored patterns match the path from the root; others match the
    /// entry's name at any depth.
    anchored: bool,
}

/// Rules from the workspace root's `.gitignore`. Supported: comments,
/// `!` negation, trailing `/` for directories, leading or inner `/` to
/// anchor at the root, and `*`, `?`, `**`. Character classes are literal.
#[derive(Debug, Clone, Default)]
pub struct IgnoreRules {
    patterns: Vec<IgnorePattern>,
}

impl IgnoreRules {
    pub fn load(root: &Path) -> Self {
        fs::read_to_string(root.join(".gitignore"))
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub fn parse(text: &str) -> Self {
        let patterns = text
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| {
                let (negated, line) = match line.strip_prefix('!') {
                    Some(rest) => (true, rest),
                    None => (false, line),
                };
                let (directory_only, line) = match line.strip_suffix('/') {
                    Some(rest) => (true, rest),
                    None => (false, line),
                };
                let anchored = line.contains('/');
                let glob = line.trim_start_matches('/').to_owned();
                (!glob.is_empty()).then_some(IgnorePattern {
                    glob,
                    negated,
                    directory_only,
                    anchored,
                })
            })
            .collect();
        Self { patterns }
    }

    /// Whether a path (relative to the root, `/`-separated) is ignored. As
    /// in git, the last matching pattern decides.
    pub fn is_ignored(&self, relative: &str, is_directory: bool) -> bool {
        let name = relative.rsplit('/').next().unwrap_or(relative);
        let mut ignored = false;
        for pattern in &self.patterns {
            if pattern.directory_only && !is_directory {
                continue;
            }
            let subject = if pattern.anchored { relative } else { name };
            if glob_matches(&pattern.glob, subject) {
                ignored = !pattern.negated;
            }
        }
        ignored
    }
}

/// One file found by [`walk_files`].
pub struct WalkedFile {
    pub path: PathBuf,
    /// Path relative to the walk root, `/`-separated.
    pub relative: String,
}

/// Visit regular files below `start` in sorted order, skipping symlinks,
/// [`SKIPPED_DIRECTORIES`], and ignored paths. `visit` returns `false` to
/// stop. `root` is the directory ignore rules and relative paths refer to.
pub fn walk_files(
    root: &Path,
    start: &Path,
    rules: &IgnoreRules,
    visit: &mut dyn FnMut(WalkedFile) -> bool,
) -> std::io::Result<()> {
    fn walk(
        root: &Path,
        directory: &Path,
        rules: &IgnoreRules,
        visit: &mut dyn FnMut(WalkedFile) -> bool,
    ) -> std::io::Result<bool> {
        let mut entries = fs::read_dir(directory)?
            .filter_map(Result::ok)
            .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry)))
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        for (name, entry) in entries {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if file_type.is_dir() {
                if is_skipped_directory(&name) || rules.is_ignored(&relative, true) {
                    continue;
                }
                if !walk(root, &path, rules, visit)? {
                    return Ok(false);
                }
            } else if file_type.is_file()
                && !rules.is_ignored(&relative, false)
                && !visit(WalkedFile { path, relative })
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
    walk(root, start, rules, visit).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_keep_single_stars_within_a_component() {
        assert!(glob_matches("*.rs", "main.rs"));
        assert!(!glob_matches("*.rs", "src/main.rs"));
        assert!(glob_matches("src/*.rs", "src/main.rs"));
        assert!(!glob_matches("src/*.rs", "src/cli/main.rs"));
        assert!(glob_matches("src/**/*.rs", "src/main.rs"));
        assert!(glob_matches("src/**/*.rs", "src/cli/deep/main.rs"));
        assert!(glob_matches("**/test_*.py", "a/b/test_x.py"));
        assert!(glob_matches("**", "anything/at/all"));
        assert!(glob_matches("a?c", "abc"));
        assert!(!glob_matches("a?c", "a/c"));
        assert!(!glob_matches("*.rs", "main.rsx"));
    }

    #[test]
    fn ignore_rules_follow_git_order_anchoring_and_directories() {
        let rules = IgnoreRules::parse("# build\n*.log\n!keep.log\n/dist\nbuild/\ndocs/*.tmp\n\n");
        assert!(rules.is_ignored("a/b/debug.log", false));
        assert!(!rules.is_ignored("a/keep.log", false));
        assert!(rules.is_ignored("dist", true));
        assert!(!rules.is_ignored("src/dist", true));
        assert!(rules.is_ignored("x/build", true));
        assert!(!rules.is_ignored("x/build", false));
        assert!(rules.is_ignored("docs/a.tmp", false));
        assert!(!rules.is_ignored("other/docs/a.tmp", false));
        assert!(!rules.is_ignored("src/main.rs", false));
    }

    #[test]
    fn walks_are_sorted_and_skip_ignored_and_cache_directories() {
        let root = std::env::temp_dir().join(format!("a-rvm-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for file in [
            "b.txt",
            "a.txt",
            "src/z.rs",
            "src/a.rs",
            "node_modules/x.js",
            "out/y",
        ] {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "x").unwrap();
        }
        fs::write(root.join(".gitignore"), "out/\n").unwrap();
        let rules = IgnoreRules::load(&root);
        let mut seen = Vec::new();
        walk_files(&root, &root, &rules, &mut |file| {
            seen.push(file.relative);
            true
        })
        .unwrap();
        assert_eq!(
            seen,
            [".gitignore", "a.txt", "b.txt", "src/a.rs", "src/z.rs"]
        );

        let mut first = Vec::new();
        walk_files(&root, &root, &rules, &mut |file| {
            first.push(file.relative);
            first.len() < 2
        })
        .unwrap();
        assert_eq!(first.len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }
}
