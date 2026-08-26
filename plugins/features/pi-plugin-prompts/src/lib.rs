#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    AgentPlugin, Command, CommandContext, CommandError, CommandOutcome, CommandSpec, PluginId,
    RegisterContext,
};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;

const CONFIG_DIR_NAME: &str = ".pi";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplateLoaderOptions {
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

impl PromptTemplateLoaderOptions {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptTemplateSourceKind {
    Project,
    User,
    Additional,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplateSource {
    pub kind: PromptTemplateSourceKind,
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub content: String,
    pub file_path: PathBuf,
    pub source: PromptTemplateSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptTemplateDiagnosticCode {
    FileInfoFailed,
    ListFailed,
    ReadFailed,
    ParseFailed,
    Collision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplateDiagnostic {
    pub code: PromptTemplateDiagnosticCode,
    pub message: String,
    pub path: PathBuf,
    pub source: PromptTemplateSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplateSourceInput<T> {
    pub path: PathBuf,
    pub source: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedPromptTemplate<T> {
    pub prompt_template: PromptTemplate,
    pub source: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedPromptTemplateDiagnostic<T> {
    pub diagnostic: PromptTemplateDiagnostic,
    pub source: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedPromptTemplateCatalog<T> {
    pub prompt_templates: Vec<SourcedPromptTemplate<T>>,
    pub diagnostics: Vec<SourcedPromptTemplateDiagnostic<T>>,
}

/// Loads source-tagged paths without interpreting or normalizing the caller's
/// provenance value. The same value is attached to successes and diagnostics.
pub fn load_sourced_prompt_templates<T: Clone>(
    inputs: impl IntoIterator<Item = PromptTemplateSourceInput<T>>,
) -> SourcedPromptTemplateCatalog<T> {
    let mut prompt_templates = Vec::new();
    let mut diagnostics = Vec::new();
    for input in inputs {
        let loader_source = PromptTemplateSource {
            kind: PromptTemplateSourceKind::Additional,
            root: absolute(&input.path),
        };
        let (loaded, loaded_diagnostics) = load_path(&input.path, &loader_source);
        prompt_templates.extend(
            loaded
                .into_iter()
                .map(|prompt_template| SourcedPromptTemplate {
                    prompt_template,
                    source: input.source.clone(),
                }),
        );
        diagnostics.extend(loaded_diagnostics.into_iter().map(|diagnostic| {
            SourcedPromptTemplateDiagnostic {
                diagnostic,
                source: input.source.clone(),
            }
        }));
    }
    SourcedPromptTemplateCatalog {
        prompt_templates,
        diagnostics,
    }
}

/// Generation-local prompt-template catalog and slash-command owner.
pub struct PromptTemplatesPlugin {
    templates: Vec<PromptTemplate>,
    diagnostics: Vec<PromptTemplateDiagnostic>,
}

impl PromptTemplatesPlugin {
    pub fn new(options: PromptTemplateLoaderOptions) -> Self {
        Self::load(options)
    }

    pub fn load(options: PromptTemplateLoaderOptions) -> Self {
        let cwd = absolute(&options.cwd);
        let agent_dir = absolute(&options.agent_dir);
        let mut roots = Vec::new();
        if options.include_defaults {
            if options.project_trusted {
                roots.push((
                    cwd.join(CONFIG_DIR_NAME).join("prompts"),
                    PromptTemplateSourceKind::Project,
                ));
            }
            roots.push((agent_dir.join("prompts"), PromptTemplateSourceKind::User));
        }
        roots.extend(options.additional_paths.into_iter().map(|path| {
            let path = if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            };
            (path, PromptTemplateSourceKind::Additional)
        }));

        let mut templates = Vec::new();
        let mut diagnostics = Vec::new();
        let mut names = HashMap::<String, PathBuf>::new();
        for (root, kind) in roots {
            let source = PromptTemplateSource {
                kind,
                root: absolute(&root),
            };
            let (loaded, mut loaded_diagnostics) = load_path(&root, &source);
            diagnostics.append(&mut loaded_diagnostics);
            for template in loaded {
                if let Some(winner) = names.get(&template.name) {
                    diagnostics.push(PromptTemplateDiagnostic {
                        code: PromptTemplateDiagnosticCode::Collision,
                        message: format!(
                            "name {:?} collision; winner: {}",
                            template.name,
                            winner.display()
                        ),
                        path: template.file_path,
                        source: template.source,
                    });
                } else {
                    names.insert(template.name.clone(), template.file_path.clone());
                    templates.push(template);
                }
            }
        }
        Self {
            templates,
            diagnostics,
        }
    }

    pub fn from_templates(templates: impl IntoIterator<Item = PromptTemplate>) -> Self {
        Self {
            templates: templates.into_iter().collect(),
            diagnostics: Vec::new(),
        }
    }

    pub fn templates(&self) -> &[PromptTemplate] {
        &self.templates
    }

    pub fn diagnostics(&self) -> &[PromptTemplateDiagnostic] {
        &self.diagnostics
    }
}

struct PromptTemplateCommand {
    template: PromptTemplate,
}

#[async_trait]
impl Command for PromptTemplateCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: self.template.name.clone(),
            description: self.template.description.clone(),
            argument_hint: self.template.argument_hint.clone(),
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
        Ok(CommandOutcome::TransformInput(
            format_prompt_template_invocation(&self.template, &parse_command_args(&arguments)),
        ))
    }
}

#[async_trait]
impl AgentPlugin for PromptTemplatesPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("prompt-templates")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        for template in &self.templates {
            context.register_command(Arc::new(PromptTemplateCommand {
                template: template.clone(),
            }))?;
        }
        Ok(())
    }
}

fn load_path(
    path: &Path,
    source: &PromptTemplateSource,
) -> (Vec<PromptTemplate>, Vec<PromptTemplateDiagnostic>) {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (Vec::new(), Vec::new());
        }
        Err(error) => {
            return (
                Vec::new(),
                vec![diagnostic(
                    PromptTemplateDiagnosticCode::FileInfoFailed,
                    error.to_string(),
                    path,
                    source,
                )],
            );
        }
    };
    if metadata.is_file() {
        if path.extension().is_none_or(|extension| extension != "md") {
            return (Vec::new(), Vec::new());
        }
        return match load_template(path, source) {
            Ok(template) => (vec![template], Vec::new()),
            Err(diagnostic) => (Vec::new(), vec![diagnostic]),
        };
    }
    if !metadata.is_dir() {
        return (Vec::new(), Vec::new());
    }
    let mut entries = match std::fs::read_dir(path) {
        Ok(entries) => entries.filter_map(Result::ok).collect::<Vec<_>>(),
        Err(error) => {
            return (
                Vec::new(),
                vec![diagnostic(
                    PromptTemplateDiagnosticCode::ListFailed,
                    error.to_string(),
                    path,
                    source,
                )],
            );
        }
    };
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut templates = Vec::new();
    let mut diagnostics = Vec::new();
    for entry in entries {
        let visible_path = entry.path();
        if visible_path
            .extension()
            .is_none_or(|extension| extension != "md")
            || !visible_path.is_file()
        {
            continue;
        }
        match load_template(&visible_path, source) {
            Ok(template) => templates.push(template),
            Err(diagnostic) => diagnostics.push(diagnostic),
        }
    }
    (templates, diagnostics)
}

fn load_template(
    path: &Path,
    source: &PromptTemplateSource,
) -> Result<PromptTemplate, PromptTemplateDiagnostic> {
    let raw = std::fs::read_to_string(path).map_err(|error| {
        diagnostic(
            PromptTemplateDiagnosticCode::ReadFailed,
            error.to_string(),
            path,
            source,
        )
    })?;
    let (frontmatter, content) = parse_frontmatter(&raw).map_err(|message| {
        diagnostic(
            PromptTemplateDiagnosticCode::ParseFailed,
            message,
            path,
            source,
        )
    })?;
    let description = frontmatter_string(&frontmatter, "description")
        .filter(|description| !description.is_empty())
        .unwrap_or_else(|| first_line_description(&content));
    let argument_hint = frontmatter_string(&frontmatter, "argument-hint")
        .filter(|argument_hint| !argument_hint.is_empty());
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("prompt")
        .strip_suffix(".md")
        .unwrap_or("prompt")
        .to_string();
    Ok(PromptTemplate {
        name,
        description,
        argument_hint,
        content,
        file_path: absolute(path),
        source: source.clone(),
    })
}

fn parse_frontmatter(content: &str) -> Result<(Value, String), String> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---") {
        return Ok((Value::Mapping(Default::default()), normalized));
    }
    let Some(relative_end) = normalized[3..].find("\n---") else {
        return Ok((Value::Mapping(Default::default()), normalized));
    };
    let end = 3 + relative_end;
    let yaml_start = 4.min(end);
    let frontmatter =
        serde_yaml::from_str(&normalized[yaml_start..end]).map_err(|error| error.to_string())?;
    Ok((frontmatter, normalized[end + 4..].trim().to_string()))
}

