//! Explicit project skill discovery for scoped agent instructions.
//!
//! Skills are directories containing a `SKILL.md` file with `name` and
//! `description` frontmatter. Discovery is deterministic and explicit loading
//! keeps skill text out of the system prompt until requested.

use std::fs;
use std::path::{Path, PathBuf};

const SKILL_FILE: &str = "SKILL.md";
const MAX_SKILL_BYTES: u64 = 64 * 1024;

/// Errors from skill discovery or loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillError {
    pub message: String,
}

impl SkillError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SkillError {}

/// Summary of a discovered skill without its instruction body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub directory: PathBuf,
}

/// A loaded skill with its scoped instruction body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub directory: PathBuf,
    pub instructions: String,
}

impl Skill {
    pub fn scoped_instructions(&self) -> String {
        format!(
            "# Skill: {}\n{}\n\n{}",
            self.name, self.description, self.instructions
        )
    }
}

/// Discover skills across ordered roots; earlier roots win on duplicate names.
pub fn discover_skills(roots: &[PathBuf]) -> Result<Vec<SkillSummary>, SkillError> {
    let mut discovered: Vec<SkillSummary> = Vec::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        let mut names: Vec<String> = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            if validate_skill_name(&name).is_err() {
                continue;
            }
            if !path.join(SKILL_FILE).is_file() {
                continue;
            }
            if discovered.iter().any(|skill| skill.name == name)
                || names.iter().any(|existing| existing == &name)
            {
                continue;
            }
            names.push(name);
        }
        names.sort();
        for name in names {
            let directory = root.join(&name);
            let skill = read_skill(&directory)?;
            if discovered
                .iter()
                .any(|existing| existing.name == skill.name)
            {
                continue;
            }
            discovered.push(SkillSummary {
                name: skill.name,
                description: skill.description,
                directory,
            });
        }
    }
    discovered.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(discovered)
}

/// Explicitly load one skill by name from ordered roots.
pub fn load_skill(roots: &[PathBuf], name: &str) -> Result<Skill, SkillError> {
    validate_skill_name(name)?;
    for root in roots {
        let directory = root.join(name);
        let file = directory.join(SKILL_FILE);
        if !file.is_file() {
            continue;
        }
        return read_skill(&directory);
    }
    Err(SkillError::new(format!("unknown skill '{name}'")))
}

fn read_skill(directory: &Path) -> Result<Skill, SkillError> {
    let file = directory.join(SKILL_FILE);
    let metadata = fs::metadata(&file)
        .map_err(|error| SkillError::new(format!("cannot read skill: {error}")))?;
    if metadata.len() > MAX_SKILL_BYTES {
        return Err(SkillError::new(
            "skill file too large: exceeds 64 KiB limit",
        ));
    }
    let content = fs::read_to_string(&file)
        .map_err(|error| SkillError::new(format!("cannot read skill: {error}")))?;
    let (name, description, instructions) = parse_skill_markdown(&content).map_err(|error| {
        SkillError::new(format!("invalid skill '{}': {error}", directory.display()))
    })?;
    let expected = directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name != expected {
        return Err(SkillError::new(format!(
            "invalid skill '{}': frontmatter name '{name}' must match directory '{expected}'",
            directory.display()
        )));
    }
    Ok(Skill {
        name,
        description,
        directory: directory.to_owned(),
        instructions,
    })
}

fn parse_skill_markdown(content: &str) -> Result<(String, String, String), String> {
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err("missing frontmatter opener '---'".to_owned());
    }
    let mut frontmatter: Vec<String> = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        frontmatter.push(line.to_owned());
    }
    if !closed {
        return Err("missing frontmatter closer '---'".to_owned());
    }
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    for line in frontmatter {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("invalid frontmatter line '{trimmed}'"))?;
        match key.trim() {
            "name" => name = Some(value.trim().to_owned()),
            "description" => description = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    let name = name
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "frontmatter must contain non-empty 'name' and 'description'".to_owned())?;
    let description = description
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "frontmatter must contain non-empty 'name' and 'description'".to_owned())?;
    validate_skill_name(&name).map_err(|error| error.to_string())?;
    let instructions: String = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    if instructions.is_empty() {
        return Err("skill body must not be empty".to_owned());
    }
    Ok((name, description, instructions))
}

fn validate_skill_name(name: &str) -> Result<(), SkillError> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(SkillError::new(
            "skill name must be 1-64 ASCII letters, numbers, '-' or '_'",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{discover_skills, load_skill, parse_skill_markdown};

    const VALID: &str =
        "---\nname: review\ndescription: Review changes\n---\n\nCheck diffs carefully.\n";

    fn skill_directory(
        root_label: &str,
        root: &std::path::Path,
        name: &str,
        content: &str,
    ) -> std::path::PathBuf {
        let _ = root_label;
        let directory = root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("SKILL.md"), content).unwrap();
        directory
    }

    fn test_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("a-rvm-skill-{label}-{}", std::process::id()))
    }

    #[test]
    fn parses_valid_skill_markdown() {
        let (name, description, instructions) = parse_skill_markdown(VALID).unwrap();
        assert_eq!(name, "review");
        assert_eq!(description, "Review changes");
        assert!(instructions.contains("Check diffs"));
    }

    #[test]
    fn rejects_malformed_skill_markdown() {
        assert!(parse_skill_markdown("no frontmatter").is_err());
        assert!(parse_skill_markdown("---\nname: review\n---\nbody").is_err());
        assert!(parse_skill_markdown("---\nname: review\ndescription: x\n---\n").is_err());
        assert!(parse_skill_markdown("---\nname: bad name!\ndescription: x\n---\nbody").is_err());
    }

    #[test]
    fn discovery_is_sorted_and_load_fails_closed() {
        let root = test_root("discover");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        skill_directory(
            "discover",
            &root,
            "b-skill",
            VALID.replace("review", "b-skill").as_str(),
        );
        skill_directory(
            "discover",
            &root,
            "a-skill",
            VALID.replace("review", "a-skill").as_str(),
        );
        std::fs::write(
            root.join("a-skill").join("SKILL.md"),
            VALID.replace("review", "a-skill"),
        )
        .unwrap();

        let discovered = discover_skills(&[root.clone()]).unwrap();
        assert_eq!(discovered.len(), 2);
        assert_eq!(discovered[0].name, "a-skill");
        assert_eq!(discovered[1].name, "b-skill");
        assert!(load_skill(&[root.clone()], "missing").is_err());
        assert!(load_skill(&[root.clone()], "bad name!").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_name_mismatched_directory() {
        let root = test_root("mismatch");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        skill_directory("mismatch", &root, "somesill", VALID);
        assert!(load_skill(&[root.clone()], "somesill").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn earlier_roots_win_on_duplicates() {
        let first = test_root("first");
        let second = test_root("second");
        let _ = std::fs::remove_dir_all(&first);
        let _ = std::fs::remove_dir_all(&second);
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        skill_directory(
            "dup",
            &first,
            "dup",
            "---\nname: dup\ndescription: first\n---\nfirst body\n",
        );
        skill_directory(
            "dup",
            &second,
            "dup",
            "---\nname: dup\ndescription: second\n---\nsecond body\n",
        );

        let discovered = discover_skills(&[first.clone(), second.clone()]).unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].description, "first");
        assert_eq!(
            load_skill(&[first.clone(), second.clone()], "dup")
                .unwrap()
                .description,
            "first"
        );
        std::fs::remove_dir_all(first).unwrap();
        std::fs::remove_dir_all(second).unwrap();
    }
}
