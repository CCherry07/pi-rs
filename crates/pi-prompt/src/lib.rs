#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextFile {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BuildSystemPromptOptions {
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

pub fn build_system_prompt(options: &BuildSystemPromptOptions) -> String {
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

fn slash_path(path: impl AsRef<std::path::Path>) -> String {
    path.as_ref().to_string_lossy().replace('\\', "/")
}
fn escape_xml(value: &str) -> String {
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
        println!("{}", prompt)
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
