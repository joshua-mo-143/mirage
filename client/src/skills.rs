use mirage_core::skills::{ResolvedSkill, Skill, load_default_skills};

/// Lightweight skill summary used by the TUI command layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AvailableSkill {
    pub(crate) name: String,
    pub(crate) description: String,
}

/// Loads the available local Mirage skills for explicit user selection.
pub(crate) fn list_available_skills() -> Result<Vec<AvailableSkill>, String> {
    let skills = sorted_skills(load_default_skills().map_err(|error| error.to_string())?);
    Ok(skills
        .into_iter()
        .map(|skill| AvailableSkill {
            name: skill.metadata.name,
            description: skill.metadata.description,
        })
        .collect())
}

/// Resolves one explicitly selected skill by name or 1-based list index.
pub(crate) fn resolve_selected_skill(selection: &str) -> Result<ResolvedSkill, String> {
    let skills = sorted_skills(load_default_skills().map_err(|error| error.to_string())?);
    resolve_selected_skill_from_slice(&skills, selection)
}

/// Sorts skills into a deterministic display order for the TUI.
fn sorted_skills(mut skills: Vec<Skill>) -> Vec<Skill> {
    skills.sort_by(|left, right| {
        left.metadata
            .name
            .to_ascii_lowercase()
            .cmp(&right.metadata.name.to_ascii_lowercase())
    });
    skills
}

/// Resolves one explicitly selected skill from an already loaded slice.
fn resolve_selected_skill_from_slice(
    skills: &[Skill],
    selection: &str,
) -> Result<ResolvedSkill, String> {
    let selection = selection.trim();
    if selection.is_empty() {
        return Err("missing skill selection".to_owned());
    }

    if let Ok(index) = selection.parse::<usize>() {
        let skill = skills
            .get(index.saturating_sub(1))
            .ok_or_else(|| format!("no skill exists at index {index}"))?;
        return Ok(skill.to_resolved());
    }

    let skill = skills
        .iter()
        .find(|skill| skill.metadata.name.eq_ignore_ascii_case(selection))
        .ok_or_else(|| format!("no skill named `{selection}` was found"))?;
    Ok(skill.to_resolved())
}

#[cfg(test)]
mod tests {
    use super::resolve_selected_skill_from_slice;
    use mirage_core::skills::{Skill, SkillMetadata};
    use std::path::PathBuf;

    /// Verifies that numbered selections resolve against the displayed skill ordering.
    #[test]
    fn resolves_selected_skill_by_index() {
        let skills = vec![
            Skill {
                metadata: SkillMetadata {
                    name: "alpha".to_owned(),
                    description: String::new(),
                    triggers: Vec::new(),
                    requires_tools: Vec::new(),
                    priority: 0,
                },
                body: "Use alpha".to_owned(),
                source_path: PathBuf::from("skills/alpha/SKILL.md"),
            },
            Skill {
                metadata: SkillMetadata {
                    name: "beta".to_owned(),
                    description: String::new(),
                    triggers: Vec::new(),
                    requires_tools: Vec::new(),
                    priority: 0,
                },
                body: "Use beta".to_owned(),
                source_path: PathBuf::from("skills/beta/SKILL.md"),
            },
        ];

        let resolved = resolve_selected_skill_from_slice(&skills, "2").unwrap();
        assert_eq!(resolved.name, "beta");
    }

    /// Verifies that case-insensitive skill-name selections resolve correctly.
    #[test]
    fn resolves_selected_skill_by_name() {
        let skills = vec![Skill {
            metadata: SkillMetadata {
                name: "council-bin-days".to_owned(),
                description: String::new(),
                triggers: Vec::new(),
                requires_tools: Vec::new(),
                priority: 0,
            },
            body: "Use the council website.".to_owned(),
            source_path: PathBuf::from("skills/council-bin-days/SKILL.md"),
        }];

        let resolved = resolve_selected_skill_from_slice(&skills, "Council-Bin-Days").unwrap();
        assert_eq!(resolved.name, "council-bin-days");
    }
}
