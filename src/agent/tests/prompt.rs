use super::*;

#[test]
fn project_instructions_are_loaded_and_composed() {
    let root = std::env::temp_dir().join(format!("a-rust-vm-instructions-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("AGENTS.md"), "Keep changes focused.").unwrap();

    let instructions = load_project_instructions(&root).unwrap();
    let prompt = system_prompt_with_instructions(instructions.as_deref());

    assert!(prompt.contains("You are the A/RVM coding agent."));
    assert!(prompt.contains("Keep changes focused."));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn loaded_skills_are_appended_in_order_or_ignored_when_empty() {
    let base = "base prompt".to_owned();
    assert_eq!(system_prompt_with_skills(&base, &[]), base);
    assert_eq!(system_prompt_with_skills(&base, &["  ".to_owned()]), base);
    let composed = system_prompt_with_skills(
        &base,
        &[
            "# Skill: a\nfirst".to_owned(),
            "# Skill: b\nsecond".to_owned(),
        ],
    );
    assert!(composed.starts_with("base prompt"));
    assert!(composed.contains("Loaded skills"));
    assert!(composed.contains("first"));
    assert!(composed.contains("second"));
    assert!(composed.find("first").unwrap() < composed.find("second").unwrap());
}
