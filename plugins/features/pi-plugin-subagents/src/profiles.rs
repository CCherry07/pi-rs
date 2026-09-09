use std::path::PathBuf;
use std::time::Duration;

use pi_core::{IsolatedContextMode, ThinkingLevel};

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
    pub(crate) inherit_project_context: bool,
    pub(crate) allow_nested_subagents: bool,
    pub(crate) max_subagent_depth: Option<usize>,
    /// `None` inherits the calling session; `Some([])` selects no tools.
    pub(crate) tools: Option<Vec<String>>,
    /// Applied after inherited or explicit tool selection. Unknown names are ignored.
    pub(crate) excluded_tools: Vec<String>,
    /// Normalized model reference. `None` inherits the calling session.
    pub(crate) model: Option<String>,
    pub(crate) thinking_level: Option<ThinkingLevel>,
    pub(crate) default_context: IsolatedContextMode,
    pub(crate) inherit_skills: bool,
    pub(crate) skills: Vec<String>,
    pub(crate) skill_paths: Vec<PathBuf>,
    pub(crate) timeout: Option<Duration>,
}

struct BuiltinProfile {
    definition: &'static str,
    name: &'static str,
    description: &'static str,
    system_prompt_mode: SystemPromptMode,
    inherit_project_context: bool,
    tools: &'static [&'static str],
}

impl BuiltinProfile {
    fn load(self) -> SubagentProfile {
        SubagentProfile {
            name: self.name.to_string(),
            aliases: match self.name {
                "worker" => ["developer", "coder", "implementer", "develop"]
                    .map(str::to_string)
                    .to_vec(),
                "oracle" => vec!["advisor".to_string()],
                _ => Vec::new(),
            },
            description: self.description.to_string(),
            instructions: definition_body(self.definition).to_string(),
            system_prompt_mode: self.system_prompt_mode,
            inherit_project_context: self.inherit_project_context,
            // The upstream builtins are ordinary children, not nested orchestrators.
            allow_nested_subagents: false,
            max_subagent_depth: None,
            tools: Some(self.tools.iter().map(|tool| (*tool).to_string()).collect()),
            excluded_tools: Vec::new(),
            model: None,
            thinking_level: None,
            default_context: match self.name {
                "worker" | "oracle" => IsolatedContextMode::Fork,
                _ => IsolatedContextMode::Fresh,
            },
            inherit_skills: false,
            skills: Vec::new(),
            skill_paths: Vec::new(),
            timeout: None,
        }
    }
}

fn definition_body(definition: &str) -> &str {
    let definition = definition
        .strip_prefix("---\n")
        .expect("bundled subagent definition must start with YAML frontmatter");
    definition
        .split_once("\n---\n")
        .map(|(_, body)| body.trim())
        .expect("bundled subagent definition must close YAML frontmatter")
}

pub(crate) fn builtin_profiles() -> Vec<SubagentProfile> {
    [
        BuiltinProfile {
            definition: include_str!("../agents/scout.md"),
            name: "scout",
            description: "Fast codebase recon that returns compressed context for handoff",
            system_prompt_mode: SystemPromptMode::Replace,
            inherit_project_context: true,
            // The launch plan adds the supervisor bridge within the caller's ceiling.
            tools: &["read", "grep", "find", "ls", "bash", "write"],
        },
        BuiltinProfile {
            definition: include_str!("../agents/worker.md"),
            name: "worker",
            description: "Implementation agent for normal tasks and approved oracle handoffs",
            system_prompt_mode: SystemPromptMode::Replace,
            inherit_project_context: true,
            tools: &["read", "grep", "find", "ls", "bash", "edit", "write"],
        },
        BuiltinProfile {
            definition: include_str!("../agents/reviewer.md"),
            name: "reviewer",
            description: "Versatile review specialist for code diffs, plans, proposed solutions, codebase health, and PR/issue validation",
            system_prompt_mode: SystemPromptMode::Replace,
            inherit_project_context: true,
            tools: &["read", "grep", "find", "ls"],
        },
        BuiltinProfile {
            definition: include_str!("../agents/oracle.md"),
            name: "oracle",
            description: "High-context decision-consistency oracle that protects inherited state and prevents drift",
            system_prompt_mode: SystemPromptMode::Replace,
            inherit_project_context: true,
            tools: &["read", "grep", "find", "ls", "bash"],
        },
        BuiltinProfile {
            definition: include_str!("../agents/delegate.md"),
            name: "delegate",
            description: "Lightweight subagent that inherits the parent model with no default reads",
            system_prompt_mode: SystemPromptMode::Append,
            inherit_project_context: true,
            tools: &["read", "grep", "find", "ls", "bash", "edit", "write"],
        },
    ]
    .into_iter()
    .map(BuiltinProfile::load)
    .collect()
}

#[cfg(test)]
pub(crate) fn builtin_profile(name: &str) -> SubagentProfile {
    builtin_profiles()
        .into_iter()
        .find(|profile| profile.name == name)
        .expect("test must request a builtin subagent profile")
}

