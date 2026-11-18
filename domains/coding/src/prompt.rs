//! Coding system-prompt rendering and generation-local resource preparation.

#[cfg(test)]
mod oracle_tests;

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use pi_core::WorkspaceSnapshot;
use pi_runtime::{
    PreparedSystemPrompt, PromptContext, PromptOutput, SystemPrompt, SystemPromptFactory,
    SystemPromptRenderer,
};
use pi_utils::path::slash_path;
use serde::{Deserialize, Serialize};

use crate::resources::{ResourceLoaderOptions, load_resources};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ContextFile {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct BuildSystemPromptOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_system_prompt: Option<String>,
    pub selected_tools: Vec<String>,
    pub hidden_tools: Vec<String>,
    pub tool_snippets: BTreeMap<String, String>,
    pub tool_guidelines: BTreeMap<String, Vec<String>>,
    pub prompt_guidelines: Vec<String>,
    pub append_system_prompt: String,
    #[serde(with = "named_sections")]
    pub sections: Vec<(String, String)>,
    pub cwd: PathBuf,
    pub context_files: Vec<ContextFile>,
    pub skills: Vec<pi_plugin_skills::SkillPromptEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readme_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples_path: Option<PathBuf>,
}

impl Default for BuildSystemPromptOptions {
    fn default() -> Self {
        Self {
            custom_prompt: None,
            force_system_prompt: None,
            selected_tools: ["read", "bash", "edit", "write"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            hidden_tools: Vec::new(),
            tool_snippets: BTreeMap::new(),
            tool_guidelines: BTreeMap::new(),
            prompt_guidelines: Vec::new(),
            append_system_prompt: String::new(),
            sections: Vec::new(),
            cwd: PathBuf::new(),
            context_files: Vec::new(),
            skills: Vec::new(),
            readme_path: None,
            docs_path: None,
            examples_path: None,
        }
    }
}

pub(crate) fn build_system_prompt(options: &BuildSystemPromptOptions) -> Result<String, String> {
    if let Some(forced) = &options.force_system_prompt {
        return Ok(forced.clone());
    }
    Ok(build_system_prompt_sections(options)?
        .into_iter()
        .map(|(_, text)| text)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n"))
}

pub(crate) fn build_system_prompt_sections(
    options: &BuildSystemPromptOptions,
) -> Result<Vec<(String, String)>, String> {
    for (name, _) in &options.sections {
        let mut bytes = name.bytes();
        if name == "preamble"
            || !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
            || !bytes.all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
            })
        {
            return Err(format!("Invalid system prompt section name: {name}"));
        }
    }
    let declared = options
        .selected_tools
        .iter()
        .filter(|name| !options.hidden_tools.contains(name))
        .map(String::as_str)
        .collect::<Vec<_>>();
    let mut sections = Vec::new();
    if let Some(custom) = options
        .custom_prompt
        .as_ref()
        .filter(|text| !text.is_empty())
    {
        sections.push(("preamble".into(), custom.clone()));
    } else {
        sections.push((
            "preamble".into(),
            "You are a coding assistant running inside pi, a coding agent harness. Help users inspect files, run commands, and modify code.".into(),
        ));
        let tools = declared
            .iter()
            .filter_map(|name| {
                options
                    .tool_snippets
                    .get(*name)
                    .filter(|snippet| !snippet.is_empty())
                    .map(|snippet| format!("- {name}: {snippet}"))
            })
            .collect::<Vec<_>>();
        let tools = if tools.is_empty() {
            "(none)".to_string()
        } else {
            tools.join("\n")
        };
        sections.push((
            "tools".into(),
            format!("{tools}\n\nAdditional tools may be available."),
        ));
        sections.push(("rules".into(), build_rules(options, &declared)));
        sections.push(("docs".into(), documentation_prompt(options)));
    }
    if !options.append_system_prompt.is_empty() {
        sections.push(("addendum".into(), options.append_system_prompt.clone()));
    }
    if !options.context_files.is_empty() {
        sections.push((
            "project_context".into(),
            render_project_context(&options.context_files),
        ));
    }
    let skills = pi_plugin_skills::render_skill_prompt_options(
        &options.skills,
        &options.selected_tools,
        &options.hidden_tools,
    );
    if !skills.text.is_empty() {
        sections.push(("skills".into(), skills.text));
    }
    sections.push(("cwd".into(), slash_path(&options.cwd)));
    for (name, content) in &options.sections {
        if content.is_empty() {
            continue;
        }
        if let Some((_, value)) = sections.iter_mut().find(|(key, _)| key == name) {
            *value = content.clone();
        } else {
            sections.push((name.clone(), content.clone()));
        }
    }
    Ok(sections
        .into_iter()
        .map(|(name, content)| {
            let content = if name == "preamble" {
                content
            } else {
                format!("<{name}>\n{content}\n</{name}>")
            };
            (name, content)
        })
        .collect())
}

