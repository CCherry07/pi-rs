//! Coding system-prompt rendering and generation-local resource preparation.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use pi_core::WorkspaceSnapshot;
use pi_runtime::{
    PreparedSystemPrompt, PromptContext, PromptOutput, SystemPrompt, SystemPromptFactory,
    SystemPromptRenderer,
};
use pi_utils::{path::slash_path, text::escape_xml};
use serde::{Deserialize, Serialize};

use crate::resources::{ResourceLoaderOptions, load_resources};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ContextFile {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct BuildSystemPromptOptions {
    pub custom_prompt: Option<String>,
    pub selected_tools: Vec<String>,
    pub tool_snippets: BTreeMap<String, String>,
    pub prompt_guidelines: Vec<String>,
    pub append_system_prompt: Option<String>,
    pub cwd: PathBuf,
    pub context_files: Vec<ContextFile>,
    pub readme_path: Option<PathBuf>,
    pub docs_path: Option<PathBuf>,
    pub examples_path: Option<PathBuf>,
}

pub(crate) fn build_system_prompt(options: &BuildSystemPromptOptions) -> String {
    let cwd = slash_path(&options.cwd);
    let append = options
        .append_system_prompt
        .as_deref()
        .filter(|value| !value.is_empty());
    let custom_prompt = options
        .custom_prompt
        .as_deref()
        .filter(|value| !value.is_empty());
    let mut prompt = if let Some(custom) = custom_prompt {
        custom.to_string()
    } else {
        default_prompt(options)
    };
    if let Some(append) = append {
        prompt.push_str("\n\n");
        prompt.push_str(append);
    }
    append_context(&mut prompt, &options.context_files);
    prompt.push_str("\nCurrent working directory: ");
    prompt.push_str(&cwd);
    if custom_prompt.is_some() {
        prompt.push('\n');
    }
    prompt
}

fn default_prompt(options: &BuildSystemPromptOptions) -> String {
    let tools = options
        .selected_tools
        .iter()
        .filter_map(|name| {
            options
                .tool_snippets
                .get(name)
                .map(|snippet| format!("- {name}: {snippet}"))
        })
        .collect::<Vec<_>>();
    let tools = if tools.is_empty() {
        "(none)".to_string()
    } else {
        tools.join("\n")
    };
    let selected = &options.selected_tools;
    let mut guidelines = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |value: &str| {
        let value = value.trim();
        if !value.is_empty() && seen.insert(value.to_string()) {
            guidelines.push(value.to_string());
        }
    };
    if selected.iter().any(|tool| tool == "bash")
        && !selected
            .iter()
            .any(|tool| matches!(tool.as_str(), "grep" | "find" | "ls"))
    {
        add("Use bash for file operations like ls, rg, find");
    }
    for guideline in &options.prompt_guidelines {
        add(guideline);
    }
    add("Be concise in your responses");
    add("Show file paths clearly when working with files");
    let guidelines = guidelines
        .into_iter()
        .map(|value| format!("- {value}"))
        .collect::<Vec<_>>()
        .join("\n");
    let readme = slash_path(
        options
            .readme_path
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new("README.md")),
    );
    let docs = slash_path(
        options
            .docs_path
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new("docs")),
    );
    let examples = slash_path(
        options
            .examples_path
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new("examples")),
    );
    format!(
        "You are an expert coding assistant operating inside pi, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.\n\nAvailable tools:\n{tools}\n\nIn addition to the tools above, you may have access to other custom tools depending on the project.\n\nGuidelines:\n{guidelines}\n\nPi documentation (read only when the user asks about pi itself, its SDK, extensions, themes, skills, or TUI):\n- Main documentation: {readme}\n- Additional docs: {docs}\n- Examples: {examples} (extensions, custom tools, SDK)\n- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory\n- When asked about: extensions (docs/extensions.md, examples/extensions/), themes (docs/themes.md), skills (docs/skills.md), prompt templates (docs/prompt-templates.md), TUI components (docs/tui.md), keybindings (docs/keybindings.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md), adding models (docs/models.md), pi packages (docs/packages.md), environment variables (docs/environment-variables.md)\n- When working on pi topics, read the docs and examples, and follow .md cross-references before implementing\n- Always read pi .md files completely and follow links to related docs (e.g., tui.md for TUI API details)"
    )
}

