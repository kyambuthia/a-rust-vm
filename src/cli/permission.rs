//! `arvm permission` and the `--rule` flag: durable and per-run permission rules.

use super::paths::permission_rules_path;

fn load_stored_permission_policy(
    path: &std::path::Path,
) -> Result<a_rust_vm::permissions::PermissionPolicy, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(a_rust_vm::permissions::PermissionPolicy::new());
        }
        Err(error) => return Err(format!("cannot read permission rules: {error}")),
    };
    if bytes.is_empty() {
        return Ok(a_rust_vm::permissions::PermissionPolicy::new());
    }
    let stored: Vec<String> = serde_json::from_slice(&bytes)
        .map_err(|error| format!("cannot decode permission rules: {error}"))?;
    let mut policy = a_rust_vm::permissions::PermissionPolicy::new();
    for text in stored {
        let rule = a_rust_vm::permissions::parse_permission_rule(&text)
            .map_err(|error| format!("invalid stored rule '{text}': {error}"))?;
        policy.add_rule(rule);
    }
    Ok(policy)
}

fn save_stored_permission_rule_at(path: &std::path::Path, text: &str) -> Result<(), String> {
    let trimmed = text.trim().to_owned();
    a_rust_vm::permissions::parse_permission_rule(&trimmed)
        .map_err(|error| format!("invalid rule '{trimmed}': {error}"))?;
    let mut stored: Vec<String> = match std::fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Vec::new(),
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("cannot decode permission rules: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(format!("cannot read permission rules: {error}")),
    };
    for existing in &stored {
        a_rust_vm::permissions::parse_permission_rule(existing)
            .map_err(|error| format!("invalid stored rule '{existing}': {error}"))?;
    }
    stored.push(trimmed);
    let content = serde_json::to_vec_pretty(&stored)
        .map_err(|error| format!("cannot encode permission rules: {error}"))?;
    a_rust_vm::private_fs::write_private(path, &content)
        .map_err(|error| format!("cannot save permission rules: {error}"))
}

fn save_stored_permission_rule(text: &str) -> Result<(), String> {
    let path = permission_rules_path();
    save_stored_permission_rule_at(&path, text)
}

/// First match wins: `--rule` flags, then configuration rules (already in
/// workspace-then-user order), then rules saved with `arvm permission add`.
fn merged_permission_policy_at(
    path: &std::path::Path,
    arguments: &[String],
    config_rules: Vec<a_rust_vm::permissions::PermissionRule>,
) -> Result<a_rust_vm::permissions::PermissionPolicy, String> {
    let mut policy = build_permission_policy(arguments)?;
    for rule in config_rules {
        policy.add_rule(rule);
    }
    for rule in load_stored_permission_policy(path)?.rules().to_vec() {
        policy.add_rule(rule);
    }
    Ok(policy)
}

pub(crate) fn merged_permission_policy(
    arguments: &[String],
    config_rules: Vec<a_rust_vm::permissions::PermissionRule>,
) -> Result<a_rust_vm::permissions::PermissionPolicy, String> {
    merged_permission_policy_at(&permission_rules_path(), arguments, config_rules)
}

fn permission_usage() {
    eprintln!("usage: arvm permission list|add <rule>|clear");
}