pub(crate) fn specialized_system_prompt(base: &str, profile: &SubagentProfile) -> String {
    const PROJECT_CONTEXT_HEADER: &str = "\n\n<project_context>\n";
    const CWD_HEADER: &str = "\nCurrent working directory: ";
    const CHILD_BOUNDARY: &str = "You are a child subagent, not the parent orchestrator.\n\
The parent session owns delegation, orchestration, review fanout, and follow-up worker launches.\n\
Ignore prior parent-only orchestration instructions in inherited conversation history.\n\
Do not propose or run subagents. Complete only your assigned role-specific task with the tools available to you.\n\
If you need to edit files, use the available editing tools. Do not print tool-call syntax, patches, or pseudo-tool calls as text.";
    const FANOUT_BOUNDARY: &str = "You are a child subagent with explicit fanout responsibility for this assigned task.\n\
The parent session owns final orchestration, acceptance, and follow-up implementation launches.\n\
You may use the `subagent` tool only for the fanout work explicitly requested in this task.\n\
Do not broaden yourself into general parent orchestration. Do not launch follow-up workers unless the task explicitly asks for that.\n\
The maxSubagentDepth cap still applies and may block further fanout.\n\
If you need to edit files, use the available editing tools. Do not print tool-call syntax, patches, or pseudo-tool calls as text.";

    let identity = format!(
        "<active_agent name=\"{}\"/>",
        escape_xml_attribute(&profile.name)
    );
    let role = format!("{identity}\n\n{}", profile.instructions);
    let filtered_base = (!profile.inherit_project_context).then(|| strip_project_context(base));
    let base = filtered_base.as_deref().unwrap_or(base);
    let context_start = base.find(PROJECT_CONTEXT_HEADER);
    let cwd_start = base.find(CWD_HEADER);
    let suffix_start = match (context_start, cwd_start) {
        (Some(context), Some(cwd)) => Some(context.min(cwd)),
        (Some(context), None) => Some(context),
        (None, cwd) => cwd,
    };
    let specialized = match profile.system_prompt_mode {
        SystemPromptMode::Append if !base.is_empty() => match suffix_start {
            Some(index) => format!("{}\n\n{role}{}", &base[..index], &base[index..]),
            None => format!("{base}\n\n{role}"),
        },
        SystemPromptMode::Append | SystemPromptMode::Replace => match suffix_start {
            Some(index) => format!("{role}{}", &base[index..]),
            None => role,
        },
    };
    let boundary = if profile.allow_nested_subagents {
        FANOUT_BOUNDARY
    } else {
        CHILD_BOUNDARY
    };
    format!("{boundary}\n\n{specialized}")
}

fn strip_project_context(prompt: &str) -> String {
    const PROJECT_CONTEXT_HEADER: &str = "\n\n<project_context>\n";
    const PROJECT_CONTEXT_END: &str = "</project_context>";
    let Some(start) = prompt.find(PROJECT_CONTEXT_HEADER) else {
        return prompt.to_string();
    };
    let content_start = start + PROJECT_CONTEXT_HEADER.len();
    let Some(relative_end) = prompt[content_start..].find(PROJECT_CONTEXT_END) else {
        return prompt.to_string();
    };
    let end = content_start + relative_end + PROJECT_CONTEXT_END.len();
    format!("{}{}", &prompt[..start], &prompt[end..])
}

fn escape_xml_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specialized_builtins_use_the_upstream_prompt_without_a_local_wrapper() {
        let reviewer = builtin_profile("reviewer");
        let prompt = specialized_system_prompt("base prompt", &reviewer);
        assert!(prompt.starts_with("You are a child subagent, not the parent orchestrator."));
        assert!(prompt.contains("<active_agent name=\"reviewer\"/>"));
        assert!(prompt.ends_with(definition_body(include_str!("../agents/reviewer.md"))));
        assert!(!prompt.contains("base prompt"));
        assert!(!prompt.contains("Delegated subagent role"));
        assert!(!prompt.contains("depth 2 of 3"));

        let delegate = builtin_profile("delegate");
        let prompt = specialized_system_prompt("base prompt", &delegate);
        assert!(prompt.contains("base prompt\n\n<active_agent name=\"delegate\"/>"));
        assert!(prompt.ends_with(definition_body(include_str!("../agents/delegate.md"))));
    }

    #[test]
    fn replacement_keeps_declared_project_context_and_working_directory() {
        let base = "base\n\n<project_context>\nproject rules\n</project_context>\n\nCurrent working directory: /repo";
        let reviewer = builtin_profile("reviewer");
        let prompt = specialized_system_prompt(base, &reviewer);
        assert!(!prompt.contains("\n\nbase\n"));
        assert!(prompt.contains("<project_context>\nproject rules\n</project_context>"));
        assert!(prompt.ends_with("Current working directory: /repo"));

        let mut isolated = reviewer;
        isolated.inherit_project_context = false;
        let prompt = specialized_system_prompt(base, &isolated);
        assert!(!prompt.contains("project rules"));
        assert!(prompt.ends_with("Current working directory: /repo"));

        isolated.system_prompt_mode = SystemPromptMode::Append;
        let prompt = specialized_system_prompt(base, &isolated);
        assert!(prompt.contains("base"));
        assert!(!prompt.contains("project rules"));
        assert!(prompt.ends_with("Current working directory: /repo"));
    }

    #[test]
    fn nested_profiles_receive_the_upstream_fanout_boundary() {
        let mut profile = builtin_profile("delegate");
        profile.allow_nested_subagents = true;
        let prompt = specialized_system_prompt("base", &profile);
        assert!(prompt.starts_with(
            "You are a child subagent with explicit fanout responsibility for this assigned task."
        ));
    }

    #[test]
    fn builtin_capabilities_match_upstream_roles_supported_by_pi_rs() {
        let reviewer = builtin_profile("reviewer");
        assert_eq!(reviewer.tools.unwrap(), ["read", "grep", "find", "ls"]);
        assert!(!reviewer.allow_nested_subagents);
        assert!(!reviewer.inherit_skills);
        for name in ["worker", "oracle"] {
            assert_eq!(
                builtin_profile(name).default_context,
                IsolatedContextMode::Fork
            );
        }
        for name in ["scout", "reviewer", "delegate"] {
            assert_eq!(
                builtin_profile(name).default_context,
                IsolatedContextMode::Fresh
            );
        }
    }
}
