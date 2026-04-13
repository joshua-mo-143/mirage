use std::env;

const MIRAGE_IDENTITY: &str = "You are Mirage, an autonomous assistant. Take initiative when it helps, use available tools to complete work end-to-end when appropriate, and keep the user informed about progress, limitations, and outcomes. Stay honest, practical, and task-focused.";

const TOOL_USAGE_GUIDANCE: &str = "Tool usage guidance:
- Prefer discovering capabilities by using `bash` instead of assuming what commands, binaries, files, or directories are available.
- Use `bash` freely for arbitrary shell commands, environment inspection, and capability discovery.
- Use `playwright` for headless browser automation when a task needs webpage interaction, form filling, visible text extraction, or screenshots.
- Prefer `subagent` for longer tasks that will require reading files, exploring code, or taking multiple tool-calling turns; use it to delegate deeper investigation or planning to a child Cursor agent and incorporate its final answer.
- Use `read_file` to inspect files before editing them when needed.
- Prefer `edit_file` for modifying part of an existing file.
- Use `write_file` only when creating a new file, replacing an entire file, or appending whole-file content intentionally.
- Use `prompt_cursor` when you want the local Cursor agent CLI (`agent -p`) to answer or inspect something.";

/// Builds the full Mirage preamble used for agent execution.
pub fn build_mirage_preamble(system_prompt: Option<&str>, personality: Option<&str>) -> String {
    let mut sections = vec![MIRAGE_IDENTITY.to_owned()];

    if let Some(personality) = normalized_prompt(personality) {
        sections.push(format!(
            "Personality:\nAdopt the following personality in your tone and phrasing while remaining competent, honest, and task-focused:\n{personality}"
        ));
    }

    if let Some(system_prompt) = normalized_prompt(system_prompt) {
        sections.push(format!("Additional instructions:\n{system_prompt}"));
    }

    sections.push(TOOL_USAGE_GUIDANCE.to_owned());
    sections.join("\n\n")
}

/// Resolves Mirage-specific runtime instructions from supported environment variables.
pub fn resolve_system_prompt() -> Option<String> {
    env_prompt("MIRAGE_SYSTEM_PROMPT").or_else(|| env_prompt("VENICE_SYSTEM_PROMPT"))
}

/// Builds a user-visible summary of custom prompt configuration for the transcript/status views.
pub fn configured_prompt_summary(
    system_prompt: Option<&str>,
    personality: Option<&str>,
) -> Option<String> {
    let personality = normalized_prompt(personality);
    let system_prompt = normalized_prompt(system_prompt);
    if personality.is_none() && system_prompt.is_none() {
        return None;
    }

    let mut sections = Vec::new();
    if let Some(personality) = personality {
        sections.push(format!("Personality:\n{personality}"));
    }
    if let Some(system_prompt) = system_prompt {
        sections.push(format!("Additional instructions:\n{system_prompt}"));
    }
    Some(sections.join("\n\n"))
}

/// Returns whether Mirage has runtime prompt configuration such as instructions or personality.
pub fn has_custom_prompt_configuration(
    system_prompt: Option<&str>,
    personality: Option<&str>,
) -> bool {
    normalized_prompt(system_prompt).is_some() || normalized_prompt(personality).is_some()
}

/// Trims prompt text and drops empty values.
fn normalized_prompt(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Loads and normalizes a prompt-like value from an environment variable.
fn env_prompt(var_name: &str) -> Option<String> {
    env::var(var_name).ok().and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::{
        build_mirage_preamble, configured_prompt_summary, has_custom_prompt_configuration,
    };

    /// Verifies that the shared preamble always includes Mirage's autonomous identity.
    #[test]
    fn preamble_includes_autonomous_identity() {
        let preamble = build_mirage_preamble(None, None);
        assert!(preamble.contains("You are Mirage, an autonomous assistant."));
        assert!(preamble.contains("Tool usage guidance:"));
    }

    /// Verifies that configured prompt summaries include both personality and instructions when set.
    #[test]
    fn prompt_summary_includes_personality_and_instructions() {
        let summary = configured_prompt_summary(Some("Be concise."), Some("Dry and witty."))
            .expect("summary should exist");
        assert!(summary.contains("Personality:\nDry and witty."));
        assert!(summary.contains("Additional instructions:\nBe concise."));
    }

    /// Verifies that empty prompt extensions are ignored.
    #[test]
    fn empty_prompt_extensions_are_not_treated_as_configured() {
        assert!(!has_custom_prompt_configuration(Some("   "), Some("")));
        assert!(configured_prompt_summary(Some("   "), Some("")).is_none());
    }
}
