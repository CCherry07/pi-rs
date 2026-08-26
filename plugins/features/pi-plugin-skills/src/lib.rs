#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    AgentPlugin, BeforeAgentStartEvent, BeforeAgentStartPatch, Command, CommandContext,
    CommandError, CommandOutcome, CommandSpec, PluginContext, PluginError, PluginId,
    RegisterContext,
};
use serde::{Deserialize, Serialize};

const CONFIG_DIR_NAME: &str = ".pi";
const AGENTS_DIR_NAME: &str = ".agents";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillLoaderOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub additional_paths: Vec<PathBuf>,
    pub include_defaults: bool,
    #[serde(default = "default_project_trusted")]
    pub project_trusted: bool,
}

fn default_project_trusted() -> bool {
    true
}

impl SkillLoaderOptions {
    pub fn new(cwd: impl Into<PathBuf>, agent_dir: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            agent_dir: agent_dir.into(),
            additional_paths: Vec::new(),
            include_defaults: true,
            project_trusted: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub file_path: PathBuf,
    pub content: String,
    pub disable_model_invocation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillDiagnosticKind {
    Warning,
    Collision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDiagnostic {
    pub kind: SkillDiagnosticKind,
    pub message: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSourceInput<T> {
    pub path: PathBuf,
    pub source: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedSkill<T> {
    pub skill: SkillInfo,
    pub source: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedSkillDiagnostic<T> {
    pub kind: SkillDiagnosticKind,
    pub message: String,
    pub path: PathBuf,
    pub source: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedSkillCatalog<T> {
    pub skills: Vec<SourcedSkill<T>>,
    pub diagnostics: Vec<SourcedSkillDiagnostic<T>>,
}

/// A generation-local, immutable catalog of skills.
///
/// Construct this plugin through `PiRuntimeBuilder::agent_plugin_factory` so every
/// runtime reload rescans the configured roots and publishes a new catalog as
/// part of the next runtime generation.
pub struct SkillsPlugin {
    skills: Vec<SkillInfo>,
    diagnostics: Vec<SkillDiagnostic>,
}

impl SkillsPlugin {
    pub fn new(options: SkillLoaderOptions) -> Self {
        Self::load(options)
    }

    pub fn load(options: SkillLoaderOptions) -> Self {
        let cwd = absolute(&options.cwd);
        let agent_dir = absolute(&options.agent_dir);
        let (skills, diagnostics) = load_skills(
            &cwd,
            &agent_dir,
            &options.additional_paths,
            options.include_defaults,
            options.project_trusted,
        );
        Self {
            skills,
            diagnostics,
        }
    }

    pub fn from_skills(skills: impl IntoIterator<Item = SkillInfo>) -> Self {
        Self {
            skills: skills.into_iter().collect(),
            diagnostics: Vec::new(),
        }
    }

    pub fn skills(&self) -> &[SkillInfo] {
        &self.skills
    }

    pub fn diagnostics(&self) -> &[SkillDiagnostic] {
        &self.diagnostics
    }
}

/// Loads source-tagged roots while preserving each caller-owned source value
/// exactly on both skills and diagnostics.
pub fn load_sourced_skills<T: Clone>(
    inputs: impl IntoIterator<Item = SkillSourceInput<T>>,
) -> SourcedSkillCatalog<T> {
    let mut sourced = SourcedSkillCatalog {
        skills: Vec::new(),
        diagnostics: Vec::new(),
    };
    for input in inputs {
        let mut skills = Vec::new();
        let mut diagnostics = Vec::new();
        let mut names = HashMap::new();
        let mut real_paths = HashSet::new();
        collect_skill_root(
            &absolute(&input.path),
            &mut skills,
            &mut diagnostics,
            &mut names,
            &mut real_paths,
        );
        sourced
            .skills
            .extend(skills.into_iter().map(|skill| SourcedSkill {
                skill,
                source: input.source.clone(),
            }));
        sourced
            .diagnostics
            .extend(
                diagnostics
                    .into_iter()
                    .map(|diagnostic| SourcedSkillDiagnostic {
                        kind: diagnostic.kind,
                        message: diagnostic.message,
                        path: diagnostic.path,
                        source: input.source.clone(),
                    }),
            );
    }
    sourced
}

struct SkillCommand {
    skill: SkillInfo,
}

#[async_trait]
impl Command for SkillCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: format!("skill:{}", self.skill.name),
            description: self.skill.description.clone(),
            argument_hint: Some("[task]".to_string()),
        }
    }

    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        if context.abort_signal.is_aborted() {
            return Err(CommandError::Aborted);
        }
        Ok(CommandOutcome::TransformInput(render_skill_invocation(
            &self.skill,
            &arguments,
        )))
    }
}

fn render_skill_invocation(skill: &SkillInfo, arguments: &str) -> String {
    let base_dir = skill
        .file_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name,
        slash_path(&skill.file_path),
        slash_path(base_dir),
        skill.content
    );
    if arguments.is_empty() {
        block
    } else {
        format!("{block}\n\n{arguments}")
    }
}

#[pi_core::agent_plugin]
impl AgentPlugin for SkillsPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("skills")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        for skill in &self.skills {
            context.register_command(Arc::new(SkillCommand {
                skill: skill.clone(),
            }))?;
        }
        Ok(())
    }