fn build_rules(options: &BuildSystemPromptOptions, selected: &[&str]) -> String {
    let mut guidelines = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |value: &str| {
        let value = value.trim();
        if !value.is_empty() && seen.insert(value.to_string()) {
            guidelines.push(value.to_string());
        }
    };
    let has_bash = selected.contains(&"bash");
    let has_powershell = selected.contains(&"powershell");
    if (has_bash || has_powershell)
        && !selected
            .iter()
            .any(|tool| matches!(*tool, "grep" | "find" | "ls"))
    {
        add(match (has_bash, has_powershell) {
            (true, true) => {
                "Use bash or PowerShell for file operations like listing, searching, and finding files"
            }
            (false, true) => {
                "Use PowerShell for file operations like listing, searching, and finding files"
            }
            _ => "Use bash for file operations like ls, rg, find",
        });
    }
    for name in selected {
        for guideline in options.tool_guidelines.get(*name).into_iter().flatten() {
            add(guideline);
        }
    }
    for guideline in &options.prompt_guidelines {
        add(guideline);
    }
    add("Be concise in your responses");
    add("Show file paths clearly when working with files");
    guidelines
        .into_iter()
        .map(|value| format!("- {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn documentation_prompt(options: &BuildSystemPromptOptions) -> String {
    let root = documentation_root();
    let readme = slash_path(
        options
            .readme_path
            .as_ref()
            .unwrap_or(&root.join("README.md")),
    );
    let docs = slash_path(options.docs_path.as_ref().unwrap_or(&root.join("docs")));
    let examples = slash_path(
        options
            .examples_path
            .as_ref()
            .unwrap_or(&root.join("examples")),
    );
    format!(
        "Pi documentation (read only when the user asks about the harness itself, its SDK, extensions, themes, skills, or TUI):\n- Main documentation: {readme}\n- Additional docs: {docs}\n- Examples: {examples} (extensions, custom tools, SDK)\n- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory\n- When asked about: extensions (docs/extensions.md, examples/extensions/), themes (docs/themes.md), skills (docs/skills.md), prompt templates (docs/prompt-templates.md), TUI components (docs/tui.md), keybindings (docs/keybindings.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md), adding models (docs/models.md), packages (docs/packages.md), environment variables (docs/environment-variables.md), MCP servers (docs/mcp.md), codemode scripts and non-LLM models such as classifiers and image models (docs/codemode.md)\n- Before implementing harness changes, read the relevant docs and examples in full, following related cross-references."
    )
}

fn render_project_context(files: &[ContextFile]) -> String {
    std::iter::once("Project-specific instructions and guidelines:".to_string())
        .chain(files.iter().map(|file| {
            format!(
                "<project_instructions path=\"{}\">\n{}\n</project_instructions>",
                slash_path(&file.path),
                file.content
            )
        }))
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(crate) fn documentation_root() -> PathBuf {
    let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/pi");
    let installed = std::env::current_exe().ok().and_then(|executable| {
        executable.parent().and_then(|directory| {
            [
                directory.join("share/pi-rs"),
                directory.join("../share/pi-rs"),
            ]
            .into_iter()
            .find(|path| path.join("README.md").is_file())
        })
    });
    let root = std::env::var_os("PI_DOCUMENTATION_ROOT")
        .map(PathBuf::from)
        .or(installed)
        .unwrap_or(source_root);
    std::fs::canonicalize(&root)
        .unwrap_or_else(|_| pi_utils::path::absolute_from_current_dir(&root))
}

mod named_sections {
    use serde::de::{MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        sections: &[(String, String)],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(sections.len()))?;
        for (name, text) in sections {
            map.serialize_entry(name, text)?;
        }
        map.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<(String, String)>, D::Error> {
        struct SectionVisitor;
        impl<'de> Visitor<'de> for SectionVisitor {
            type Value = Vec<(String, String)>;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object of named system prompt sections")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut sections = Vec::new();
                while let Some(section) = map.next_entry()? {
                    sections.push(section);
                }
                Ok(sections)
            }
        }
        deserializer.deserialize_map(SectionVisitor)
    }
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
        let documentation = documentation_root();
        options
            .readme_path
            .get_or_insert_with(|| documentation.join("README.md"));
        options
            .docs_path
            .get_or_insert_with(|| documentation.join("docs"));
        options
            .examples_path
            .get_or_insert_with(|| documentation.join("examples"));
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
        let contributions = options
            .context_files
            .iter()
            .map(|file| pi_core::PromptContribution {
                category: pi_core::ContextCategory::MemoryFiles,
                name: slash_path(&file.path),
                text: format!(
                    "<project_instructions path=\"{}\">\n{}\n</project_instructions>",
                    slash_path(&file.path),
                    file.content
                ),
            })
            .collect();
        let mut prepared = PreparedSystemPrompt::new(CodingPromptRenderer(options));
        prepared.prompt_contributions = contributions;
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
        options.selected_tools = context.selected_tools.to_vec();
        options.hidden_tools = context.hidden_tools.to_vec();
        options.tool_snippets.clear();
        options.tool_guidelines.clear();
        for spec in context.available_tools {
            if let Some(snippet) = &spec.prompt_snippet {
                let snippet = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
                if !snippet.is_empty() {
                    options.tool_snippets.insert(spec.name.clone(), snippet);
                }
            }
            options
                .tool_guidelines
                .insert(spec.name.clone(), spec.prompt_guidelines.clone());
        }
        render_options(options)
    }

    fn render_options(
        &self,
        context: PromptContext<'_>,
        options: &serde_json::Value,
    ) -> Result<PromptOutput, String> {
        let mut options: BuildSystemPromptOptions =
            serde_json::from_value(options.clone()).map_err(|error| error.to_string())?;
        options.hidden_tools = context.hidden_tools.to_vec();
        render_options(options)
    }

    fn render_selected_options(
        &self,
        context: PromptContext<'_>,
        options: &serde_json::Value,
    ) -> Result<PromptOutput, String> {
        let mut options: BuildSystemPromptOptions =
            serde_json::from_value(options.clone()).map_err(|error| error.to_string())?;
        options.selected_tools = context.selected_tools.to_vec();
        options.hidden_tools = context.hidden_tools.to_vec();
        render_options(options)
    }

    fn force_options(&self, options: &serde_json::Value, text: &str) -> Option<serde_json::Value> {
        let mut options = options.clone();
        options.as_object_mut()?.insert(
            "forceSystemPrompt".into(),
            serde_json::Value::String(text.into()),
        );
        Some(options)
    }
}

fn render_options(options: BuildSystemPromptOptions) -> Result<PromptOutput, String> {
    let skills = pi_plugin_skills::render_skill_prompt_options(
        &options.skills,
        &options.selected_tools,
        &options.hidden_tools,
    );
    Ok(PromptOutput {
        system_prompt: build_system_prompt(&options)?,
        sections: if options.force_system_prompt.is_some() {
            None
        } else {
            Some(build_system_prompt_sections(&options)?)
        },
        active_tools: Some(options.selected_tools.clone()),
        forced: options.force_system_prompt.is_some(),
        contributions: skills.contributions,
        options: Some(serde_json::to_value(options).map_err(|error| error.to_string())?),
    })
}

#[cfg(test)]
mod generation_tests {
    use pi_agent::AgentOptions;
    use pi_core::{Message, SystemMessage};
    use pi_runtime::PiRuntime;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    struct OptionsDispatcher(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl pi_js_plugin::JsCallbackDispatcher for OptionsDispatcher {
        async fn invoke(
            &self,
            invocation: pi_js_plugin::JsInvocation,
            _: pi_plugin::PluginContextHandle,
        ) -> Result<serde_json::Value, pi_js_plugin::JsCallbackError> {
            let mode = self.0.load(Ordering::SeqCst);
            let event = &invocation.payload["event"];
            assert_eq!(event["supportsSystemPromptOptions"], true);
            let mut options = event["systemPromptOptions"].clone();
            if invocation.callback_id == "first" {
                options["appendSystemPrompt"] = serde_json::json!("first addendum");
                if mode < 2 {
                    options["sections"]["extra"] = serde_json::json!("run section");
                } else {
                    options["selectedTools"] = serde_json::json!(["write", "missing", "write"]);
                    options["sections"] = serde_json::json!({});
                }
                return Ok(serde_json::json!({"__systemPromptOptions":options}));
            }
            let prompt = event["systemPrompt"].as_str().unwrap();
            assert!(prompt.contains("<addendum>\nfirst addendum\n</addendum>"));
            assert_eq!(prompt.contains("<skills>"), mode < 2);
            assert_eq!(
                event["activeTools"],
                if mode < 2 {
                    serde_json::json!(["read"])
                } else {
                    serde_json::json!(["write"])
                }
            );
            options["appendSystemPrompt"] = serde_json::json!("second addendum");
            if mode == 3 {
                Ok(
                    serde_json::json!({"__systemPromptOptions":options,"systemPrompt":"exact forced prompt"}),
                )
            } else {
                Ok(serde_json::json!({"__systemPromptOptions":options}))
            }
        }
    }

    fn options_runtime(
        stage: Arc<AtomicUsize>,
        cwd: &std::path::Path,
        provider: pi_test_support::ScriptedProviderPlugin,
    ) -> PiRuntime {
        let manifest = serde_json::from_value(serde_json::json!({
            "generationId":"prompt-options",
            "agentPlugins":[{"id":"prompt-options","hooks":[
                {"name":"before_agent_start","callbackId":"first"},
                {"name":"before_agent_start","callbackId":"second"}
            ]}]
        }))
        .unwrap();
        let generation =
            pi_js_plugin::JsPluginGeneration::prepare(manifest, Arc::new(OptionsDispatcher(stage)))
                .unwrap();
        let mut builder = PiRuntime::builder()
            .provider_plugin(provider)
            .plugin(pi_plugin_read::ReadPlugin)
            .plugin(pi_plugin_write::WritePlugin)
            .plugin(pi_plugin_skills::SkillsPlugin::from_skills([
                pi_plugin_skills::SkillInfo {
                    name: "inspect".into(),
                    description: "Inspect the project".into(),
                    file_path: cwd.join("inspect/SKILL.md"),
                    content: "body".into(),
                    disable_model_invocation: false,
                },
            ]))
            .agent_options(AgentOptions {
                cwd: cwd.into(),
                active_tools: vec!["read".into()],
                ..Default::default()
            })
            .system_prompt(CodingSystemPrompt::default().into());
        for plugin in generation.plugins() {
            builder = builder.plugin_arc(plugin);
        }
        builder.build().unwrap()
    }

    #[tokio::test]
    async fn mutable_hook_options_persist_section_deltas_and_resume_without_repeating_them() {
        use pi_session::{AgentSession, AgentSessionOptions, CompactionSettings};
        use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let stage = Arc::new(AtomicUsize::new(0));
        let plugin =
            ScriptedProviderPlugin::scripted((0..5).map(|_| ScriptedTurn::Text("done".into())));
        let provider = plugin.provider();
        let runtime = options_runtime(Arc::clone(&stage), directory.path(), plugin);
        let options = || {
            AgentSessionOptions::default().compaction(CompactionSettings {
                enabled: false,
                ..Default::default()
            })
        };
        let session = AgentSession::create(runtime, &path, options())
            .await
            .unwrap();
        assert!(!path.exists());
        session.prompt("first").await.unwrap();
        assert!(path.exists());
        let first = session.snapshot().agent.messages;
        assert!(matches!(first.first(), Some(Message::System(_))));
        assert!(matches!(first.get(1), Some(Message::User(_))));
        let system = first
            .iter()
            .filter(|message| matches!(message, Message::System(_)))
            .collect::<Vec<_>>();
        assert_eq!(system.len(), 1);
        let Message::System(system) = system[0] else {
            unreachable!()
        };
        assert!(system.sections.iter().any(|(name, _)| name == "skills"));
        assert!(SystemMessage::fold("", &first).contains("second addendum"));
        assert!(!SystemMessage::fold("", &first).contains("first addendum"));
        stage.store(1, Ordering::SeqCst);
        session.prompt("unchanged").await.unwrap();
        assert_eq!(
            session
                .snapshot()
                .agent
                .messages
                .iter()
                .filter(|message| matches!(message, Message::System(_)))
                .count(),
            1
        );

        stage.store(2, Ordering::SeqCst);
        session.prompt("change selection").await.unwrap();
        let snapshot = session.snapshot();
        assert_eq!(snapshot.agent.active_tools, ["write"]);
        let last = snapshot
            .agent
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::System(message) => Some(message),
                _ => None,
            })
            .unwrap();
        assert!(last.sections.contains(&("skills".into(), None)));
        assert!(last.sections.contains(&("extra".into(), None)));
        assert!(
            !last
                .sections
                .iter()
                .any(|(name, _)| name == "docs" || name == "preamble")
        );
        let request = provider.requests().last().unwrap().clone();
        assert_eq!(
            request
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["write"]
        );
        assert!(
            !SystemMessage::fold(&request.system_prompt, &request.messages).contains("<skills>")
        );

        stage.store(3, Ordering::SeqCst);
        session.prompt("force").await.unwrap();
        let request = provider.requests().last().unwrap().clone();
        assert_eq!(request.system_prompt, "exact forced prompt");
        assert!(
            !request
                .messages
                .iter()
                .any(|message| matches!(message, Message::System(_)))
        );
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("exact forced prompt")
        );
        session.shutdown().await;

        stage.store(2, Ordering::SeqCst);
        let plugin = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("resumed".into())]);
        let provider = plugin.provider();
        let resumed = AgentSession::open(
            options_runtime(stage, directory.path(), plugin),
            &path,
            options(),
        )
        .await
        .unwrap();
        assert_eq!(resumed.snapshot().agent.active_tools, ["write"]);
        let before = resumed
            .snapshot()
            .agent
            .messages
            .iter()
            .filter(|message| matches!(message, Message::System(_)))
            .count();
        resumed.prompt("resume").await.unwrap();
        assert_eq!(
            resumed
                .snapshot()
                .agent
                .messages
                .iter()
                .filter(|message| matches!(message, Message::System(_)))
                .count(),
            before
        );
        let request = provider.requests().pop().unwrap();
        assert!(request.system_prompt.is_empty());
        assert!(SystemMessage::fold("", &request.messages).contains("second addendum"));
        resumed.shutdown().await;
    }

    #[tokio::test]
    async fn hidden_read_declaration_uses_indirect_skill_instructions() {
        use pi_plugin_codemode::{CodemodeMode, CodemodeOptions, ConfiguredCodemodePlugin};
        use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

        let directory = tempfile::tempdir().unwrap();
        let plugin = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".into())]);
        let provider = plugin.provider();
        let runtime = PiRuntime::builder()
            .provider_plugin(plugin)
            .plugin(pi_plugin_read::ReadPlugin)
            .plugin(ConfiguredCodemodePlugin::new(CodemodeOptions {
                mode: CodemodeMode::Only,
                ..Default::default()
            }))
            .plugin(pi_plugin_skills::SkillsPlugin::from_skills([
                pi_plugin_skills::SkillInfo {
                    name: "inspect".into(),
                    description: "Inspect the project".into(),
                    file_path: directory.path().join("inspect/SKILL.md"),
                    content: "body".into(),
                    disable_model_invocation: false,
                },
            ]))
            .agent_options(AgentOptions {
                cwd: directory.path().into(),
                active_tools: vec!["read".into(), "codemode".into()],
                ..Default::default()
            })
            .system_prompt(CodingSystemPrompt::default().into())
            .build()
            .unwrap();
        assert_eq!(
            runtime.prompt_options().unwrap()["hiddenTools"],
            serde_json::json!(["read"])
        );

        runtime.prompt("inspect").await.unwrap();

        let request = provider.requests().pop().unwrap();
        assert_eq!(
            request
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["codemode"]
        );
        let prompt = SystemMessage::fold(&request.system_prompt, &request.messages);
        assert!(prompt.contains("<skills>"));
        assert!(prompt.contains("Load a skill's file when the task matches its description."));
        assert!(!prompt.contains("Use the read tool to load a skill's file"));
        assert!(!prompt.contains("Use read to examine files instead of cat or sed."));
        assert!(!prompt.contains("- read: Read file contents"));
        assert!(
            request.tools[0]
                .description
                .contains("declare const tools: { read(")
        );
        assert_eq!(runtime.agent().state().active_tools, ["read", "codemode"]);
    }

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
            .provider_plugin(pi_test_support::ScriptedProviderPlugin::scripted([
                pi_test_support::ScriptedTurn::Text("done".into()),
            ]))
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
        assert!(prompt.starts_with("You are a coding assistant running inside pi"));
        assert!(prompt.contains("- read: Read file contents"));
        assert!(prompt.contains("Use read to examine files instead of cat or sed."));
        assert!(!prompt.contains("- write: Create or overwrite files"));
        assert!(prompt.contains("original project rules"));
        assert!(prompt.contains("global addendum"));
        assert!(!prompt.contains("untrusted project prompt"));
        assert!(prompt.contains(&format!("<cwd>\n{}\n</cwd>", cwd.display())));
        let options = runtime.prompt_options().unwrap();
        assert_eq!(options["selectedTools"], serde_json::json!(["read"]));
        assert_eq!(options["cwd"], serde_json::json!(cwd));
        let mut edited_options = options;
        edited_options["selectedTools"] = serde_json::json!(["write", "read", "write", "missing"]);
        let edited_prompt = runtime
            .agent()
            .render_system_prompt_options(edited_options)
            .unwrap();
        assert!(edited_prompt.find("- read:").unwrap() < edited_prompt.find("- write:").unwrap());
        assert_eq!(edited_prompt.matches("- write:").count(), 1);
        assert_eq!(runtime.active_tools(), ["read"]);
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
        assert!(runtime.agent().context_request_usage().is_none());
        runtime.prompt("inspect context").await.unwrap();
        let usage = runtime.agent().context_request_usage().unwrap();
        let memory = usage
            .categories
            .iter()
            .find(|part| part.category == pi_core::ContextCategory::MemoryFiles)
            .unwrap();
        assert!(
            memory
                .items
                .iter()
                .any(|item| item.name.ends_with("AGENTS.md") && item.tokens > 0)
        );
    }
}

