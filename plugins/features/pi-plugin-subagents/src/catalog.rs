use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_core::ThinkingLevel;
use serde::{Deserialize, Deserializer, de};
use serde_yaml::Value as YamlValue;

use crate::profiles::{SubagentProfile, SystemPromptMode, builtin_profiles};

const MAX_DEFINITION_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub struct SubagentLoaderOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub project_trusted: bool,
    pub additional_paths: Vec<PathBuf>,
}

impl SubagentLoaderOptions {
    pub fn new(cwd: impl Into<PathBuf>, agent_dir: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            agent_dir: agent_dir.into(),
            project_trusted: true,
            additional_paths: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SubagentCatalogError {
    #[error("failed to load subagent configuration {}: {message}", path.display())]
    Configuration { path: PathBuf, message: String },
    #[error("failed to load subagent definition {}: {message}", path.display())]
    Definition { path: PathBuf, message: String },
    #[error(
        "duplicate subagent name {name:?} in one discovery root: {} and {}",
        first.display(),
        second.display()
    )]
    Duplicate {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
    #[error("ambiguous subagent alias {alias:?} is declared by both {first:?} and {second:?}")]
    AmbiguousAlias {
        alias: String,
        first: String,
        second: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct SubagentCatalog {
    profiles: Arc<Vec<SubagentProfile>>,
}

impl SubagentCatalog {
    pub(crate) fn builtins() -> Self {
        Self {
            profiles: Arc::new(builtin_profiles()),
        }
    }

    pub(crate) fn load(options: &SubagentLoaderOptions) -> Result<Self, SubagentCatalogError> {
        let cwd = absolute(&options.cwd);
        let agent_dir = absolute(&options.agent_dir);
        let mut profiles = builtin_profiles();

        merge_root(&mut profiles, &agent_dir.join("agents"))?;
        for path in &options.additional_paths {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                cwd.join(path)
            };
            merge_root(&mut profiles, &path)?;
        }
        if options.project_trusted
            && let Some(path) = nearest_project_agent_root(&cwd)
        {
            merge_root(&mut profiles, &path)?;
        }
        validate_aliases(&profiles)?;

        Ok(Self {
            profiles: Arc::new(profiles),
        })
    }

    pub(crate) fn profile(&self, name: &str) -> Option<SubagentProfile> {
        self.profiles
            .iter()
            .find(|profile| profile.name == name)
            .or_else(|| {
                self.profiles
                    .iter()
                    .find(|profile| profile.aliases.iter().any(|alias| alias == name))
            })
            .cloned()
    }

    pub(crate) fn profile_names(&self) -> Vec<String> {
        let canonical = self
            .profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<HashSet<_>>();
        let mut seen = HashSet::new();
        self.profiles
            .iter()
            .flat_map(|profile| {
                std::iter::once(profile.name.clone()).chain(
                    profile
                        .aliases
                        .iter()
                        .filter(|alias| !canonical.contains(alias.as_str()))
                        .cloned(),
                )
            })
            .filter(|name| seen.insert(name.clone()))
            .collect()
    }

    pub(crate) fn formatted_catalog(&self) -> String {
        let canonical = self
            .profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<HashSet<_>>();
        self.profiles
            .iter()
            .map(|profile| {
                let aliases = profile
                    .aliases
                    .iter()
                    .filter(|alias| !canonical.contains(alias.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                let aliases = if aliases.is_empty() {
                    String::new()
                } else {
                    format!(" (aliases: {})", aliases.join(", "))
                };
                format!("- `{}`{aliases}: {}", profile.name, profile.description)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn validate_aliases(profiles: &[SubagentProfile]) -> Result<(), SubagentCatalogError> {
    let canonical = profiles
        .iter()
        .map(|profile| profile.name.as_str())
        .collect::<HashSet<_>>();
    let mut aliases = HashMap::<&str, &str>::new();
    for profile in profiles {
        for alias in &profile.aliases {
            if canonical.contains(alias.as_str()) {
                continue;
            }
            if let Some(first) = aliases.insert(alias, &profile.name)
                && first != profile.name
            {
                return Err(SubagentCatalogError::AmbiguousAlias {
                    alias: alias.clone(),
                    first: first.to_string(),
                    second: profile.name.clone(),
                });
            }
        }
    }
    Ok(())
}

fn merge_root(
    profiles: &mut Vec<SubagentProfile>,
    root: &Path,
) -> Result<(), SubagentCatalogError> {
    let files = discover_definition_files(root);
    let mut names = HashMap::<String, PathBuf>::new();
    for path in files {
        let profile = load_definition(&path)?;
        if let Some(first) = names.insert(profile.name.clone(), path.clone()) {
            return Err(SubagentCatalogError::Duplicate {
                name: profile.name,
                first,
                second: path,
            });
        }
        if let Some(index) = profiles
            .iter()
            .position(|candidate| candidate.name == profile.name)
        {
            profiles[index] = profile;
        } else {
            profiles.push(profile);
        }
    }
    Ok(())
}

fn discover_definition_files(root: &Path) -> Vec<PathBuf> {
    if root.is_file() {
        return root
            .extension()
            .is_some_and(|extension| extension == "md")
            .then(|| root.to_path_buf())
            .into_iter()
            .collect();
    }
    if !root.is_dir() {
        return Vec::new();
    }
    let mut files = Vec::new();
    let mut visited = HashSet::new();
    collect_definition_files(root, &mut visited, &mut files);
    files.sort();
    files
}

fn collect_definition_files(root: &Path, visited: &mut HashSet<PathBuf>, files: &mut Vec<PathBuf>) {
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| absolute(root));
    if !visited.insert(canonical) {
        return;
    }
    let mut entries = std::fs::read_dir(root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_definition_files(&path, visited, files);
        } else if path.extension().is_some_and(|extension| extension == "md")
            && !name.ends_with(".chain.md")
        {
            files.push(path);
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    aliases: StringList,
    system_prompt_mode: Option<String>,
    #[serde(default)]
    allow_nested_subagents: bool,
    max_subagent_depth: Option<usize>,
    #[serde(default)]
    tools: OptionalStringList,
    #[serde(default)]
    exclude_tools: StringList,
    model: Option<String>,
    thinking: Option<ThinkingSetting>,
    #[serde(default)]
    inherit_skills: bool,
    #[serde(default)]
    skills: StringList,
    #[serde(default)]
    skill_path: StringList,
    timeout_ms: Option<u64>,
}

#[derive(Debug, Default)]
struct OptionalStringList(Option<Vec<String>>);

impl<'de> Deserialize<'de> for OptionalStringList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let values = deserialize_string_list(YamlValue::deserialize(deserializer)?)?;
        Ok(Self(Some(values)))
    }
}

#[derive(Debug, Default)]
struct StringList(Vec<String>);

impl<'de> Deserialize<'de> for StringList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_string_list(YamlValue::deserialize(deserializer)?).map(Self)
    }
}

fn deserialize_string_list<E: de::Error>(value: YamlValue) -> Result<Vec<String>, E> {
    let values = match value {
        YamlValue::Null => Vec::new(),
        YamlValue::String(value) => value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
        YamlValue::Sequence(values) => values
            .into_iter()
            .map(|value| match value {
                YamlValue::String(value) if !value.trim().is_empty() => {
                    Ok(value.trim().to_string())
                }
                _ => Err(E::custom("list fields must contain only non-empty strings")),
            })
            .collect::<Result<Vec<_>, E>>()?,
        _ => {
            return Err(E::custom(
                "list fields must be a comma-separated string, a string list, or empty",
            ));
        }
    };
    let mut unique = HashSet::new();
    Ok(values
        .into_iter()
        .filter(|value| unique.insert(value.clone()))
        .collect())
}

#[derive(Debug, Clone, Copy)]
struct ThinkingSetting(ThinkingLevel);

impl<'de> Deserialize<'de> for ThinkingSetting {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match YamlValue::deserialize(deserializer)? {
            YamlValue::Bool(false) => Ok(Self(ThinkingLevel::Off)),
            YamlValue::String(value) => value
                .trim()
                .to_ascii_lowercase()
                .parse()
                .map(Self)
                .map_err(de::Error::custom),
            YamlValue::Bool(true) => Err(de::Error::custom(
                "thinking: true is ambiguous; use a named thinking level",
            )),
            _ => Err(de::Error::custom(
                "thinking must be false or one of off, minimal, low, medium, high, xhigh, max",
            )),
        }
    }
}

fn load_definition(path: &Path) -> Result<SubagentProfile, SubagentCatalogError> {
    let raw =
        std::fs::read_to_string(path).map_err(|error| definition_error(path, error.to_string()))?;
    if raw.len() > MAX_DEFINITION_BYTES {
        return Err(definition_error(
            path,
            format!("definition exceeds {MAX_DEFINITION_BYTES} bytes"),
        ));
    }
    let (yaml, body) =
        split_frontmatter(&raw).map_err(|message| definition_error(path, message))?;
    let frontmatter: AgentFrontmatter =
        serde_yaml::from_str(&yaml).map_err(|error| definition_error(path, error.to_string()))?;
    let name = frontmatter.name.trim();
    if !valid_name(name) {
        return Err(definition_error(
            path,
            "name must use lowercase letters, digits, hyphens, and optional dot-separated namespaces"
                .to_string(),
        ));
    }
    let description = frontmatter.description.trim();
    if description.is_empty() {
        return Err(definition_error(
            path,
            "description must not be empty".to_string(),
        ));
    }
    if description.len() > 1024 {
        return Err(definition_error(
            path,
            "description exceeds 1024 bytes".to_string(),
        ));
    }
    if body.trim().is_empty() {
        return Err(definition_error(
            path,
            "system prompt body must not be empty".to_string(),
        ));
    }
    let aliases = frontmatter
        .aliases
        .0
        .into_iter()
        .filter(|alias| alias != name)
        .map(|alias| {
            if valid_name(&alias) {
                Ok(alias)
            } else {
                Err(definition_error(
                    path,
                    format!(
                        "alias {alias:?} must use lowercase letters, digits, hyphens, and optional dot-separated namespaces"
                    ),
                ))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let system_prompt_mode = match frontmatter.system_prompt_mode.as_deref() {
        None | Some("replace") => SystemPromptMode::Replace,
        Some("append") => SystemPromptMode::Append,
        Some(value) => {
            return Err(definition_error(
                path,
                format!("systemPromptMode must be append or replace, found {value:?}"),
            ));
        }
    };
    let model = match frontmatter.model {
        None => None,
        Some(model) => {
            let model = model.trim();
            if model.is_empty() {
                return Err(definition_error(
                    path,
                    "model must not be empty; omit it or use inherit".to_string(),
                ));
            }
            (!model.eq_ignore_ascii_case("inherit")).then(|| model.to_string())
        }
    };
    let timeout = match frontmatter.timeout_ms {
        Some(0) => {
            return Err(definition_error(
                path,
                "timeoutMs must be positive".to_string(),
            ));
        }
        Some(timeout_ms) => Some(std::time::Duration::from_millis(timeout_ms)),
        None => None,
    };
    let skill_paths = frontmatter
        .skill_path
        .0
        .into_iter()
        .map(|skill_path| resolve_definition_path(path, &skill_path))
        .collect();
    Ok(SubagentProfile {
        name: name.to_string(),
        aliases,
        description: description.to_string(),
        instructions: body.trim().to_string(),
        system_prompt_mode,
        allow_nested_subagents: frontmatter.allow_nested_subagents,
        max_subagent_depth: frontmatter.max_subagent_depth,
        tools: frontmatter.tools.0,
        excluded_tools: frontmatter.exclude_tools.0,
        model,
        thinking_level: frontmatter.thinking.map(|thinking| thinking.0),
        inherit_skills: frontmatter.inherit_skills,
        skills: frontmatter.skills.0,
        skill_paths,
        timeout,
    })
}

fn resolve_definition_path(definition: &Path, value: &str) -> PathBuf {
    let path = expand_tilde(value);
    let resolved = if path.is_absolute() {
        path
    } else {
        definition
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    };
    normalize_lexically(&absolute(&resolved))
}

fn expand_tilde(value: &str) -> PathBuf {
    if value == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(value));
    }
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(value)
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn split_frontmatter(raw: &str) -> Result<(String, String), String> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let rest = normalized
        .strip_prefix("---\n")
        .ok_or_else(|| "YAML frontmatter must start on the first line".to_string())?;
    let end = rest
        .find("\n---")
        .ok_or_else(|| "unterminated YAML frontmatter".to_string())?;
    let after = &rest[end + 4..];
    if !after.is_empty() && !after.starts_with('\n') {
        return Err("frontmatter closing delimiter must occupy its own line".to_string());
    }
    Ok((
        rest[..end].to_string(),
        after.strip_prefix('\n').unwrap_or(after).trim().to_string(),
    ))
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|segment| {
            !segment.is_empty()
                && segment.chars().next().is_some_and(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit()
                })
                && segment.chars().all(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
                })
        })
}

fn nearest_project_agent_root(cwd: &Path) -> Option<PathBuf> {
    let git_root = cwd
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists());
    for ancestor in cwd.ancestors() {
        let root = ancestor.join(".pi/agents");
        if root.is_dir() {
            return Some(root);
        }
        if git_root == Some(ancestor) {
            break;
        }
    }
    None
}