pub(crate) fn run_permission_command(arguments: &[String]) {
    let path = permission_rules_path();
    let Some(command) = arguments.first().map(String::as_str) else {
        permission_usage();
        std::process::exit(2);
    };
    match command {
        "list" => match load_stored_permission_policy(&path) {
            Ok(policy) => {
                if policy.rules().is_empty() {
                    println!("(no rules)");
                } else {
                    for rule in policy.rules() {
                        let effect = match rule.effect {
                            a_rust_vm::permissions::PermissionEffect::Allow => "allow",
                            a_rust_vm::permissions::PermissionEffect::Ask => "ask",
                            a_rust_vm::permissions::PermissionEffect::Deny => "deny",
                        };
                        if rule.target_prefix.is_empty() {
                            println!("{effect} {}", rule.tool);
                        } else {
                            println!("{effect} {}:{}", rule.tool, rule.target_prefix);
                        }
                    }
                }
            }
            Err(error) => {
                eprintln!("[permission] {error}");
                std::process::exit(1);
            }
        },
        "add" => {
            if arguments.len() != 2 {
                permission_usage();
                std::process::exit(2);
            }
            if let Err(error) = save_stored_permission_rule(&arguments[1]) {
                eprintln!("[permission] {error}");
                std::process::exit(1);
            }
            println!("saved rule");
        }
        "clear" => {
            if arguments.len() != 1 {
                permission_usage();
                std::process::exit(2);
            }
            match std::fs::remove_file(&path) {
                Ok(()) => println!("cleared rules"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    println!("(no rules)")
                }
                Err(error) => {
                    eprintln!("[permission] cannot clear rules: {error}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            permission_usage();
            std::process::exit(2);
        }
    }
}

pub(crate) fn extract_permission_rule_texts(arguments: &[String]) -> Vec<String> {
    arguments
        .windows(2)
        .filter(|window| window[0] == "--rule")
        .map(|window| window[1].clone())
        .collect()
}

pub(crate) fn build_permission_policy(
    arguments: &[String],
) -> Result<a_rust_vm::permissions::PermissionPolicy, String> {
    let mut policy = a_rust_vm::permissions::PermissionPolicy::new();
    for text in extract_permission_rule_texts(arguments) {
        let rule = a_rust_vm::permissions::parse_permission_rule(&text)
            .map_err(|error| format!("invalid --rule '{text}': {error}"))?;
        policy.add_rule(rule);
    }
    Ok(policy)
}

#[cfg(test)]
mod tests {

    #[test]
    fn durable_permission_rules_round_trip_missing_and_invalid() {
        let path = std::env::temp_dir().join(format!(
            "a-rvm-rules-{}-{}.json",
            std::process::id(),
            "round-trip"
        ));
        let _ = std::fs::remove_file(&path);
        let empty = super::load_stored_permission_policy(&path).unwrap();
        assert!(empty.rules().is_empty());
        super::save_stored_permission_rule_at(&path, "deny run_command:rm").unwrap();
        let stored = super::load_stored_permission_policy(&path).unwrap();
        assert_eq!(
            stored.decide("run_command", "rm -rf /tmp"),
            a_rust_vm::permissions::PermissionEffect::Deny
        );
        assert!(super::save_stored_permission_rule_at(&path, "permit write_file").is_err());
        std::fs::write(&path, b"not json").unwrap();
        assert!(super::load_stored_permission_policy(&path).is_err());
        std::fs::write(&path, b"[\"permit x\"]").unwrap();
        assert!(super::load_stored_permission_policy(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cli_rules_take_precedence_over_stored_rules() {
        let path = std::env::temp_dir().join(format!(
            "a-rvm-rules-{}-{}.json",
            std::process::id(),
            "precedence"
        ));
        let _ = std::fs::remove_file(&path);
        super::save_stored_permission_rule_at(&path, "deny write_file:notes.txt").unwrap();
        let arguments = ["--rule".to_owned(), "allow write_file:notes.txt".to_owned()];
        let policy = super::merged_permission_policy_at(&path, &arguments, Vec::new()).unwrap();
        assert_eq!(
            policy.decide("write_file", "notes.txt"),
            a_rust_vm::permissions::PermissionEffect::Allow
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn config_rules_sit_between_flags_and_stored_rules() {
        let path = std::env::temp_dir().join(format!(
            "a-rvm-rules-{}-{}.json",
            std::process::id(),
            "config-order"
        ));
        let _ = std::fs::remove_file(&path);
        super::save_stored_permission_rule_at(&path, "allow run_command").unwrap();
        let config_rules =
            vec![a_rust_vm::permissions::parse_permission_rule("deny run_command:cargo").unwrap()];
        let policy = super::merged_permission_policy_at(&path, &[], config_rules.clone()).unwrap();
        assert_eq!(
            policy.decide("run_command", "cargo publish"),
            a_rust_vm::permissions::PermissionEffect::Deny
        );
        assert_eq!(
            policy.decide("run_command", "ls"),
            a_rust_vm::permissions::PermissionEffect::Allow
        );
        let flags = ["--rule".to_owned(), "ask run_command:cargo".to_owned()];
        let policy = super::merged_permission_policy_at(&path, &flags, config_rules).unwrap();
        assert_eq!(
            policy.decide("run_command", "cargo test"),
            a_rust_vm::permissions::PermissionEffect::Ask
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn permission_rule_flags_build_a_policy_or_fail_closed() {
        let arguments = [
            "--rule".to_owned(),
            "deny run_command:rm".to_owned(),
            "hi".to_owned(),
        ];
        let policy = super::build_permission_policy(&arguments).unwrap();
        assert_eq!(
            policy.decide("run_command", "rm -rf /tmp"),
            a_rust_vm::permissions::PermissionEffect::Deny
        );
        assert!(
            super::build_permission_policy(&["--rule".to_owned(), "permit x".to_owned()]).is_err()
        );
    }
}
