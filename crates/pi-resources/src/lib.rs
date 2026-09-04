#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use pi_prompt::{BuildSystemPromptOptions, ContextFile};
use serde::{Deserialize, Serialize};

pub const CONFIG_DIR_NAME: &str = ".pi";
const CONTEXT_CANDIDATES: &[&str] = &[
    "HERMES.md",
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLoaderOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub project_trusted: bool,
    pub system_prompt: Option<String>,
    pub append_system_prompts: Option<Vec<String>>,
    pub load_context_files: bool,
}

impl ResourceLoaderOptions {
    pub fn new(cwd: impl Into<PathBuf>, agent_dir: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            agent_dir: agent_dir.into(),
            project_trusted: false,
            system_prompt: None,
            append_system_prompts: None,
            load_context_files: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    Warning,
    Collision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDiagnostic {
    pub kind: DiagnosticKind,
    pub message: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct LoadedResources {
    pub system_prompt: Option<String>,
    pub system_prompt_source: Option<PathBuf>,
    pub append_system_prompts: Vec<String>,
    pub append_system_prompt_sources: Vec<PathBuf>,
    pub context_files: Vec<ContextFile>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

impl LoadedResources {
    pub fn apply_to_prompt(&self, options: &mut BuildSystemPromptOptions) {
        options.custom_prompt.clone_from(&self.system_prompt);
        options.append_system_prompt = (!self.append_system_prompts.is_empty())
            .then(|| self.append_system_prompts.join("\n\n"));
        options.context_files.clone_from(&self.context_files);
    }
}

pub fn load_resources(options: &ResourceLoaderOptions) -> LoadedResources {
    let cwd = absolute(&options.cwd);
    let agent_dir = absolute(&options.agent_dir);
    let (system_prompt, system_prompt_source) = load_prompt_source(
        options.system_prompt.as_deref(),
        discover_prompt(&cwd, &agent_dir, options.project_trusted, "SYSTEM.md").as_deref(),
    );
    let append_sources = options.append_system_prompts.clone().unwrap_or_else(|| {
        discover_prompt(
            &cwd,
            &agent_dir,
            options.project_trusted,
            "APPEND_SYSTEM.md",
        )
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
    });
    let mut append_system_prompts = Vec::new();
    let mut append_system_prompt_sources = Vec::new();
    for source in append_sources {
        let path = Path::new(&source);
        if path.exists() {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    append_system_prompts.push(content);
                    append_system_prompt_sources.push(absolute(path));
                }
                Err(_) => append_system_prompts.push(source),
            }
        } else {
            append_system_prompts.push(source);
        }
    }
    let context_files = if options.load_context_files {
        load_project_context_files(&cwd, &agent_dir)
    } else {
        Vec::new()
    };
    LoadedResources {
        system_prompt,
        system_prompt_source,
        append_system_prompts,
        append_system_prompt_sources,
        context_files,
        diagnostics: Vec::new(),
    }
}

fn discover_prompt(cwd: &Path, agent_dir: &Path, trusted: bool, name: &str) -> Option<PathBuf> {
    let project = cwd.join(CONFIG_DIR_NAME).join(name);
    if trusted && project.is_file() {
        return Some(project);
    }
    let global = agent_dir.join(name);
    global.is_file().then_some(global)
}

fn load_prompt_source(
    explicit: Option<&str>,
    discovered: Option<&Path>,
) -> (Option<String>, Option<PathBuf>) {
    if let Some(value) = explicit {
        let path = Path::new(value);
        if path.exists() {
            return match std::fs::read_to_string(path) {
                Ok(content) => (Some(content), Some(absolute(path))),
                Err(_) => (Some(value.to_string()), None),
            };
        }
        return (Some(value.to_string()), None);
    }
    discovered.map_or((None, None), |path| match std::fs::read_to_string(path) {
        Ok(content) => (Some(content), Some(absolute(path))),
        Err(_) => (Some(path.to_string_lossy().into_owned()), None),
    })
}

pub fn load_project_context_files(cwd: &Path, agent_dir: &Path) -> Vec<ContextFile> {
    let cwd = absolute(cwd);
    let agent_dir = absolute(agent_dir);
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    if let Some(file) = load_context_from_dir(&agent_dir) {
        seen.insert(absolute(&file.path));
        output.push(file);
    }
    let mut ancestors = Vec::new();
    let mut current = Some(cwd.as_path());
    while let Some(dir) = current {
        if let Some(file) = load_context_from_dir(dir) {
            let path = absolute(&file.path);
            if seen.insert(path) {
                ancestors.push(file);
            }
        }
        current = dir.parent();
    }
    ancestors.reverse();
    output.extend(ancestors);
    output
}

fn load_context_from_dir(dir: &Path) -> Option<ContextFile> {
    CONTEXT_CANDIDATES.iter().find_map(|name| {
        let path = dir.join(name);
        path.is_file()
            .then(|| {
                std::fs::read_to_string(&path)
                    .ok()
                    .map(|content| ContextFile {
                        path: absolute(&path),
                        content,
                    })
            })
            .flatten()
    })
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
    #[test]
    fn prompt_precedence_respects_trust() {
        let root = tempdir();
        let project = root.join("project");
        let agent = root.join("agent");
        std::fs::create_dir_all(project.join(".pi")).unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(project.join(".pi/SYSTEM.md"), "project").unwrap();
        std::fs::write(agent.join("SYSTEM.md"), "global").unwrap();
        let mut o = ResourceLoaderOptions::new(&project, &agent);
        assert_eq!(load_resources(&o).system_prompt.as_deref(), Some("global"));
        o.project_trusted = true;
        assert_eq!(load_resources(&o).system_prompt.as_deref(), Some("project"));
    }
    #[test]
    fn context_is_global_then_root_to_cwd() {
        let root = tempdir();
        let agent = root.join("agent");
        let child = root.join("a/b");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(agent.join("AGENTS.md"), "g").unwrap();
        std::fs::write(root.join("AGENTS.md"), "r").unwrap();
        std::fs::write(root.join("a/AGENTS.md"), "a").unwrap();
        let values = load_project_context_files(&child, &agent)
            .into_iter()
            .map(|f| f.content)
            .collect::<Vec<_>>();
        assert_eq!(values, vec!["g", "r", "a"]);
    }

    #[test]
    fn hermes_md_is_the_preferred_repository_context_file() {
        let root = tempdir();
        let agent = root.join("agent");
        let project = root.join("project");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("HERMES.md"), "hermes").unwrap();
        std::fs::write(project.join("AGENTS.md"), "agents").unwrap();

        let contexts = load_project_context_files(&project, &agent);

        let project_contexts = contexts
            .iter()
            .filter(|context| context.path.starts_with(&project))
            .collect::<Vec<_>>();
        assert_eq!(project_contexts.len(), 1);
        assert_eq!(project_contexts[0].content, "hermes");
        assert_eq!(project_contexts[0].path, project.join("HERMES.md"));
    }
    fn tempdir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "pi-resources-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