#[cfg(test)]
mod assembly_tests {
    use super::*;

    #[test]
    fn default_prompt_has_ordered_current_pi_sections() {
        let options = BuildSystemPromptOptions {
            selected_tools: vec!["read".into(), "bash".into()],
            tool_snippets: BTreeMap::from([
                ("read".into(), "Read file contents".into()),
                ("bash".into(), "Execute bash commands".into()),
            ]),
            tool_guidelines: BTreeMap::from([
                ("read".into(), vec!["Use read first.".into()]),
                ("bash".into(), vec!["Use bash safely.".into()]),
            ]),
            cwd: "/project".into(),
            append_system_prompt: "Additional instructions".into(),
            context_files: vec![ContextFile {
                path: "/project/AGENTS.md".into(),
                content: "Repository rules".into(),
            }],
            readme_path: Some("/pi/README.md".into()),
            docs_path: Some("/pi/docs".into()),
            examples_path: Some("/pi/examples".into()),
            ..Default::default()
        };
        let sections = build_system_prompt_sections(&options).unwrap();
        assert_eq!(
            sections
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            [
                "preamble",
                "tools",
                "rules",
                "docs",
                "addendum",
                "project_context",
                "cwd"
            ]
        );
        assert_eq!(
            sections[0].1,
            "You are a coding assistant running inside pi, a coding agent harness. Help users inspect files, run commands, and modify code."
        );
        assert_eq!(
            sections[1].1,
            "<tools>\n- read: Read file contents\n- bash: Execute bash commands\n\nAdditional tools may be available.\n</tools>"
        );
        assert_eq!(
            sections[2].1,
            "<rules>\n- Use bash for file operations like ls, rg, find\n- Use read first.\n- Use bash safely.\n- Be concise in your responses\n- Show file paths clearly when working with files\n</rules>"
        );
        assert!(
            sections[3]
                .1
                .contains("MCP servers (docs/mcp.md), codemode scripts")
        );
        assert!(
            sections[3]
                .1
                .contains("- Main documentation: /pi/README.md")
        );
        assert_eq!(
            sections[5].1,
            "<project_context>\nProject-specific instructions and guidelines:\n\n<project_instructions path=\"/project/AGENTS.md\">\nRepository rules\n</project_instructions>\n</project_context>"
        );
        assert_eq!(
            build_system_prompt(&options).unwrap(),
            sections
                .into_iter()
                .map(|(_, text)| text)
                .collect::<Vec<_>>()
                .join("\n\n")
        );
    }

