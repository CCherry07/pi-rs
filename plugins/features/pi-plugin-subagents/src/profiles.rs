use std::path::PathBuf;
use std::time::Duration;

use pi_core::ThinkingLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SystemPromptMode {
    Append,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubagentProfile {
    pub(crate) name: String,
    pub(crate) aliases: Vec<String>,
    pub(crate) description: String,
    pub(crate) instructions: String,
    pub(crate) system_prompt_mode: SystemPromptMode,
    pub(crate) allow_nested_subagents: bool,
    pub(crate) max_subagent_depth: Option<usize>,
    /// `None` inherits the calling session; `Some([])` selects no tools.
    pub(crate) tools: Option<Vec<String>>,
    /// Applied after inherited or explicit tool selection. Unknown names are ignored.
    pub(crate) excluded_tools: Vec<String>,
    /// Normalized model reference. `None` inherits the calling session.
    pub(crate) model: Option<String>,
    pub(crate) thinking_level: Option<ThinkingLevel>,
    pub(crate) inherit_skills: bool,
    pub(crate) skills: Vec<String>,
    pub(crate) skill_paths: Vec<PathBuf>,
    pub(crate) timeout: Option<Duration>,
}

impl SubagentProfile {
    fn builtin(name: &str, description: &str, instructions: &str) -> Self {
        Self {
            name: name.to_string(),
            aliases: Vec::new(),
            description: description.to_string(),
            instructions: instructions.to_string(),
            system_prompt_mode: SystemPromptMode::Append,
            allow_nested_subagents: true,
            max_subagent_depth: None,
            tools: None,
            excluded_tools: Vec::new(),
            model: None,
            thinking_level: None,
            // Preserve the pre-frontmatter behavior of builtins, which used
            // append mode and therefore received the normal skill catalog.
            inherit_skills: true,
            skills: Vec::new(),
            skill_paths: Vec::new(),
            timeout: None,
        }
    }
}

pub(crate) fn builtin_profiles() -> Vec<SubagentProfile> {
    vec![
        SubagentProfile::builtin(
            "scout",
            "Map relevant code, entry points, data flow, risks, and open questions.",
            "Work as a fast codebase scout. Inspect before concluding, keep the search focused, cite exact paths and symbols, and return compressed context that another agent can act on. Do not edit files unless the delegated task explicitly asks you to.",
        ),
        SubagentProfile::builtin(
            "worker",
            "Implement a bounded change and verify it with focused checks.",
            "Work as the implementation thread. Validate the task against the code, make the smallest coherent edits, preserve unrelated work, and run checks proportionate to the change. Escalate material ambiguity in your result instead of inventing product decisions.",
        ),
        SubagentProfile::builtin(
            "reviewer",
            "Review code or a plan for correctness, tests, edge cases, and simplicity.",
            "Work as a read-only reviewer. Inspect the actual code, diff, tests, and stated intent. Report only evidence-backed findings, ordered by severity, with exact paths and actionable fixes. Do not edit files.",
        ),
        SubagentProfile::builtin(
            "oracle",
            "Challenge assumptions and provide an independent second opinion.",
            "Work as an independent technical oracle. Reconstruct the decision, challenge its assumptions, compare credible alternatives, and identify hidden risks. Do not edit files; return a concrete recommendation and the evidence behind it.",
        ),
        SubagentProfile::builtin(
            "delegate",
            "Handle a focused general-purpose task using the normal Pi capabilities.",
            "Work as a focused general delegate. Complete only the assigned task, use the available tools when they improve confidence, and return a concise result that the parent can directly incorporate.",
        ),
    ]
}

#[cfg(test)]
pub(crate) fn builtin_profile(name: &str) -> SubagentProfile {
    builtin_profiles()
        .into_iter()
        .find(|profile| profile.name == name)
        .expect("test must request a builtin subagent profile")
}

pub(crate) fn specialized_system_prompt(
    base: &str,
    profile: &SubagentProfile,
    depth: usize,
    max_depth: usize,
) -> String {
    let specialization = format!(
        "# Delegated subagent role: {name}\n\n{instructions}\n\nThis is isolated child depth {depth} of {max_depth}. {delegation} The leading `pi-rs-subagent-run` marker in the first user message is runtime metadata; do not quote it in your answer.",
        name = profile.name,
        instructions = profile.instructions,
        delegation = if profile.allow_nested_subagents {
            "You may delegate a smaller task through the `subagent` tool when that materially improves the result and the remaining depth and spawn budgets allow it."
        } else {
            "Complete this task directly; this agent definition does not authorize nested delegation."
        },
    );
    match profile.system_prompt_mode {
        SystemPromptMode::Append => format!("{base}\n\n{specialization}"),
        SystemPromptMode::Replace => specialization,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specialization_keeps_product_context_for_append_profiles() {
        let profile = builtin_profile("reviewer");
        let prompt = specialized_system_prompt("base prompt", &profile, 2, 3);
        assert!(prompt.starts_with("base prompt"));
        assert!(prompt.contains("Delegated subagent role: reviewer"));
        assert!(prompt.contains("depth 2 of 3"));
        assert!(prompt.contains("Do not edit files"));
    }

    #[test]
    fn replacement_profiles_receive_only_their_specialization() {
        let mut profile = builtin_profile("scout");
        profile.system_prompt_mode = SystemPromptMode::Replace;
        let prompt = specialized_system_prompt("base prompt", &profile, 1, 3);
        assert!(!prompt.contains("base prompt"));
        assert!(prompt.contains("Delegated subagent role: scout"));
    }
}
