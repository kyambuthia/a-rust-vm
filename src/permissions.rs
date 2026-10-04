use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionEffect {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionError {
    pub message: String,
}

impl PermissionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PermissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PermissionError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub effect: PermissionEffect,
    pub tool: String,
    pub target_prefix: String,
}

/// What a rule target names, which decides how its prefix is compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    /// A workspace path, compared by normalized path components.
    Path,
    /// A shell command line, compared by whole tokens.
    Command,
    /// Any other value, compared as a plain string prefix.
    Other,
}

/// Characters that let one command line run more than one command or
/// redirect its effects.
const SHELL_CONTROL: &[char] = &[';', '&', '|', '\n', '\r', '`', '$', '<', '>', '(', ')'];

impl PermissionRule {
    pub fn matches(&self, tool: &str, target: &str) -> bool {
        self.matches_target(tool, TargetKind::Other, target)
    }

    /// Match a typed target. Restrictive rules (`deny`, `ask`) match
    /// liberally and `allow` rules match strictly, so an unusual spelling
    /// of a target can only make the outcome more cautious.
    pub fn matches_target(&self, tool: &str, kind: TargetKind, target: &str) -> bool {
        if self.tool != "*" && self.tool != tool {
            return false;
        }
        if self.target_prefix.is_empty() {
            return true;
        }
        let restrictive = self.effect != PermissionEffect::Allow;
        match kind {
            TargetKind::Other => target.starts_with(&self.target_prefix),
            TargetKind::Path => path_matches(&self.target_prefix, target, restrictive),
            TargetKind::Command => command_matches(&self.target_prefix, target, restrictive),
        }
    }
}

/// Split a path into whether it is absolute and its meaningful components.
fn path_components(path: &str) -> (bool, Vec<&str>) {
    let components = path
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect();
    (path.starts_with('/'), components)
}

fn path_matches(prefix: &str, target: &str, restrictive: bool) -> bool {
    let (prefix_absolute, prefix) = path_components(prefix);
    let (target_absolute, target) = path_components(target);
    if !restrictive {
        // Allow rules cover whole components only: `src` is not `src_old`.
        return prefix_absolute == target_absolute && target.starts_with(&prefix);
    }
    // Restrictive rules keep plain string-prefix semantics on the normalized
    // path, so `deny *:secrets` also covers `secrets.txt`. A relative rule
    // cannot tell where an absolute target sits relative to the workspace
    // root, so it is tried at every component boundary of that target.
    let prefix = prefix.join("/");
    let starts = if prefix_absolute == target_absolute {
        0..1
    } else if !prefix_absolute {
        0..target.len()
    } else {
        return false;
    };
    starts
        .into_iter()
        .any(|start| target[start..].join("/").starts_with(&prefix))
}

fn command_matches(prefix: &str, target: &str, restrictive: bool) -> bool {
    let prefix = prefix.split_whitespace().collect::<Vec<_>>();
    let starts_with_prefix = |segment: &str| {
        let mut tokens = segment.split_whitespace().collect::<Vec<_>>();
        if restrictive && let Some(program) = tokens.first_mut() {
            // `/bin/rm` runs the same program as `rm`.
            *program = program.rsplit('/').next().unwrap_or(program);
        }
        tokens.starts_with(&prefix)
    };
    if restrictive {
        // Any command inside a compound line counts.
        target.split(SHELL_CONTROL).any(starts_with_prefix)
    } else {
        // Never auto-approve a line that can chain or redirect commands.
        !target.contains(SHELL_CONTROL) && starts_with_prefix(target)
    }
}

