use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Request-scoped skill content selected for a single prompt execution.
#[allow(missing_docs)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedSkill {
    pub name: String,
    pub content: String,
}

/// Parsed skill metadata loaded from a Markdown skill file.
#[allow(missing_docs)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SkillMetadata {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub requires_tools: Vec<String>,
    #[serde(default)]
    pub priority: i32,
}

/// Fully parsed skill loaded from disk.
#[allow(missing_docs)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub metadata: SkillMetadata,
    pub body: String,
    pub source_path: PathBuf,
}

impl Skill {
    /// Formats the skill into request-scoped prompt guidance text.
    pub fn to_resolved(&self) -> ResolvedSkill {
        let mut sections = vec![format!("## Skill: {}", self.metadata.name)];
        if !self.metadata.description.trim().is_empty() {
            sections.push(format!("Description: {}", self.metadata.description.trim()));
        }
        if !self.metadata.requires_tools.is_empty() {
            sections.push(format!(
                "Preferred tools: {}",
                self.metadata.requires_tools.join(", ")
            ));
        }
        if !self.body.trim().is_empty() {
            sections.push(String::new());
            sections.push(self.body.trim().to_owned());
        }

        ResolvedSkill {
            name: self.metadata.name.clone(),
            content: sections.join("\n"),
        }
    }
}

/// Errors that can occur while discovering or parsing skills from disk.
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum SkillStoreError {
    #[error("unable to determine Mirage config directory for skills")]
    ConfigDirectoryUnavailable,
    #[error("failed to read skills from disk: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse YAML frontmatter in {path}: {error}")]
    Frontmatter {
        path: PathBuf,
        error: serde_yaml::Error,
    },
}

/// Returns the default root directory Mirage uses for global skills.
pub fn default_skills_root() -> Result<PathBuf, SkillStoreError> {
    if let Ok(path) = env::var("MIRAGE_SKILLS_DIR") {
        return Ok(PathBuf::from(path));
    }

    let base = if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(path)
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".config")
    } else {
        return Err(SkillStoreError::ConfigDirectoryUnavailable);
    };

    Ok(base.join("mirage").join("skills"))
}

/// Loads all skills from Mirage's default global skills directory.
pub fn load_default_skills() -> Result<Vec<Skill>, SkillStoreError> {
    let root = default_skills_root()?;
    load_skills_from_dir(&root)
}

/// Loads all skills from a specific root directory.
pub fn load_skills_from_dir(root: &Path) -> Result<Vec<Skill>, SkillStoreError> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let mut skill_files = Vec::new();
    collect_skill_files(root, &mut skill_files)?;
    skill_files.sort();

    let mut skills = Vec::new();
    for path in skill_files {
        skills.push(parse_skill_file(&path)?);
    }
    Ok(skills)
}

/// Selects the best matching skills for a prompt using simple trigger and token scoring.
pub fn match_skills(skills: &[Skill], prompt: &str, limit: usize) -> Vec<ResolvedSkill> {
    let normalized_prompt = normalize(prompt);
    let prompt_tokens = token_set(prompt);

    let mut scored = skills
        .iter()
        .filter_map(|skill| {
            let score = score_skill(skill, &normalized_prompt, &prompt_tokens);
            (score > 0).then_some((score, skill))
        })
        .collect::<Vec<_>>();

    scored.sort_by(|(left_score, left_skill), (right_score, right_skill)| {
        right_score
            .cmp(left_score)
            .then_with(|| left_skill.metadata.name.cmp(&right_skill.metadata.name))
    });

    scored
        .into_iter()
        .take(limit)
        .map(|(_, skill)| skill.to_resolved())
        .collect()
}

/// Combines a prompt with request-scoped skill guidance text.
pub fn prompt_with_resolved_skills(prompt: &str, resolved_skills: &[ResolvedSkill]) -> String {
    if resolved_skills.is_empty() {
        return prompt.to_owned();
    }

    let skill_blocks = resolved_skills
        .iter()
        .map(|skill| skill.content.trim())
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "Use the following request-scoped skills as procedural guidance when relevant.\n\n{}\n\n## User Request\n{}",
        skill_blocks, prompt
    )
}

/// Recursively collects Markdown skill files from the provided directory.
fn collect_skill_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<(), SkillStoreError> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_skill_files(&path, files)?;
        } else if path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| {
                name.eq_ignore_ascii_case("SKILL.md") || name.ends_with(".skill.md")
            })
        {
            files.push(path);
        }
    }
    Ok(())
}

/// Parses a single Markdown skill file with optional YAML frontmatter.
fn parse_skill_file(path: &Path) -> Result<Skill, SkillStoreError> {
    let contents = fs::read_to_string(path)?;
    let (metadata, body) = parse_skill_contents(&contents, path)?;
    Ok(Skill {
        metadata,
        body,
        source_path: path.to_path_buf(),
    })
}