    async fn before_agent_start(
        &self,
        _context: PluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        if !event.active_tools.iter().any(|tool| tool == "read") {
            return Ok(BeforeAgentStartPatch::default());
        }
        let catalog = format_skills_for_prompt(&self.skills);
        if catalog.is_empty() {
            return Ok(BeforeAgentStartPatch::default());
        }
        Ok(BeforeAgentStartPatch {
            system_prompt: Some(format!("{}{catalog}", event.system_prompt)),
            messages: Vec::new(),
        })
    }
}

fn load_skills(
    cwd: &Path,
    agent_dir: &Path,
    extra: &[PathBuf],
    defaults: bool,
    project_trusted: bool,
) -> (Vec<SkillInfo>, Vec<SkillDiagnostic>) {
    let mut roots = Vec::new();
    if defaults {
        if project_trusted {
            roots.push(cwd.join(CONFIG_DIR_NAME).join("skills"));
            roots.extend(ancestor_agent_skill_dirs(cwd));
        }
        roots.push(agent_dir.join("skills"));
    }
    roots.extend(extra.iter().map(|path| {
        if path.is_absolute() {
            path.clone()
        } else {
            cwd.join(path)
        }
    }));

    let mut skills = Vec::new();
    let mut diagnostics = Vec::new();
    let mut names = HashMap::<String, PathBuf>::new();
    let mut real_paths = HashSet::new();
    for root in roots {
        collect_skill_root(
            &root,
            &mut skills,
            &mut diagnostics,
            &mut names,
            &mut real_paths,
        );
    }
    (skills, diagnostics)
}

fn collect_skill_root(
    root: &Path,
    skills: &mut Vec<SkillInfo>,
    diagnostics: &mut Vec<SkillDiagnostic>,
    names: &mut HashMap<String, PathBuf>,
    real_paths: &mut HashSet<PathBuf>,
) {
    let files = if root.is_file() {
        vec![root.to_path_buf()]
    } else {
        discover_skill_files(root, true)
    };
    for file in files {
        let canonical = std::fs::canonicalize(&file).unwrap_or_else(|_| absolute(&file));
        if !real_paths.insert(canonical) {
            continue;
        }
        let declared = file.file_name().is_some_and(|name| name == "SKILL.md");
        match load_skill(&file, declared) {
            Ok(Some(skill)) => {
                if let Some(winner) = names.get(&skill.name) {
                    diagnostics.push(SkillDiagnostic {
                        kind: SkillDiagnosticKind::Collision,
                        message: format!(
                            "name {:?} collision; winner: {}",
                            skill.name,
                            winner.display()
                        ),
                        path: file,
                    });
                } else {
                    names.insert(skill.name.clone(), file);
                    skills.push(skill);
                }
            }
            Ok(None) => {}
            Err(message) => diagnostics.push(SkillDiagnostic {
                kind: SkillDiagnosticKind::Warning,
                message,
                path: file,
            }),
        }
    }
}