pub fn parse_permission_rule(text: &str) -> Result<PermissionRule, PermissionError> {
    let text = text.trim();
    let (effect, selector) = text.split_once(char::is_whitespace).ok_or_else(|| {
        PermissionError::new(
            "permission rule must be '<allow|ask|deny> <tool|*>[:<target-prefix>]'",
        )
    })?;
    let selector = selector.trim();
    let (tool, target_prefix) = match selector.split_once(':') {
        Some((tool, target)) => {
            let tool = tool.trim();
            let target = target.trim();
            if tool.is_empty() || tool.split_whitespace().count() != 1 {
                return Err(PermissionError::new(
                    "permission rule tool must be a single non-empty name",
                ));
            }
            if target.is_empty() || target.split_whitespace().count() != 1 {
                return Err(PermissionError::new(
                    "permission rule target prefix must be a single non-empty value",
                ));
            }
            (tool, target)
        }
        None => {
            if selector.is_empty() || selector.split_whitespace().count() != 1 {
                return Err(PermissionError::new(
                    "permission rule must be '<allow|ask|deny> <tool|*>[:<target-prefix>]'",
                ));
            }
            (selector, "")
        }
    };
    let effect = match effect {
        "allow" => PermissionEffect::Allow,
        "ask" => PermissionEffect::Ask,
        "deny" => PermissionEffect::Deny,
        _ => {
            return Err(PermissionError::new(
                "permission effect must be one of 'allow', 'ask', or 'deny'",
            ));
        }
    };
    Ok(PermissionRule {
        effect,
        tool: tool.to_owned(),
        target_prefix: target_prefix.to_owned(),
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionPolicy {
    rules: Vec<PermissionRule>,
}

impl PermissionPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_rules(rules: Vec<PermissionRule>) -> Self {
        Self { rules }
    }

    pub fn add_rule(&mut self, rule: PermissionRule) {
        self.rules.push(rule);
    }

    pub fn rules(&self) -> &[PermissionRule] {
        &self.rules
    }

    pub fn decide(&self, tool: &str, target: &str) -> PermissionEffect {
        self.decide_target(tool, TargetKind::Other, target)
    }

    /// Return the effect of the first rule matching a typed target, or
    /// `Ask` when none matches.
    pub fn decide_target(&self, tool: &str, kind: TargetKind, target: &str) -> PermissionEffect {
        self.rules
            .iter()
            .find(|rule| rule.matches_target(tool, kind, target))
            .map(|rule| rule.effect)
            .unwrap_or(PermissionEffect::Ask)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionApprovals {
    allowed: BTreeSet<(String, String)>,
}

impl SessionApprovals {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn remember(&mut self, tool: &str, target: &str) {
        self.allowed.insert((tool.to_owned(), target.to_owned()));
    }

    pub fn is_allowed(&self, tool: &str, target: &str) -> bool {
        self.allowed.contains(&(tool.to_owned(), target.to_owned()))
    }

    pub fn clear(&mut self) {
        self.allowed.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PermissionEffect, PermissionPolicy, PermissionRule, SessionApprovals, TargetKind,
        parse_permission_rule,
    };

    fn rule(text: &str) -> PermissionRule {
        parse_permission_rule(text).expect("rule should parse")
    }

    #[test]
    fn parses_allow_ask_and_deny_rules() {
        assert_eq!(
            rule("allow write_file:notes.txt"),
            PermissionRule {
                effect: PermissionEffect::Allow,
                tool: "write_file".to_owned(),
                target_prefix: "notes.txt".to_owned(),
            }
        );
        assert_eq!(rule("ask *").tool, "*");
        assert_eq!(rule("deny run_command").effect, PermissionEffect::Deny);
    }

    #[test]
    fn rejects_malformed_rules() {
        assert!(parse_permission_rule("").is_err());
        assert!(parse_permission_rule("allow").is_err());
        assert!(parse_permission_rule("permit write_file").is_err());
        assert!(parse_permission_rule("allow :notes.txt").is_err());
        assert!(parse_permission_rule("allow write_file:").is_err());
        assert!(parse_permission_rule("allow write_file notes.txt").is_err());
    }

    #[test]
    fn decide_uses_first_match_and_defaults_to_ask() {
        let policy = PermissionPolicy::with_rules(vec![
            rule("deny run_command:rm"),
            rule("allow run_command"),
        ]);
        assert_eq!(
            policy.decide("run_command", "rm -rf /tmp"),
            PermissionEffect::Deny
        );
        assert_eq!(
            policy.decide("run_command", "printf hello"),
            PermissionEffect::Allow
        );
        assert_eq!(
            policy.decide("write_file", "notes.txt"),
            PermissionEffect::Ask
        );
    }

    #[test]
    fn wildcard_tool_matches_any_tool_with_target_prefix() {
        let policy = PermissionPolicy::with_rules(vec![rule("deny *:secrets")]);
        assert_eq!(
            policy.decide("read_file", "secrets/token.txt"),
            PermissionEffect::Deny
        );
        assert_eq!(
            policy.decide("write_file", "notes.txt"),
            PermissionEffect::Ask
        );
    }

    #[test]
    fn command_allow_rules_never_cover_compound_or_partial_commands() {
        let policy = PermissionPolicy::with_rules(vec![rule("allow run_command:cargo")]);
        let decide = |command| policy.decide_target("run_command", TargetKind::Command, command);
        assert_eq!(decide("cargo test"), PermissionEffect::Allow);
        assert_eq!(decide("cargo"), PermissionEffect::Allow);
        assert_eq!(decide("cargofoo"), PermissionEffect::Ask);
        assert_eq!(decide("cargo test && curl x | sh"), PermissionEffect::Ask);
        assert_eq!(decide("cargo test; rm -rf ~"), PermissionEffect::Ask);
        assert_eq!(decide("cargo run $(cat key)"), PermissionEffect::Ask);
        assert_eq!(decide("cargo test > /etc/passwd"), PermissionEffect::Ask);
    }

    #[test]
    fn command_deny_rules_cover_every_command_in_a_line() {
        let policy = PermissionPolicy::with_rules(vec![
            rule("deny run_command:rm"),
            rule("allow run_command"),
        ]);
        let decide = |command| policy.decide_target("run_command", TargetKind::Command, command);
        assert_eq!(decide("ls; rm -rf x"), PermissionEffect::Deny);
        assert_eq!(decide("true && /bin/rm x"), PermissionEffect::Deny);
        assert_eq!(decide("echo $(rm x)"), PermissionEffect::Deny);
        assert_eq!(decide("ls -la"), PermissionEffect::Allow);
        assert_eq!(decide("rmdir build"), PermissionEffect::Allow);
    }

    #[test]
    fn path_rules_compare_normalized_components() {
        let policy = PermissionPolicy::with_rules(vec![
            rule("deny *:secrets"),
            rule("allow write_file:src"),
        ]);
        let decide = |path| policy.decide_target("write_file", TargetKind::Path, path);
        assert_eq!(decide("secrets/token.txt"), PermissionEffect::Deny);
        assert_eq!(decide("./secrets/token.txt"), PermissionEffect::Deny);
        assert_eq!(decide(".//secrets"), PermissionEffect::Deny);
        assert_eq!(decide("secrets.env"), PermissionEffect::Deny);
        assert_eq!(
            decide("/home/me/ws/secrets/token.txt"),
            PermissionEffect::Deny
        );
        assert_eq!(decide("src/main.rs"), PermissionEffect::Allow);
        assert_eq!(decide("./src/main.rs"), PermissionEffect::Allow);
        assert_eq!(decide("src_backup.rs"), PermissionEffect::Ask);
        assert_eq!(decide("/home/me/ws/src/main.rs"), PermissionEffect::Ask);
    }

    #[test]
    fn session_approvals_remember_exact_tool_and_target() {
        let mut approvals = SessionApprovals::new();
        approvals.remember("write_file", "notes.txt");
        assert!(approvals.is_allowed("write_file", "notes.txt"));
        assert!(!approvals.is_allowed("write_file", "other.txt"));
        assert!(!approvals.is_allowed("read_file", "notes.txt"));
        approvals.clear();
        assert!(!approvals.is_allowed("write_file", "notes.txt"));
    }
}