fn definition_error(path: &Path, message: String) -> SubagentCatalogError {
    SubagentCatalogError::Definition {
        path: path.to_path_buf(),
        message,
    }
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

    fn write_definition(root: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join(format!("{name}.md")), body).unwrap();
    }

    #[test]
    fn project_definitions_override_builtins_and_add_profiles_when_trusted() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let project = directory.path().join("project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        write_definition(
            &project.join(".pi/agents"),
            "scout",
            "---\nname: scout\ndescription: Project scout\nsystemPromptMode: append\n---\nProject-specific scout instructions.",
        );
        write_definition(
            &project.join(".pi/agents"),
            "smoke-delegate",
            "---\nname: smoke-delegate\ndescription: Nested smoke delegate\nallowNestedSubagents: true\n---\nDelegate once.",
        );

        let catalog =
            SubagentCatalog::load(&SubagentLoaderOptions::new(&project, &agent_dir)).unwrap();
        assert_eq!(
            catalog.profile("scout").unwrap().description,
            "Project scout"
        );
        assert!(
            catalog
                .profile("smoke-delegate")
                .unwrap()
                .allow_nested_subagents
        );

        let mut untrusted = SubagentLoaderOptions::new(&project, &agent_dir);
        untrusted.project_trusted = false;
        let catalog = SubagentCatalog::load(&untrusted).unwrap();
        assert_ne!(
            catalog.profile("scout").unwrap().description,
            "Project scout"
        );
        assert!(catalog.profile("smoke-delegate").is_none());
    }

    #[test]
    fn runtime_selection_frontmatter_preserves_inherit_and_empty_semantics() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("agents");
        write_definition(
            &root,
            "configured",
            "---\nname: configured\ndescription: Configured child\ntools: read, grep, read\nmodel: provider/model\nthinking: high\ninheritSkills: true\nmaxSubagentDepth: 4\n---\nInspect code.",
        );
        write_definition(
            &root,
            "inherited",
            "---\nname: inherited\ndescription: Inherited child\ntools:\nmodel: inherit\nthinking: false\n---\nInspect code.",
        );
        let mut options = SubagentLoaderOptions::new(directory.path(), directory.path());
        options.additional_paths.push(root);
        let catalog = SubagentCatalog::load(&options).unwrap();
        let configured = catalog.profile("configured").unwrap();
        assert_eq!(configured.tools, Some(vec!["read".into(), "grep".into()]));
        assert_eq!(configured.model.as_deref(), Some("provider/model"));
        assert_eq!(configured.thinking_level, Some(ThinkingLevel::High));
        assert!(configured.inherit_skills);
        assert_eq!(configured.max_subagent_depth, Some(4));
        let inherited = catalog.profile("inherited").unwrap();
        assert_eq!(inherited.tools, Some(Vec::new()));
        assert_eq!(inherited.model, None);
        assert_eq!(inherited.thinking_level, Some(ThinkingLevel::Off));
        assert!(!inherited.inherit_skills);
        assert_eq!(inherited.max_subagent_depth, None);
    }

    #[test]
    fn catalog_resolves_aliases_and_loads_narrowing_and_skill_policy() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("agents");
        write_definition(
            &root,
            "configured",
            "---\nname: configured\ndescription: Configured child\naliases: explorer, code-scout, explorer\nexcludeTools: bash, write, bash\ntimeoutMs: 900000\ninheritSkills: false\nskills: safe-bash, review-checklist, safe-bash\nskillPath: ./skills, ../shared-skills\n---\nInspect code.",
        );
        let mut options = SubagentLoaderOptions::new(directory.path(), directory.path());
        options.additional_paths.push(root.clone());

        let catalog = SubagentCatalog::load(&options).unwrap();
        let configured = catalog.profile("explorer").unwrap();

        assert_eq!(configured.name, "configured");
        assert_eq!(configured.aliases, vec!["explorer", "code-scout"]);
        assert_eq!(configured.excluded_tools, vec!["bash", "write"]);
        assert_eq!(
            configured.timeout,
            Some(std::time::Duration::from_millis(900_000))
        );
        assert!(!configured.inherit_skills);
        assert_eq!(configured.skills, vec!["safe-bash", "review-checklist"]);
        assert_eq!(
            configured.skill_paths,
            vec![root.join("skills"), directory.path().join("shared-skills")]
        );
        assert!(catalog.profile_names().contains(&"explorer".to_string()));
    }

    #[test]
    fn canonical_names_win_over_aliases_but_alias_alias_collisions_fail() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("agents");
        write_definition(
            &root,
            "first",
            "---\nname: first\ndescription: First\naliases: shared, second\n---\nFirst.",
        );
        write_definition(
            &root,
            "second",
            "---\nname: second\ndescription: Second\n---\nSecond.",
        );
        let mut options = SubagentLoaderOptions::new(directory.path(), directory.path());
        options.additional_paths.push(root.clone());

        let catalog = SubagentCatalog::load(&options).unwrap();
        assert_eq!(catalog.profile("second").unwrap().name, "second");
        assert_eq!(catalog.profile("shared").unwrap().name, "first");

        write_definition(
            &root,
            "third",
            "---\nname: third\ndescription: Third\naliases: shared\n---\nThird.",
        );
        let error = SubagentCatalog::load(&options).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("ambiguous subagent alias \"shared\"")
        );
    }

    #[test]
    fn timeout_must_be_positive() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("agents");
        write_definition(
            &root,
            "invalid-timeout",
            "---\nname: invalid-timeout\ndescription: Invalid timeout\ntimeoutMs: 0\n---\nInspect code.",
        );
        let mut options = SubagentLoaderOptions::new(directory.path(), directory.path());
        options.additional_paths.push(root);
        let error = SubagentCatalog::load(&options).unwrap_err();
        assert!(error.to_string().contains("timeoutMs must be positive"));
    }

    #[test]
    fn invalid_max_subagent_depth_fails_definition_loading() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("agents");
        write_definition(
            &root,
            "invalid-depth",
            "---\nname: invalid-depth\ndescription: Invalid nesting limit\nmaxSubagentDepth: -1\n---\nInspect code.",
        );
        let mut options = SubagentLoaderOptions::new(directory.path(), directory.path());
        options.additional_paths.push(root);
        let error = SubagentCatalog::load(&options).unwrap_err();
        assert!(error.to_string().contains("maxSubagentDepth"));
    }

    #[test]
    fn unsupported_frontmatter_still_fails_instead_of_being_silently_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("agents");
        write_definition(
            &root,
            "unsupported",
            "---\nname: unsupported\ndescription: Unsupported fallback policy\nfallbackModels: other/model\n---\nInspect code.",
        );
        let mut options = SubagentLoaderOptions::new(directory.path(), directory.path());
        options.additional_paths.push(root);
        let error = SubagentCatalog::load(&options).unwrap_err();
        assert!(error.to_string().contains("unknown field `fallbackModels`"));
    }
}