fn ancestor_agent_skill_dirs(cwd: &Path) -> Vec<PathBuf> {
    let git_root = cwd
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists());
    let user_skills = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| absolute(&home.join(AGENTS_DIR_NAME).join("skills")));
    let mut roots = Vec::new();
    for ancestor in cwd.ancestors() {
        let skills = ancestor.join(AGENTS_DIR_NAME).join("skills");
        if user_skills
            .as_ref()
            .is_none_or(|user_skills| absolute(&skills) != *user_skills)
        {
            roots.push(skills);
        }
        if git_root == Some(ancestor) {
            break;
        }
    }
    roots
}

fn discover_skill_files(root: &Path, include_root_md: bool) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }
    let declared = root.join("SKILL.md");
    if declared.is_file() {
        return vec![declared];
    }
    let mut entries = std::fs::read_dir(root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut files = Vec::new();
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            files.extend(discover_skill_files(&path, false));
        } else if include_root_md && path.extension().is_some_and(|ext| ext == "md") {
            files.push(path);
        }
    }
    files
}

#[derive(Deserialize, Default)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(rename = "disable-model-invocation")]
    disable_model_invocation: Option<bool>,
}

fn load_skill(path: &Path, declared: bool) -> Result<Option<SkillInfo>, String> {
    let content = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let frontmatter = match parse_frontmatter(&content) {
        Ok(frontmatter) => frontmatter,
        Err(_) if !declared => return Ok(None),
        Err(error) => return Err(error),
    };
    let description = match frontmatter
        .description
        .filter(|value| !value.trim().is_empty())
    {
        Some(description) => description,
        None if !declared => return Ok(None),
        None => return Err("description is required".to_string()),
    };
    if description.len() > 1024 {
        return Err("description exceeds 1024 characters".to_string());
    }
    let parent_name = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .unwrap_or("skill");
    let name = frontmatter.name.unwrap_or_else(|| parent_name.to_string());
    Ok(Some(SkillInfo {
        name,
        description,
        file_path: absolute(path),
        content: strip_frontmatter(&content).trim().to_string(),
        disable_model_invocation: frontmatter.disable_model_invocation.unwrap_or(false),
    }))
}

fn parse_frontmatter(content: &str) -> Result<Frontmatter, String> {
    let Some(rest) = content.strip_prefix("---") else {
        return Ok(Frontmatter::default());
    };
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest);
    let end = rest
        .find("\n---")
        .ok_or_else(|| "unterminated frontmatter".to_string())?;
    serde_yaml::from_str(&rest[..end]).map_err(|error| error.to_string())
}

fn strip_frontmatter(content: &str) -> &str {
    let Some(rest) = content.strip_prefix("---") else {
        return content;
    };
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .unwrap_or(rest);
    let Some(end) = rest.find("\n---") else {
        return content;
    };
    let after = &rest[end + 4..];
    after
        .strip_prefix("\r\n")
        .or_else(|| after.strip_prefix('\n'))
        .unwrap_or(after)
}