fn append_context(prompt: &mut String, files: &[ContextFile]) {
    if files.is_empty() {
        return;
    }
    prompt.push_str("\n\n<project_context>\n\nProject-specific instructions and guidelines:\n\n");
    for file in files {
        prompt.push_str(&format!(
            "<project_instructions path=\"{}\">\n{}\n</project_instructions>\n\n",
            escape_xml(&slash_path(&file.path)),
            file.content
        ));
    }
    prompt.push_str("</project_context>\n");
}

/// Coding product adapter for Pi prompt assembly and project resource discovery.
/// The runtime only sees the prepared renderer and its serialized inspection data.
#[derive(Debug, Clone, Default)]
pub(crate) struct CodingSystemPrompt {
    options: BuildSystemPromptOptions,
    resources: Option<ResourceLoaderOptions>,
}

impl CodingSystemPrompt {
    pub(crate) fn new(options: BuildSystemPromptOptions) -> Self {
        Self {
            options,
            resources: None,
        }
    }

    pub(crate) fn resources(mut self, resources: ResourceLoaderOptions) -> Self {
        self.resources = Some(resources);
        self
    }
}

impl From<CodingSystemPrompt> for SystemPrompt {
    fn from(prompt: CodingSystemPrompt) -> Self {
        Self::dynamic(prompt)
    }
}

impl SystemPromptFactory for CodingSystemPrompt {
    fn prepare(&self, workspace: &WorkspaceSnapshot) -> Result<PreparedSystemPrompt, String> {
        let mut options = self.options.clone();
        options.cwd = workspace.cwd().to_path_buf();
        let mut diagnostics = Vec::new();
        let resource_options = if let Some(mut resources) = self.resources.clone() {
            resources.cwd = workspace.cwd().to_path_buf();
            let loaded = load_resources(&resources);
            diagnostics.extend(loaded.diagnostics.clone());
            loaded.apply_to_prompt(&mut options);
            Some(serde_json::to_value(resources).map_err(|error| error.to_string())?)
        } else {
            None
        };
        let mut prepared = PreparedSystemPrompt::new(CodingPromptRenderer(options));
        prepared.resource_options = resource_options;
        prepared.diagnostics = diagnostics;
        Ok(prepared)
    }
}

struct CodingPromptRenderer(BuildSystemPromptOptions);

impl SystemPromptRenderer for CodingPromptRenderer {
    fn render(&self, context: PromptContext<'_>) -> Result<PromptOutput, String> {
        let mut options = self.0.clone();
        options.cwd = context.workspace.cwd().to_path_buf();
        options.selected_tools.clear();
        options.tool_snippets.clear();
        options.prompt_guidelines.clear();
        for spec in context.active_tools {
            options.selected_tools.push(spec.name.clone());
            if let Some(snippet) = &spec.prompt_snippet {
                options.tool_snippets.insert(
                    spec.name.clone(),
                    snippet.split_whitespace().collect::<Vec<_>>().join(" "),
                );
            }
            options
                .prompt_guidelines
                .extend(spec.prompt_guidelines.clone());
        }
        Ok(PromptOutput {
            system_prompt: build_system_prompt(&options),
            options: Some(serde_json::to_value(options).map_err(|error| error.to_string())?),
        })
    }
}

#[cfg(test)]
mod generation_tests {
    use pi_agent::AgentOptions;
    use pi_runtime::PiRuntime;

    use super::*;