/// Parses frontmatter and body content from a Markdown skill file.
fn parse_skill_contents(
    contents: &str,
    path: &Path,
) -> Result<(SkillMetadata, String), SkillStoreError> {
    let mut metadata = SkillMetadata {
        name: default_skill_name(path),
        ..SkillMetadata::default()
    };

    let body = if let Some(rest) = contents.strip_prefix("---\n") {
        if let Some((frontmatter, tail)) = rest.split_once("\n---\n") {
            let parsed = serde_yaml::from_str::<SkillMetadata>(frontmatter).map_err(|error| {
                SkillStoreError::Frontmatter {
                    path: path.to_path_buf(),
                    error,
                }
            })?;
            metadata = SkillMetadata {
                name: if parsed.name.trim().is_empty() {
                    metadata.name
                } else {
                    parsed.name
                },
                description: parsed.description,
                triggers: parsed.triggers,
                requires_tools: parsed.requires_tools,
                priority: parsed.priority,
            };
            tail.to_owned()
        } else {
            contents.to_owned()
        }
    } else {
        contents.to_owned()
    };

    Ok((metadata, body.trim().to_owned()))
}

/// Derives a human-readable fallback skill name from the skill file path.
fn default_skill_name(path: &Path) -> String {
    let file_stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("skill");
    if file_stem.eq_ignore_ascii_case("skill") {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .unwrap_or("skill")
            .to_owned()
    } else {
        file_stem.to_owned()
    }
}

/// Scores a single skill against a user prompt.
fn score_skill(skill: &Skill, normalized_prompt: &str, prompt_tokens: &HashSet<String>) -> i64 {
    let mut score = i64::from(skill.metadata.priority) * 100;

    let name = normalize(&skill.metadata.name);
    if !name.is_empty() && normalized_prompt.contains(&name) {
        score += 300;
    }

    let description = normalize(&skill.metadata.description);
    if !description.is_empty() && normalized_prompt.contains(&description) {
        score += 150;
    }

    for trigger in &skill.metadata.triggers {
        let normalized_trigger = normalize(trigger);
        if normalized_trigger.is_empty() {
            continue;
        }

        if normalized_prompt.contains(&normalized_trigger) {
            score += 400 + normalized_trigger.len() as i64;
        } else {
            let trigger_tokens = token_set(&normalized_trigger);
            let overlap = trigger_tokens.intersection(prompt_tokens).count() as i64;
            if overlap > 0 {
                score += overlap * 40;
            }
        }
    }

    let searchable = format!(
        "{} {} {}",
        skill.metadata.name, skill.metadata.description, skill.body
    );
    let searchable_tokens = token_set(&searchable);
    let overlap = searchable_tokens.intersection(prompt_tokens).count() as i64;
    score + overlap * 5
}

/// Normalizes text for simple substring matching.
fn normalize(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Splits text into a normalized token set for coarse matching.
fn token_set(value: &str) -> HashSet<String> {
    value
        .to_ascii_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 2)
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        Skill, SkillMetadata, default_skill_name, match_skills, parse_skill_contents,
        prompt_with_resolved_skills,
    };
    use std::path::Path;

    /// Verifies that YAML frontmatter is parsed into structured skill metadata.
    #[test]
    fn parses_skill_frontmatter() {
        let (metadata, body) = parse_skill_contents(
            "---\nname: council-bin-days\ndescription: Find council bin days\ntriggers:\n  - bin day\npriority: 5\n---\nUse the council site.",
            Path::new("skills/council-bin-days/SKILL.md"),
        )
        .unwrap();

        assert_eq!(metadata.name, "council-bin-days");
        assert_eq!(metadata.description, "Find council bin days");
        assert_eq!(metadata.triggers, vec!["bin day"]);
        assert_eq!(body, "Use the council site.");
    }

    /// Verifies that trigger matches resolve the expected skill for a prompt.
    #[test]
    fn matches_skills_by_trigger() {
        let skills = vec![Skill {
            metadata: SkillMetadata {
                name: "council-bin-days".to_owned(),
                description: "Find local council bin collection dates".to_owned(),
                triggers: vec!["bin day".to_owned(), "recycling collection".to_owned()],
                requires_tools: vec!["playwright".to_owned()],
                priority: 1,
            },
            body: "Use the council portal.".to_owned(),
            source_path: Path::new("skills/council-bin-days/SKILL.md").to_path_buf(),
        }];

        let resolved = match_skills(&skills, "When is my next bin day?", 3);
        assert_eq!(resolved.len(), 1);
        assert!(resolved[0].content.contains("council-bin-days"));
    }

    /// Verifies that resolved skills are appended above the user request in the effective prompt text.
    #[test]
    fn expands_prompt_with_resolved_skills() {
        let prompt = prompt_with_resolved_skills(
            "When is my next bin day?",
            &[super::ResolvedSkill {
                name: "council-bin-days".to_owned(),
                content: "## Skill: council-bin-days\nUse the council portal.".to_owned(),
            }],
        );

        assert!(prompt.contains("request-scoped skills"));
        assert!(prompt.contains("council-bin-days"));
        assert!(prompt.contains("When is my next bin day?"));
    }

    /// Verifies that `SKILL.md` files inherit their directory name as the fallback skill id.
    #[test]
    fn derives_directory_name_for_skill_markdown_files() {
        let name = default_skill_name(Path::new("skills/council-bin-days/SKILL.md"));
        assert_eq!(name, "council-bin-days");
    }
}