    #[test]
    fn hidden_tools_leave_neither_snippets_nor_guidelines() {
        let options = BuildSystemPromptOptions {
            selected_tools: vec!["read".into(), "bash".into()],
            hidden_tools: vec!["read".into()],
            tool_snippets: BTreeMap::from([
                ("read".into(), "read snippet".into()),
                ("bash".into(), "bash snippet".into()),
            ]),
            tool_guidelines: BTreeMap::from([
                ("read".into(), vec!["read-specific rule".into()]),
                ("bash".into(), vec!["bash-specific rule".into()]),
            ]),
            ..Default::default()
        };
        let prompt = build_system_prompt(&options).unwrap();
        assert!(!prompt.contains("read snippet"));
        assert!(!prompt.contains("read-specific rule"));
        assert!(prompt.contains("bash snippet"));
        assert!(prompt.contains("bash-specific rule"));
    }

    #[test]
    fn section_overrides_preserve_builtin_positions_and_validate_names() {
        let mut options: BuildSystemPromptOptions = serde_json::from_str(
            r#"{
            "customPrompt":"custom", "cwd":"/project",
            "sections":{"z_extra":"first","cwd":"overridden","a_extra":"last","ignored":""}
        }"#,
        )
        .unwrap();
        let sections = build_system_prompt_sections(&options).unwrap();
        assert_eq!(
            sections
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["preamble", "cwd", "z_extra", "a_extra"]
        );
        assert_eq!(sections[1].1, "<cwd>\noverridden\n</cwd>");
        for name in ["preamble", "", "Uppercase", "a/b", "a>injected", "1first"] {
            options.sections = vec![(name.into(), "value".into())];
            assert_eq!(
                build_system_prompt(&options),
                Err(format!("Invalid system prompt section name: {name}"))
            );
        }
        options.force_system_prompt = Some(String::new());
        assert_eq!(build_system_prompt(&options).unwrap(), "");
    }

    #[test]
    fn powershell_guidance_tracks_the_selected_shells() {
        for (tools, expected) in [
            (
                vec!["powershell"],
                "Use PowerShell for file operations like listing, searching, and finding files",
            ),
            (
                vec!["bash", "powershell"],
                "Use bash or PowerShell for file operations like listing, searching, and finding files",
            ),
        ] {
            let prompt = build_system_prompt(&BuildSystemPromptOptions {
                selected_tools: tools.into_iter().map(str::to_string).collect(),
                ..Default::default()
            })
            .unwrap();
            assert!(prompt.contains(expected));
        }
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: vec!["powershell".into(), "find".into()],
            ..Default::default()
        })
        .unwrap();
        assert!(!prompt.contains("file operations like"));
    }

    #[test]
    fn custom_prompt_still_appends_context_and_cwd() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            custom_prompt: Some("custom".into()),
            selected_tools: vec!["read".into()],
            append_system_prompt: "append".into(),
            cwd: "/tmp/project".into(),
            context_files: vec![ContextFile {
                path: "AGENTS.md".into(),
                content: "rules".into(),
            }],
            ..Default::default()
        })
        .unwrap();
        assert!(prompt.starts_with("custom\n\n<addendum>\nappend\n</addendum>"));
        assert!(prompt.contains("<project_context>"));
        assert!(prompt.ends_with("<cwd>\n/tmp/project\n</cwd>"));
    }
    #[test]
    fn guidelines_are_ordered_and_deduplicated() {
        let prompt = build_system_prompt(&BuildSystemPromptOptions {
            selected_tools: vec!["bash".into()],
            prompt_guidelines: vec!["x".into(), "x".into()],
            cwd: ".".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(prompt.matches("- x").count(), 1);
        assert!(prompt.find("Use bash for file operations").unwrap() < prompt.find("- x").unwrap());
    }
}