fn format_skills_for_prompt(skills: &[SkillInfo]) -> String {
    let visible = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return String::new();
    }
    let mut prompt = String::from(
        "\n\nThe following skills provide specialized instructions for specific tasks.\nUse the read tool to load a skill's file when the task matches its description.\nWhen a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.\n\n<available_skills>\n",
    );
    for skill in visible {
        prompt.push_str(&format!(
            "  <skill>\n    <name>{}</name>\n    <description>{}</description>\n    <location>{}</location>\n  </skill>\n",
            escape_xml(&skill.name),
            escape_xml(&skill.description),
            escape_xml(&slash_path(&skill.file_path))
        ));
    }
    prompt.push_str("</available_skills>");
    prompt
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn slash_path(path: impl AsRef<Path>) -> String {
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
    use pi_core::{AbortHandle, ModelId, ProviderId, RunId};
    use pi_runtime::{PiRuntime, SystemPrompt};
    use pi_test_support::ScriptedProviderPlugin;

    #[tokio::test]
    async fn command_registry_expands_explicit_skill_invocation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SKILL.md");
        std::fs::write(
            &path,
            "---\nname: grill-me\ndescription: Interview the user\ndisable-model-invocation: true\n---\n\n# Instructions\nAsk questions.",
        )
        .unwrap();
        let runtime = PiRuntime::builder()
            .agent_plugin(SkillsPlugin::from_skills([SkillInfo {
                name: "grill-me".to_string(),
                description: "Interview the user".to_string(),
                file_path: path,
                content: "# Instructions\nAsk questions.".to_string(),
                disable_model_invocation: true,
            }]))
            .build()
            .unwrap();
        let outcome = runtime
            .execute_command("/skill:grill-me sharpen this design")
            .await
            .unwrap()
            .expect("skill command should be registered");
        let CommandOutcome::TransformInput(text) = outcome else {
            panic!("expected transformed input")
        };
        assert!(text.contains("<skill name=\"grill-me\""));
        assert!(!text.contains("disable-model-invocation"));
        assert!(text.ends_with("sharpen this design"));
    }

    #[test]
    fn explicit_skill_invocation_formats_location_references_and_task() {
        let skill = SkillInfo {
            name: "inspect".to_string(),
            description: "Inspect things".to_string(),
            file_path: "/project/.pi/skills/inspect/SKILL.md".into(),
            content: "Use inspection tools.".to_string(),
            disable_model_invocation: false,
        };

        assert_eq!(
            render_skill_invocation(&skill, "Check errors."),
            "<skill name=\"inspect\" location=\"/project/.pi/skills/inspect/SKILL.md\">\nReferences are relative to /project/.pi/skills/inspect.\n\nUse inspection tools.\n</skill>\n\nCheck errors."
        );
    }

    #[test]
    fn skill_commands_use_registry_duplicate_checks() {
        let skill = SkillInfo {
            name: "same".to_string(),
            description: "duplicate fixture".to_string(),
            file_path: "/same/SKILL.md".into(),
            content: "body".to_string(),
            disable_model_invocation: false,
        };
        let error = match PiRuntime::builder()
            .agent_plugin(SkillsPlugin::from_skills([skill.clone(), skill]))
            .build()
        {
            Ok(_) => panic!("duplicate skill commands must fail registration"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("duplicate command name: skill:same")
        );
    }

    #[tokio::test]
    async fn before_agent_start_contributes_visible_catalog_only_with_read() {
        let plugin = SkillsPlugin::from_skills([
            SkillInfo {
                name: "visible".into(),
                description: "visible description".into(),
                file_path: "/visible/SKILL.md".into(),
                content: "visible content".into(),
                disable_model_invocation: false,
            },
            SkillInfo {
                name: "hidden".into(),
                description: "hidden description".into(),
                file_path: "/hidden/SKILL.md".into(),
                content: "hidden content".into(),
                disable_model_invocation: true,
            },
        ]);
        let (_, signal) = AbortHandle::new();
        let patch = plugin
            .before_agent_start(
                PluginContext::new(
                    PluginId::new("skills"),
                    RunId::new("run"),
                    "/tmp".into(),
                    signal,
                ),
                BeforeAgentStartEvent {
                    system_prompt: "base".into(),
                    input_messages: Vec::new(),
                    active_tools: vec!["read".into()],
                    provider_id: ProviderId::new("provider"),
                    model_id: ModelId::new("model"),
                },
            )
            .await
            .unwrap();
        let prompt = patch.system_prompt.unwrap();
        assert!(prompt.starts_with("base"));
        assert!(prompt.contains("<name>visible</name>"));
        assert!(!prompt.contains("<name>hidden</name>"));
    }

    #[test]
    fn model_visible_catalog_escapes_every_xml_field_and_preserves_order() {
        let prompt = format_skills_for_prompt(&[
            SkillInfo {
                name: "a&b".into(),
                description: "Use <this> & \"that\" 'now'".into(),
                file_path: "/skills/<bad>&\"quoted\"/SKILL.md".into(),
                content: "first".into(),
                disable_model_invocation: false,
            },
            SkillInfo {
                name: "hidden".into(),
                description: "must not be rendered".into(),
                file_path: "/hidden/SKILL.md".into(),
                content: "hidden".into(),
                disable_model_invocation: true,
            },
            SkillInfo {
                name: "second".into(),
                description: "second".into(),
                file_path: "/second/SKILL.md".into(),
                content: "second".into(),
                disable_model_invocation: false,
            },
        ]);

        assert!(prompt.contains("<name>a&amp;b</name>"));
        assert!(prompt.contains(
            "<description>Use &lt;this&gt; &amp; &quot;that&quot; &apos;now&apos;</description>"
        ));
        assert!(
            prompt.contains(
                "<location>/skills/&lt;bad&gt;&amp;&quot;quoted&quot;/SKILL.md</location>"
            )
        );
        assert!(!prompt.contains("<name>hidden</name>"));
        assert!(
            prompt.find("<name>a&amp;b</name>").unwrap()
                < prompt.find("<name>second</name>").unwrap()
        );
    }

    #[test]
    fn loader_owns_discovery_and_first_root_wins_collisions() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let cwd = directory.path().join("project");
        std::fs::create_dir_all(agent_dir.join("skills/a")).unwrap();
        std::fs::create_dir_all(cwd.join(".pi/skills/b")).unwrap();
        let body = "---\nname: same\ndescription: test\n---\nbody";
        std::fs::write(agent_dir.join("skills/a/SKILL.md"), body).unwrap();
        std::fs::write(cwd.join(".pi/skills/b/SKILL.md"), body).unwrap();

        let plugin = SkillsPlugin::load(SkillLoaderOptions::new(&cwd, &agent_dir));

        assert_eq!(plugin.skills().len(), 1);
        assert_eq!(plugin.diagnostics().len(), 1);
        assert_eq!(plugin.diagnostics()[0].kind, SkillDiagnosticKind::Collision);
    }

    #[test]
    fn sourced_loader_preserves_caller_provenance_on_skills_and_diagnostics() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("user");
        std::fs::create_dir_all(root.join("example")).unwrap();
        std::fs::create_dir_all(root.join("broken")).unwrap();
        std::fs::write(
            root.join("example/SKILL.md"),
            "---\nname: example\ndescription: Example\n---\nbody",
        )
        .unwrap();
        std::fs::write(root.join("broken/SKILL.md"), "---\nname: broken\n---\nbody").unwrap();

        let catalog = load_sourced_skills([SkillSourceInput {
            path: root,
            source: ("user".to_string(), 7_u32),
        }]);

        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].skill.name, "example");
        assert_eq!(catalog.skills[0].source, ("user".to_string(), 7));
        assert_eq!(catalog.diagnostics.len(), 1);
        assert_eq!(catalog.diagnostics[0].source, ("user".to_string(), 7));
        assert!(catalog.diagnostics[0].message.contains("description"));
    }

    #[test]
    fn root_documentation_without_skill_metadata_is_silently_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("skills");
        std::fs::create_dir_all(root.join("nested-skill")).unwrap();
        std::fs::write(root.join("README.md"), "# Shared skills\n\nDocumentation.").unwrap();
        std::fs::write(root.join("AGENTS.md"), "# Agent notes").unwrap();
        std::fs::write(
            root.join("CLAUDE.md"),
            "---\ndescription: [invalid\n---\nDocumentation.",
        )
        .unwrap();
        std::fs::write(
            root.join("root.md"),
            "---\ndescription: Root skill\n---\nRoot content",
        )
        .unwrap();
        std::fs::write(
            root.join("nested-skill/SKILL.md"),
            "---\nname: nested-skill\ndescription: Nested skill\n---\nNested content",
        )
        .unwrap();
        let mut options = SkillLoaderOptions::new(directory.path(), directory.path().join("agent"));
        options.include_defaults = false;
        options.additional_paths = vec![root];

        let plugin = SkillsPlugin::load(options);

        assert!(plugin.diagnostics().is_empty());
        let mut names = plugin
            .skills()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(names, vec!["nested-skill", "skills"]);
    }

    #[cfg(unix)]
    #[test]
    fn loader_follows_symlinked_skill_directories_without_rewriting_the_visible_path() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let actual = directory.path().join("actual/example");
        let linked = directory.path().join("skills-link");
        std::fs::create_dir_all(&actual).unwrap();
        std::fs::write(
            actual.join("SKILL.md"),
            "---\nname: example\ndescription: Example skill\n---\nbody",
        )
        .unwrap();
        symlink(directory.path().join("actual"), &linked).unwrap();
        let mut options = SkillLoaderOptions::new(directory.path(), directory.path().join("agent"));
        options.include_defaults = false;
        options.additional_paths = vec![linked.clone()];

        let plugin = SkillsPlugin::load(options);

        assert!(plugin.diagnostics().is_empty());
        assert_eq!(plugin.skills().len(), 1);
        assert_eq!(plugin.skills()[0].name, "example");
        assert_eq!(
            plugin.skills()[0].file_path,
            linked.join("example/SKILL.md")
        );
    }

    #[test]
    fn untrusted_projects_only_load_user_skill_roots() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let cwd = directory.path().join("project");
        std::fs::create_dir_all(agent_dir.join("skills/global")).unwrap();
        std::fs::create_dir_all(cwd.join(".pi/skills/project")).unwrap();
        std::fs::write(
            agent_dir.join("skills/global/SKILL.md"),
            "---\nname: global\ndescription: global\n---\nbody",
        )
        .unwrap();
        std::fs::write(
            cwd.join(".pi/skills/project/SKILL.md"),
            "---\nname: project\ndescription: project\n---\nbody",
        )
        .unwrap();
        let mut options = SkillLoaderOptions::new(&cwd, &agent_dir);
        options.project_trusted = false;

        let plugin = SkillsPlugin::load(options);

        assert_eq!(
            plugin
                .skills()
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            vec!["global"]
        );
    }

    #[test]
    fn trusted_projects_search_ancestor_agents_skills_to_git_root() {
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path().join("repo");
        let cwd = repo.join("packages/app");
        let agent_dir = directory.path().join("agent");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(cwd.clone()).unwrap();
        std::fs::create_dir_all(repo.join(".agents/skills/shared")).unwrap();
        std::fs::write(
            repo.join(".agents/skills/shared/SKILL.md"),
            "---\nname: shared\ndescription: shared\n---\nbody",
        )
        .unwrap();

        let plugin = SkillsPlugin::load(SkillLoaderOptions::new(&cwd, &agent_dir));

        assert!(plugin.skills().iter().any(|skill| skill.name == "shared"));
    }

    #[tokio::test]
    async fn reload_rescans_skill_catalog_as_part_of_the_next_generation() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let cwd = directory.path().join("project");
        std::fs::create_dir_all(agent_dir.join("skills/one")).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(
            agent_dir.join("skills/one/SKILL.md"),
            "---\nname: one\ndescription: first\n---\nbody",
        )
        .unwrap();
        let options = SkillLoaderOptions::new(&cwd, &agent_dir);
        let runtime = PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .agent_plugin_factory(move || SkillsPlugin::load(options.clone()))
            .system_prompt(SystemPrompt::Pi(Box::default()))
            .build()
            .unwrap();
        assert!(
            runtime
                .command_specs()
                .iter()
                .any(|spec| spec.name == "skill:one")
        );
        let Some(CommandOutcome::TransformInput(initial)) =
            runtime.execute_command("/skill:one").await.unwrap()
        else {
            panic!("expected skill expansion")
        };
        assert!(initial.contains("body"));

        std::fs::write(
            agent_dir.join("skills/one/SKILL.md"),
            "---\nname: one\ndescription: updated\n---\nnew body",
        )
        .unwrap();
        let Some(CommandOutcome::TransformInput(before_reload)) =
            runtime.execute_command("/skill:one").await.unwrap()
        else {
            panic!("expected skill expansion")
        };
        assert!(before_reload.contains("\nbody\n</skill>"));
        assert!(!before_reload.contains("new body"));

        std::fs::create_dir_all(agent_dir.join("skills/two")).unwrap();
        std::fs::write(
            agent_dir.join("skills/two/SKILL.md"),
            "---\nname: two\ndescription: second\n---\nbody",
        )
        .unwrap();
        runtime.reload().await.unwrap();

        assert!(
            runtime
                .command_specs()
                .iter()
                .any(|spec| spec.name == "skill:two")
        );
        let Some(CommandOutcome::TransformInput(after_reload)) =
            runtime.execute_command("/skill:one").await.unwrap()
        else {
            panic!("expected skill expansion")
        };
        assert!(after_reload.contains("new body"));
        assert_eq!(runtime.generation(), 2);
    }
}
