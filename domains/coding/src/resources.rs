//! Coding project-context and system-prompt discovery.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use pi_runtime::{DiagnosticKind, ResourceDiagnostic};
use pi_utils::path::absolute_from_current_dir as absolute;
use serde::{Deserialize, Serialize};

use crate::prompt::{BuildSystemPromptOptions, ContextFile};

const CONFIG_DIR_NAME: &str = ".pi";
const CONTEXT_CANDIDATES: &[&str] = &[
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResourceLoaderOptions {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub project_trusted: bool,
    pub system_prompt: Option<String>,
    pub append_system_prompts: Option<Vec<String>>,
    pub load_context_files: bool,
    #[serde(default = "default_hermes_context")]
    pub include_hermes_context: bool,
}

fn default_hermes_context() -> bool {
    true
}

impl ResourceLoaderOptions {
    pub(crate) fn new(cwd: impl Into<PathBuf>, agent_dir: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            agent_dir: agent_dir.into(),
            project_trusted: false,
            system_prompt: None,
            append_system_prompts: None,
            load_context_files: true,
            include_hermes_context: true,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LoadedResources {
    pub system_prompt: Option<String>,
    pub append_system_prompts: Vec<String>,
    pub context_files: Vec<ContextFile>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

impl LoadedResources {
    pub(crate) fn apply_to_prompt(&self, options: &mut BuildSystemPromptOptions) {
        options.custom_prompt.clone_from(&self.system_prompt);
        options.append_system_prompt = self.append_system_prompts.join("\n\n");
        options.context_files.clone_from(&self.context_files);
    }
}

pub(crate) fn load_resources(options: &ResourceLoaderOptions) -> LoadedResources {
    let cwd = absolute(&options.cwd);
    let agent_dir = absolute(&options.agent_dir);
    let mut diagnostics = Vec::new();
    let discovered = discover_prompt(&cwd, &agent_dir, options.project_trusted, "SYSTEM.md");
    let discovered = discovered.as_ref().map(|path| path.to_string_lossy());
    let system_prompt = load_prompt_source(
        options.system_prompt.as_deref().or(discovered.as_deref()),
        &mut diagnostics,
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
    let append_system_prompts = append_sources
        .iter()
        .filter_map(|source| load_prompt_source(Some(source), &mut diagnostics))
        .collect();
    let context_files = if options.load_context_files {
        load_context_files(
            &cwd,
            &agent_dir,
            options.include_hermes_context,
            &mut diagnostics,
        )
    } else {
        Vec::new()
    };
    LoadedResources {
        system_prompt,
        append_system_prompts,
        context_files,
        diagnostics,
    }
}

fn discover_prompt(cwd: &Path, agent_dir: &Path, trusted: bool, name: &str) -> Option<PathBuf> {
    let project = cwd.join(CONFIG_DIR_NAME).join(name);
    if trusted && project.exists() {
        return Some(project);
    }
    let global = agent_dir.join(name);
    global.exists().then_some(global)
}

fn load_prompt_source(
    source: Option<&str>,
    diagnostics: &mut Vec<ResourceDiagnostic>,
) -> Option<String> {
    let value = source.filter(|value| !value.is_empty())?;
    let path = Path::new(value);
    if path.exists() {
        match read_text(path, diagnostics) {
            Some(content) => return Some(content),
            None => return Some(value.to_string()),
        }
    }
    Some(value.to_string())
}

#[cfg(test)]
fn load_project_context_files(cwd: &Path, agent_dir: &Path) -> Vec<ContextFile> {
    load_context_files(cwd, agent_dir, true, &mut Vec::new())
}

fn load_context_files(
    cwd: &Path,
    agent_dir: &Path,
    include_hermes: bool,
    diagnostics: &mut Vec<ResourceDiagnostic>,
) -> Vec<ContextFile> {
    let cwd = absolute(cwd);
    let agent_dir = absolute(agent_dir);
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    if let Some(file) = load_context_from_dir(&agent_dir, include_hermes, diagnostics) {
        seen.insert(absolute(&file.path));
        output.push(file);
    }
    let mut ancestors = Vec::new();
    let shadowed_context = find_shadowed_context_file(&cwd, include_hermes, diagnostics);
    let mut current = Some(cwd.as_path());
    while let Some(dir) = current {
        if let Some(file) = load_context_from_dir(dir, include_hermes, diagnostics) {
            let path = absolute(&file.path);
            // Compare the directory, rather than resolving the file symlink:
            // a linked worktree may point its instructions at the main copy.
            let shadowed = shadowed_context.as_ref().is_some_and(|shadowed| {
                file.path
                    .file_name()
                    .is_some_and(|name| paths_equal(&canonicalize(dir).join(name), shadowed))
            });
            if !shadowed && seen.insert(path) {
                ancestors.push(file);
            }
        }
        current = dir.parent();
    }
    ancestors.reverse();
    output.extend(ancestors);
    output
}

fn load_context_from_dir(
    dir: &Path,
    include_hermes: bool,
    diagnostics: &mut Vec<ResourceDiagnostic>,
) -> Option<ContextFile> {
    include_hermes
        .then_some("HERMES.md")
        .into_iter()
        .chain(CONTEXT_CANDIDATES.iter().copied())
        .find_map(|name| {
            let path = dir.join(name);
            path.is_file()
                .then(|| {
                    read_text(&path, diagnostics).map(|content| ContextFile {
                        path: absolute(&path),
                        content,
                    })
                })
                .flatten()
        })
}

fn read_text(path: &Path, diagnostics: &mut Vec<ResourceDiagnostic>) -> Option<String> {
    match std::fs::read(path) {
        Ok(content) => {
            let content = String::from_utf8_lossy(&content);
            Some(
                content
                    .strip_prefix('\u{feff}')
                    .unwrap_or(&content)
                    .to_string(),
            )
        }
        Err(error) => {
            diagnostics.push(ResourceDiagnostic {
                kind: DiagnosticKind::Warning,
                message: format!("Could not read {}: {error}", path.display()),
                path: path.to_path_buf(),
            });
            None
        }
    }
}

fn canonicalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

/// A nested linked worktree replaces the main checkout's same-scope context.
/// Ordinary ancestors, sibling worktrees, bare layouts and submodules retain
/// their normal inheritance. Git metadata is read without spawning Git.
fn find_shadowed_context_file(
    cwd: &Path,
    include_hermes: bool,
    diagnostics: &mut Vec<ResourceDiagnostic>,
) -> Option<PathBuf> {
    let (worktree_root, common_git_dir) = find_git_paths(cwd)?;
    let worktree_root = canonicalize(&worktree_root);
    let common_git_dir = canonicalize(&common_git_dir);
    let main_repo_root = common_git_dir.parent()?;
    let nested = worktree_root
        .ancestors()
        .skip(1)
        .any(|ancestor| paths_equal(ancestor, main_repo_root));
    if !nested || !paths_equal(&canonicalize(&main_repo_root.join(".git")), &common_git_dir) {
        return None;
    }
    let context = load_context_from_dir(&worktree_root, include_hermes, diagnostics)?;
    Some(main_repo_root.join(context.path.file_name()?))
}

fn find_git_paths(cwd: &Path) -> Option<(PathBuf, PathBuf)> {
    for directory in cwd.ancestors() {
        let dot_git = directory.join(".git");
        if dot_git.is_dir() {
            return dot_git
                .join("HEAD")
                .exists()
                .then(|| (directory.to_path_buf(), dot_git));
        }
        if dot_git.is_file() {
            let contents = std::fs::read_to_string(&dot_git).ok()?;
            let Some(git_dir) = contents.trim().strip_prefix("gitdir: ") else {
                continue;
            };
            let git_dir = directory.join(git_dir.trim());
            if !git_dir.join("HEAD").exists() {
                return None;
            }
            let common = git_dir.join("commondir");
            let common_git_dir = if common.exists() {
                git_dir.join(std::fs::read_to_string(common).ok()?.trim())
            } else {
                git_dir
            };
            return Some((directory.to_path_buf(), common_git_dir));
        }
    }
    None
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
    fn explicit_prompt_inputs_replace_discovery_and_remove_file_boms() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        let agent = directory.path().join("agent");
        std::fs::create_dir_all(project.join(".pi")).unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(project.join(".pi/SYSTEM.md"), "project system").unwrap();
        std::fs::write(project.join(".pi/APPEND_SYSTEM.md"), "project append").unwrap();
        std::fs::write(agent.join("APPEND_SYSTEM.md"), "global append").unwrap();
        std::fs::write(project.join("AGENTS.md"), "\u{feff}project rules").unwrap();
        let custom = directory.path().join("custom.md");
        std::fs::write(&custom, "\u{feff}custom instructions").unwrap();

        let mut options = ResourceLoaderOptions::new(&project, &agent);
        options.project_trusted = true;
        assert_eq!(
            load_resources(&options).append_system_prompts,
            ["project append"]
        );
        options.system_prompt = Some(custom.to_string_lossy().into_owned());
        options.append_system_prompts = Some(vec![String::new(), "inline append".into()]);
        let resources = load_resources(&options);
        assert_eq!(
            resources.system_prompt.as_deref(),
            Some("custom instructions")
        );
        assert_eq!(resources.append_system_prompts, ["inline append"]);
        assert_eq!(
            resources.context_files.last().unwrap().content,
            "project rules"
        );

        options.system_prompt = Some(String::new());
        options.append_system_prompts = Some(Vec::new());
        let resources = load_resources(&options);
        assert!(resources.system_prompt.is_none());
        assert!(resources.append_system_prompts.is_empty());
    }

    #[test]
    fn nested_worktree_context_replaces_main_repository_copy() {
        let directory = tempfile::tempdir().unwrap();
        let main = directory.path().join("main");
        let worktree = main.join("worktrees/feature");
        let cwd = worktree.join("src");
        let agent = directory.path().join("agent");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        make_worktree(&main, &worktree);
        std::fs::write(main.join("AGENTS.md"), "main rules").unwrap();
        std::fs::write(main.join("worktrees/AGENTS.md"), "intermediate rules").unwrap();
        std::fs::write(worktree.join("AGENTS.md"), "worktree rules").unwrap();

        let contexts = load_project_context_files(&cwd, &agent);
        let values = contexts
            .iter()
            .map(|file| file.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(values, ["intermediate rules", "worktree rules"]);

        std::fs::remove_file(worktree.join("AGENTS.md")).unwrap();
        let contexts = load_project_context_files(&cwd, &agent);
        let values = contexts
            .iter()
            .map(|file| file.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(values, ["main rules", "intermediate rules"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_worktree_context_is_loaded_once() {
        let directory = tempfile::tempdir().unwrap();
        let main = directory.path().join("main");
        let worktree = main.join("feature");
        let agent = directory.path().join("agent");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        make_worktree(&main, &worktree);
        std::fs::write(main.join("AGENTS.md"), "shared rules").unwrap();
        std::os::unix::fs::symlink(main.join("AGENTS.md"), worktree.join("AGENTS.md")).unwrap();
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(&main, &alias).unwrap();

        let contexts = load_project_context_files(&alias.join("feature"), &agent);
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].content, "shared rules");
        assert_eq!(contexts[0].path, alias.join("feature/AGENTS.md"));
    }

    #[test]
    fn bare_repository_layout_keeps_parent_context() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("project");
        let worktree = parent.join("feature");
        let git_dir = parent.join(".bare/worktrees/feature");
        let agent = directory.path().join("agent");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature").unwrap();
        std::fs::write(git_dir.join("commondir"), "../..").unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}", git_dir.display()),
        )
        .unwrap();
        std::fs::write(parent.join("AGENTS.md"), "parent rules").unwrap();
        std::fs::write(worktree.join("AGENTS.md"), "worktree rules").unwrap();

        let contexts = load_project_context_files(&worktree, &agent);
        let values = contexts
            .iter()
            .map(|file| file.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(values, ["parent rules", "worktree rules"]);
    }

    fn make_worktree(main: &Path, worktree: &Path) {
        let git_dir = main.join(".git/worktrees/feature");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(main.join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature").unwrap();
        std::fs::write(git_dir.join("commondir"), "../..").unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}", git_dir.display()),
        )
        .unwrap();
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

        let mut options = ResourceLoaderOptions::new(&project, &agent);
        options.include_hermes_context = false;
        let contexts = load_resources(&options).context_files;
        let project_context = contexts
            .iter()
            .find(|context| context.path.starts_with(&project))
            .unwrap();
        assert_eq!(project_context.path, project.join("AGENTS.md"));
        assert_eq!(project_context.content, "agents");
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
