//! `arvm skill` and the `--skill` flag: project and user skills.

use super::paths::{user_config_root, working_directory};

fn skill_usage() {
    eprintln!("usage: arvm skill list|show <name>|load <name>");
}

pub(crate) fn skill_roots_for(
    project_root: &std::path::Path,
    user_root: &std::path::Path,
) -> Vec<std::path::PathBuf> {
    vec![project_root.join("skills"), user_root.join("skills")]
}

fn skill_roots() -> Vec<std::path::PathBuf> {
    skill_roots_for(&working_directory(), &user_config_root())
}

pub(crate) fn run_skill_command(arguments: &[String]) {
    let roots = skill_roots();
    let Some(command) = arguments.first().map(String::as_str) else {
        skill_usage();
        std::process::exit(2);
    };
    match command {
        "list" => {
            if arguments.len() != 1 {
                skill_usage();
                std::process::exit(2);
            }
            match a_rust_vm::skills::discover_skills(&roots) {
                Ok(skills) => {
                    if skills.is_empty() {
                        println!("(no skills)");
                    } else {
                        for skill in skills {
                            println!("{}\t{}", skill.name, skill.description);
                        }
                    }
                }
                Err(error) => {
                    eprintln!("[skill] {error}");
                    std::process::exit(1);
                }
            }
        }
        "show" | "load" => {
            if arguments.len() != 2 {
                skill_usage();
                std::process::exit(2);
            }
            match a_rust_vm::skills::load_skill(&roots, &arguments[1]) {
                Ok(skill) => {
                    if command == "show" {
                        println!("{}\t{}", skill.name, skill.description);
                    } else {
                        println!("{}", skill.scoped_instructions());
                    }
                }
                Err(error) => {
                    eprintln!("[skill] {error}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            skill_usage();
            std::process::exit(2);
        }
    }
}

pub(crate) fn extract_skill_names(arguments: &[String]) -> Vec<String> {
    arguments
        .windows(2)
        .filter(|window| window[0] == "--skill")
        .map(|window| window[1].clone())
        .collect()
}

pub(crate) fn load_requested_skills(root: &std::path::Path, arguments: &[String]) -> Vec<String> {
    let roots = skill_roots_for(root, &user_config_root());
    extract_skill_names(arguments)
        .iter()
        .map(|name| {
            a_rust_vm::skills::load_skill(&roots, name)
                .map(|skill| skill.scoped_instructions())
                .map_err(|error| format!("invalid --skill '{name}': {error}"))
        })
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| {
            eprintln!("[skill] {error}");
            std::process::exit(2);
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_roots_prefer_project_then_user_and_cli_loads() {
        let project =
            std::env::temp_dir().join(format!("a-rvm-skill-cli-project-{}", std::process::id()));
        let user =
            std::env::temp_dir().join(format!("a-rvm-skill-cli-user-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&user);
        std::fs::create_dir_all(project.join("skills").join("review")).unwrap();
        std::fs::write(
            project.join("skills").join("review").join("SKILL.md"),
            "---\nname: review\ndescription: Review changes\n---\nCheck diffs.\n",
        )
        .unwrap();

        let roots = skill_roots_for(&project, &user);
        assert_eq!(roots, vec![project.join("skills"), user.join("skills")]);
        let discovered = a_rust_vm::skills::discover_skills(&roots).unwrap();
        assert_eq!(discovered.len(), 1);
        let loaded = a_rust_vm::skills::load_skill(&roots, "review").unwrap();
        assert!(loaded.scoped_instructions().contains("Check diffs."));
        assert!(a_rust_vm::skills::load_skill(&roots, "missing").is_err());
        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&user);
    }
}