fn frontmatter_string(frontmatter: &Value, key: &str) -> Option<String> {
    frontmatter
        .as_mapping()?
        .get(Value::String(key.to_string()))?
        .as_str()
        .map(str::to_string)
}

fn first_line_description(content: &str) -> String {
    let Some(line) = content.lines().find(|line| !line.trim().is_empty()) else {
        return String::new();
    };
    let mut description = line.chars().take(60).collect::<String>();
    if line.chars().count() > 60 {
        description.push_str("...");
    }
    description
}

fn diagnostic(
    code: PromptTemplateDiagnosticCode,
    message: String,
    path: &Path,
    source: &PromptTemplateSource,
) -> PromptTemplateDiagnostic {
    PromptTemplateDiagnostic {
        code,
        message,
        path: absolute(path),
        source: source.clone(),
    }
}

/// Parses command arguments with Pi's simple single/double-quote rules.
pub fn parse_command_args(arguments: &str) -> Vec<String> {
    let mut parsed = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in arguments.chars() {
        if let Some(expected) = quote {
            if character == expected {
                quote = None;
            } else {
                current.push(character);
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if matches!(character, ' ' | '\t') {
            if !current.is_empty() {
                parsed.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if !current.is_empty() {
        parsed.push(current);
    }
    parsed
}

/// Substitutes `$N`, `$@`, `$ARGUMENTS`, `${@:N}`, and `${@:N:L}`.
pub fn substitute_args(content: &str, arguments: &[String]) -> String {
    let all_arguments = arguments.join(" ");
    let mut output = String::with_capacity(content.len());
    let mut index = 0;
    while index < content.len() {
        let remaining = &content[index..];
        if !remaining.starts_with('$') {
            let character = remaining.chars().next().expect("non-empty remainder");
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        if let Some(rest) = remaining.strip_prefix("$ARGUMENTS") {
            output.push_str(&all_arguments);
            index = content.len() - rest.len();
            continue;
        }
        if let Some(rest) = remaining.strip_prefix("$@") {
            output.push_str(&all_arguments);
            index = content.len() - rest.len();
            continue;
        }
        if let Some(rest) = remaining.strip_prefix("${@:")
            && let Some(end) = rest.find('}')
            && let Some(replacement) = substitute_slice(&rest[..end], arguments)
        {
            output.push_str(&replacement);
            index += 4 + end + 1;
            continue;
        }
        let digits = remaining[1..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        if digits > 0 {
            let number = remaining[1..1 + digits].parse::<usize>().unwrap_or(0);
            if let Some(argument) = number
                .checked_sub(1)
                .and_then(|argument_index| arguments.get(argument_index))
            {
                output.push_str(argument);
            }
            index += 1 + digits;
            continue;
        }
        output.push('$');
        index += 1;
    }
    output
}

fn substitute_slice(slice: &str, arguments: &[String]) -> Option<String> {
    let mut parts = slice.split(':');
    let start = parts.next()?.parse::<usize>().ok()?.saturating_sub(1);
    let length = parts.next().map(str::parse::<usize>).transpose().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let end = length.map_or(arguments.len(), |length| {
        start.saturating_add(length).min(arguments.len())
    });
    Some(
        arguments
            .get(start.min(arguments.len())..end)
            .unwrap_or_default()
            .join(" "),
    )
}

pub fn format_prompt_template_invocation(
    template: &PromptTemplate,
    arguments: &[String],
) -> String {
    substitute_args(&template.content, arguments)
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

#[cfg(test)]
mod tests {
    use super::*;
    use pi_test_support::ScriptedProviderPlugin;

    fn options(root: &Path) -> PromptTemplateLoaderOptions {
        PromptTemplateLoaderOptions {
            cwd: root.to_path_buf(),
            agent_dir: root.join("agent"),
            additional_paths: Vec::new(),
            include_defaults: false,
            project_trusted: true,
        }
    }

    #[test]
    fn loads_markdown_non_recursively_in_path_and_filename_order() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a/nested")).unwrap();
        std::fs::create_dir_all(root.path().join("b")).unwrap();
        std::fs::write(
            root.path().join("a/one.md"),
            "---\ndescription: One template\nargument-hint: <file>\n---\nHello $1",
        )
        .unwrap();
        std::fs::write(root.path().join("a/nested/ignored.md"), "Ignored").unwrap();
        std::fs::write(root.path().join("b/two.md"), "First line description\nBody").unwrap();
        let mut options = options(root.path());
        options.additional_paths = vec![root.path().join("a"), root.path().join("b")];

        let plugin = PromptTemplatesPlugin::load(options);

        assert!(plugin.diagnostics().is_empty());
        assert_eq!(
            plugin
                .templates()
                .iter()
                .map(|template| template.name.as_str())
                .collect::<Vec<_>>(),
            vec!["one", "two"]
        );
        assert_eq!(plugin.templates()[0].description, "One template");
        assert_eq!(
            plugin.templates()[0].argument_hint.as_deref(),
            Some("<file>")
        );
        assert_eq!(plugin.templates()[0].content, "Hello $1");
        assert_eq!(plugin.templates()[1].description, "First line description");
        assert!(plugin.templates().iter().all(|template| {
            template.source.kind == PromptTemplateSourceKind::Additional
                && template.source.root.is_absolute()
        }));
    }

    #[test]
    fn source_and_parse_diagnostics_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        let broken = root.path().join("broken.md");
        std::fs::write(&broken, "---\ndescription: [unterminated\n---\nBody").unwrap();
        let mut options = options(root.path());
        options.additional_paths = vec![broken.clone()];

        let plugin = PromptTemplatesPlugin::load(options);

        assert!(plugin.templates().is_empty());
        assert_eq!(plugin.diagnostics().len(), 1);
        assert_eq!(
            plugin.diagnostics()[0].code,
            PromptTemplateDiagnosticCode::ParseFailed
        );
        assert_eq!(plugin.diagnostics()[0].path, broken);
        assert_eq!(
            plugin.diagnostics()[0].source.kind,
            PromptTemplateSourceKind::Additional
        );
    }

    #[test]
    fn sourced_loader_preserves_arbitrary_caller_provenance_exactly() {
        #[derive(Debug, Clone, PartialEq, Eq)]
        struct Provenance {
            scope: &'static str,
            priority: u8,
        }

        let root = tempfile::tempdir().unwrap();
        let valid = root.path().join("valid.md");
        let broken = root.path().join("broken.md");
        std::fs::write(&valid, "---\ndescription: Valid\n---\nBody").unwrap();
        std::fs::write(&broken, "---\ndescription: [unterminated\n---\nBody").unwrap();
        let valid_source = Provenance {
            scope: "project",
            priority: 1,
        };
        let broken_source = Provenance {
            scope: "user",
            priority: 2,
        };

        let catalog = load_sourced_prompt_templates([
            PromptTemplateSourceInput {
                path: valid,
                source: valid_source.clone(),
            },
            PromptTemplateSourceInput {
                path: broken,
                source: broken_source.clone(),
            },
        ]);

        assert_eq!(catalog.prompt_templates.len(), 1);
        assert_eq!(catalog.prompt_templates[0].source, valid_source);
        assert_eq!(catalog.diagnostics.len(), 1);
        assert_eq!(catalog.diagnostics[0].source, broken_source);
    }

    #[cfg(unix)]
    #[test]
    fn explicit_and_symlinked_files_keep_the_visible_name() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target.md");
        let linked = root.path().join("link.md");
        std::fs::write(&target, "---\ndescription: Target\n---\nTarget body").unwrap();
        symlink(&target, &linked).unwrap();
        let mut options = options(root.path());
        options.additional_paths = vec![target, linked.clone()];

        let plugin = PromptTemplatesPlugin::load(options);

        assert_eq!(
            plugin
                .templates()
                .iter()
                .map(|template| template.name.as_str())
                .collect::<Vec<_>>(),
            vec!["target", "link"]
        );
        assert_eq!(plugin.templates()[1].file_path, linked);
    }

    #[test]
    fn argument_parsing_and_every_placeholder_match_pi() {
        let arguments = parse_command_args("'hello world' test third");
        assert_eq!(arguments, vec!["hello world", "test", "third"]);
        assert_eq!(
            substitute_args("$1|$2|$9|${@:2}|${@:2:1}|$ARGUMENTS|$@", &arguments),
            "hello world|test||test third|test|hello world test third|hello world test third"
        );
    }

    #[tokio::test]
    async fn registered_template_command_expands_before_agent_input() {
        let template = PromptTemplate {
            name: "review".to_string(),
            description: "Review a file".to_string(),
            argument_hint: Some("<file> [focus]".to_string()),
            content: "Review $1 with ${@:2}".to_string(),
            file_path: "/prompts/review.md".into(),
            source: PromptTemplateSource {
                kind: PromptTemplateSourceKind::User,
                root: "/prompts".into(),
            },
        };
        let runtime = pi_runtime::PiRuntime::builder()
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .agent_plugin(PromptTemplatesPlugin::from_templates([template]))
            .build()
            .unwrap();

        assert_eq!(
            runtime
                .execute_command("/review 'a b.rs' safety")
                .await
                .unwrap(),
            Some(CommandOutcome::TransformInput(
                "Review a b.rs with safety".to_string()
            ))
        );
        let spec = runtime
            .command_specs()
            .into_iter()
            .find(|spec| spec.name == "review")
            .unwrap();
        assert_eq!(spec.argument_hint.as_deref(), Some("<file> [focus]"));
    }
}