    #[tokio::test]
    async fn resources_are_frozen_until_reload_and_prompt_options_follow_active_tools() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let agent_dir = root.path().join("agent");
        std::fs::create_dir_all(cwd.join(".pi")).unwrap();
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(cwd.join("AGENTS.md"), "original project rules").unwrap();
        std::fs::write(cwd.join(".pi/SYSTEM.md"), "untrusted project prompt").unwrap();
        std::fs::write(agent_dir.join("APPEND_SYSTEM.md"), "global addendum").unwrap();
        let runtime = PiRuntime::builder()
            .provider_plugin(pi_test_support::ScriptedProviderPlugin::scripted([]))
            .plugin(pi_plugin_read::ReadPlugin)
            .plugin(pi_plugin_write::WritePlugin)
            .agent_options(AgentOptions {
                active_tools: vec!["read".to_string()],
                cwd: cwd.clone(),
                ..AgentOptions::default()
            })
            .system_prompt(
                CodingSystemPrompt::default()
                    .resources(ResourceLoaderOptions::new("ignored", &agent_dir))
                    .into(),
            )
            .build()
            .unwrap();

        let prompt = runtime.agent().state().system_prompt;
        assert!(prompt.starts_with("You are an expert coding assistant"));
        assert!(prompt.contains("- read: Read file contents"));
        assert!(prompt.contains("Use read to examine files instead of cat or sed."));
        assert!(!prompt.contains("- write: Create or overwrite files"));
        assert!(prompt.contains("original project rules"));
        assert!(prompt.contains("global addendum"));
        assert!(!prompt.contains("untrusted project prompt"));
        assert!(prompt.contains(&format!("Current working directory: {}", cwd.display())));
        let options = runtime.prompt_options().unwrap();
        assert_eq!(options["selectedTools"], serde_json::json!(["read"]));
        assert_eq!(options["cwd"], serde_json::json!(cwd));
        assert_eq!(
            runtime.resource_options().unwrap()["cwd"],
            serde_json::json!(cwd)
        );
        assert!(runtime.resource_diagnostics().is_empty());

        std::fs::write(cwd.join("AGENTS.md"), "updated project rules").unwrap();
        runtime.set_active_tools(["write"]).unwrap();
        let prompt = runtime.agent().state().system_prompt;
        assert!(prompt.contains("- write: Create or overwrite files"));
        assert!(!prompt.contains("- read: Read file contents"));
        assert!(prompt.contains("original project rules"));
        assert!(!prompt.contains("updated project rules"));
        assert_eq!(
            runtime.prompt_options().unwrap()["selectedTools"],
            serde_json::json!(["write"])
        );

        runtime.reload().await.unwrap();
        let prompt = runtime.agent().state().system_prompt;
        assert!(prompt.contains("updated project rules"));
        assert!(!prompt.contains("original project rules"));
        assert!(prompt.contains("- write: Create or overwrite files"));
    }
}

#[cfg(test)]
mod assembly_tests {
    use super::*;
    #[test]
    fn custom_prompt_still_appends_context_and_cwd() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            custom_prompt: Some("custom".into()),
            selected_tools: vec!["read".into()],
            append_system_prompt: Some("append".into()),
            cwd: "/tmp/project".into(),
            context_files: vec![ContextFile {
                path: "AGENTS.md".into(),
                content: "rules".into(),
            }],
            ..Default::default()
        });
        assert!(prompt.starts_with("custom\n\nappend"));
        assert!(prompt.contains("<project_context>"));
        assert!(prompt.ends_with("Current working directory: /tmp/project\n"));
    }
    #[test]
    fn guidelines_are_ordered_and_deduplicated() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: vec!["bash".into()],
            prompt_guidelines: vec!["x".into(), "x".into()],
            cwd: ".".into(),
            ..Default::default()
        });
        assert_eq!(prompt.matches("- x").count(), 1);
        assert!(prompt.find("Use bash for file operations").unwrap() < prompt.find("- x").unwrap());
    }
}
