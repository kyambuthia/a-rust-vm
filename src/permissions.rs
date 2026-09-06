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

impl PermissionRule {
    pub fn matches(&self, tool: &str, target: &str) -> bool {
        (self.tool == "*" || self.tool == tool) && target.starts_with(&self.target_prefix)
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
        self.rules
            .iter()
            .find(|rule| rule.matches(tool, target))
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
        PermissionEffect, PermissionPolicy, PermissionRule, SessionApprovals, parse_permission_rule,
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
