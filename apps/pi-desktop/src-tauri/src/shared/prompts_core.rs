use pi_utils::frontmatter::{parse_frontmatter as parse_yaml_frontmatter, split_frontmatter};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;
use tokio::task;

use crate::agent_paths;
use crate::types::WorkspaceEntry;

#[derive(Serialize, Clone)]
pub(crate) struct CustomPromptEntry {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) description: Option<String>,
    #[serde(rename = "argumentHint")]
    pub(crate) argument_hint: Option<String>,
    pub(crate) content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scope: Option<String>,
}

fn default_prompts_dir() -> Option<PathBuf> {
    agent_paths::agent_dir()
        .ok()
        .map(|home| home.join("prompts"))
}

fn require_workspace_entry(
    workspaces: &HashMap<String, WorkspaceEntry>,
    workspace_id: &str,
) -> Result<WorkspaceEntry, String> {
    workspaces
        .get(workspace_id)
        .cloned()
        .ok_or_else(|| "workspace not found".to_string())
}

fn app_data_dir(settings_path: &Path) -> Result<PathBuf, String> {
    settings_path
        .parent()
        .map(|path| path.to_path_buf())
        .ok_or_else(|| "Unable to resolve app data dir.".to_string())
}

fn workspace_prompts_dir(settings_path: &Path, entry: &WorkspaceEntry) -> Result<PathBuf, String> {
    let data_dir = app_data_dir(settings_path)?;
    Ok(data_dir.join("workspaces").join(&entry.id).join("prompts"))
}

fn prompt_roots_for_workspace(
    settings_path: &Path,
    entry: &WorkspaceEntry,
) -> Result<Vec<PathBuf>, String> {
    let mut roots = Vec::new();
    roots.push(workspace_prompts_dir(settings_path, entry)?);
    if let Some(global_dir) = default_prompts_dir() {
        roots.push(global_dir);
    }
    Ok(roots)
}

fn ensure_path_within_roots(path: &Path, roots: &[PathBuf]) -> Result<(), String> {
    let canonical_path = path
        .canonicalize()
        .map_err(|_| "Invalid prompt path.".to_string())?;
    for root in roots {
        if let Ok(canonical_root) = root.canonicalize() {
            if canonical_path.starts_with(&canonical_root) {
                return Ok(());
            }
        }
    }
    Err("Prompt path is not within allowed directories.".to_string())
}

#[cfg(unix)]
fn is_cross_device_error(err: &std::io::Error) -> bool {
    err.raw_os_error() == Some(libc::EXDEV)
}

#[cfg(not(unix))]
fn is_cross_device_error(_err: &std::io::Error) -> bool {
    false
}

fn move_file(src: &Path, dest: &Path) -> Result<(), String> {
    match fs::rename(src, dest) {
        Ok(()) => Ok(()),
        Err(err) if is_cross_device_error(&err) => {
            fs::copy(src, dest).map_err(|err| err.to_string())?;
            fs::remove_file(src).map_err(|err| err.to_string())
        }
        Err(err) => Err(err.to_string()),
    }
}

#[derive(Default, Deserialize)]
struct PromptFrontmatter {
    description: Option<PromptString>,
    #[serde(rename = "argument-hint", alias = "argument_hint")]
    argument_hint: Option<PromptString>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PromptFrontmatterValue {
    Fields(PromptFrontmatter),
    Other(serde::de::IgnoredAny),
}

impl PromptFrontmatterValue {
    fn into_fields(self) -> PromptFrontmatter {
        match self {
            Self::Fields(fields) => fields,
            Self::Other(_) => PromptFrontmatter::default(),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PromptString {
    String(String),
    Other(serde::de::IgnoredAny),
}

impl PromptString {
    fn into_string(self) -> Option<String> {
        match self {
            Self::String(value) => Some(value),
            Self::Other(_) => None,
        }
    }
}

fn parse_prompt_document(content: &str) -> (Option<String>, Option<String>, String) {
    match parse_yaml_frontmatter::<PromptFrontmatterValue>(content) {
        Ok(document) => {
            let frontmatter = document
                .frontmatter
                .map(PromptFrontmatterValue::into_fields)
                .unwrap_or_default();
            (
                frontmatter.description.and_then(PromptString::into_string),
                frontmatter
                    .argument_hint
                    .and_then(PromptString::into_string),
                document.body,
            )
        }
        Err(_) => (None, None, split_frontmatter(content).body),
    }
}

fn build_prompt_contents(
    description: Option<String>,
    argument_hint: Option<String>,
    content: String,
) -> String {
    let has_meta = description
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty())
        || argument_hint
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty());
    if !has_meta {
        return content;
    }
    let mut output = String::from("---\n");
    if let Some(description) = description {
        let trimmed = description.trim();
        if !trimmed.is_empty() {
            output.push_str(&format!(
                "description: \"{}\"\n",
                trimmed.replace('"', "\\\"")
            ));
        }
    }
    if let Some(argument_hint) = argument_hint {
        let trimmed = argument_hint.trim();
        if !trimmed.is_empty() {
            output.push_str(&format!(
                "argument-hint: \"{}\"\n",
                trimmed.replace('"', "\\\"")
            ));
        }
    }
    output.push_str("---\n");
    output.push_str(&content);
    output
}

fn sanitize_prompt_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Prompt name is required.".to_string());
    }
    if trimmed.chars().any(|ch| ch.is_whitespace()) {
        return Err("Prompt name cannot include whitespace.".to_string());
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err("Prompt name cannot include path separators.".to_string());
    }
    Ok(trimmed.to_string())
}

fn discover_prompts_in(dir: &Path, scope: Option<&str>) -> Vec<CustomPromptEntry> {
    let mut out: Vec<CustomPromptEntry> = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return out,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let is_file = fs::metadata(&path).map(|m| m.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        let is_md = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("md"))
            .unwrap_or(false);
        if !is_md {
            continue;
        }
        let Some(name) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let (description, argument_hint, body) = parse_prompt_document(&content);
        out.push(CustomPromptEntry {
            name,
            path: path.to_string_lossy().to_string(),
            description,
            argument_hint,
            content: body,
            scope: scope.map(|value| value.to_string()),
        });
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub(crate) async fn prompts_list_core(
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    settings_path: &Path,
    workspace_id: String,
) -> Result<Vec<CustomPromptEntry>, String> {
    let (workspace_dir, global_dir) = {
        let workspaces = workspaces.lock().await;
        let entry = workspaces.get(&workspace_id).cloned();
        let workspace_dir = entry
            .as_ref()
            .and_then(|entry| workspace_prompts_dir(settings_path, entry).ok());
        let global_dir = entry.as_ref().and_then(|_| default_prompts_dir());
        (workspace_dir, global_dir)
    };

    task::spawn_blocking(move || {
        let mut out = Vec::new();
        if let Some(dir) = workspace_dir {
            let _ = fs::create_dir_all(&dir);
            out.extend(discover_prompts_in(&dir, Some("workspace")));
        }
        if let Some(dir) = global_dir {
            let _ = fs::create_dir_all(&dir);
            out.extend(discover_prompts_in(&dir, Some("global")));
        }
        out
    })
    .await
    .map_err(|_| "prompt discovery failed".to_string())
}

pub(crate) async fn prompts_workspace_dir_core(
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    settings_path: &Path,
    workspace_id: String,
) -> Result<String, String> {
    let dir = {
        let workspaces = workspaces.lock().await;
        let entry = require_workspace_entry(&workspaces, &workspace_id)?;
        workspace_prompts_dir(settings_path, &entry)?
    };
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    Ok(dir.to_string_lossy().to_string())
}

pub(crate) async fn prompts_global_dir_core(
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    workspace_id: String,
) -> Result<String, String> {
    let workspaces = workspaces.lock().await;
    require_workspace_entry(&workspaces, &workspace_id)?;
    let dir = default_prompts_dir().ok_or("Unable to resolve Pi agent directory".to_string())?;
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    Ok(dir.to_string_lossy().to_string())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn prompts_create_core(
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    settings_path: &Path,
    workspace_id: String,
    scope: String,
    name: String,
    description: Option<String>,
    argument_hint: Option<String>,
    content: String,
) -> Result<CustomPromptEntry, String> {
    let name = sanitize_prompt_name(&name)?;
    let (target_dir, resolved_scope) = {
        let workspaces = workspaces.lock().await;
        let entry = require_workspace_entry(&workspaces, &workspace_id)?;
        match scope.as_str() {
            "workspace" => {
                let dir = workspace_prompts_dir(settings_path, &entry)?;
                (dir, "workspace")
            }
            "global" => {
                let dir = default_prompts_dir()
                    .ok_or("Unable to resolve Pi agent directory".to_string())?;
                (dir, "global")
            }
            _ => return Err("Invalid scope.".to_string()),
        }
    };
    let path = target_dir.join(format!("{name}.md"));
    if path.exists() {
        return Err("Prompt already exists.".to_string());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let body = build_prompt_contents(description.clone(), argument_hint.clone(), content.clone());
    fs::write(&path, body).map_err(|err| err.to_string())?;
    Ok(CustomPromptEntry {
        name,
        path: path.to_string_lossy().to_string(),
        description,
        argument_hint,
        content,
        scope: Some(resolved_scope.to_string()),
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn prompts_update_core(
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    settings_path: &Path,
    workspace_id: String,
    path: String,
    name: String,
    description: Option<String>,
    argument_hint: Option<String>,
    content: String,
) -> Result<CustomPromptEntry, String> {
    let name = sanitize_prompt_name(&name)?;
    let target_path = PathBuf::from(&path);
    if !target_path.exists() {
        return Err("Prompt not found.".to_string());
    }
    {
        let workspaces = workspaces.lock().await;
        let entry = require_workspace_entry(&workspaces, &workspace_id)?;
        let roots = prompt_roots_for_workspace(settings_path, &entry)?;
        ensure_path_within_roots(&target_path, &roots)?;
    }
    let dir = target_path
        .parent()
        .ok_or("Unable to resolve prompt directory.".to_string())?;
    let next_path = dir.join(format!("{name}.md"));
    if next_path != target_path && next_path.exists() {
        return Err("Prompt with that name already exists.".to_string());
    }
    let body = build_prompt_contents(description.clone(), argument_hint.clone(), content.clone());
    fs::write(&next_path, body).map_err(|err| err.to_string())?;
    if next_path != target_path {
        fs::remove_file(&target_path).map_err(|err| err.to_string())?;
    }
    let scope = {
        let workspaces = workspaces.lock().await;
        let entry = require_workspace_entry(&workspaces, &workspace_id)?;
        let workspace_dir = workspace_prompts_dir(settings_path, &entry)?;
        if next_path.starts_with(&workspace_dir) {
            Some("workspace".to_string())
        } else {
            Some("global".to_string())
        }
    };
    Ok(CustomPromptEntry {
        name,
        path: next_path.to_string_lossy().to_string(),
        description,
        argument_hint,
        content,
        scope,
    })
}

pub(crate) async fn prompts_delete_core(
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    settings_path: &Path,
    workspace_id: String,
    path: String,
) -> Result<(), String> {
    let target = PathBuf::from(path);
    if !target.exists() {
        return Ok(());
    }
    {
        let workspaces = workspaces.lock().await;
        let entry = require_workspace_entry(&workspaces, &workspace_id)?;
        let roots = prompt_roots_for_workspace(settings_path, &entry)?;
        ensure_path_within_roots(&target, &roots)?;
    }
    fs::remove_file(&target).map_err(|err| err.to_string())
}

pub(crate) async fn prompts_move_core(
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    settings_path: &Path,
    workspace_id: String,
    path: String,
    scope: String,
) -> Result<CustomPromptEntry, String> {
    let target_path = PathBuf::from(&path);
    if !target_path.exists() {
        return Err("Prompt not found.".to_string());
    }
    let roots = {
        let workspaces = workspaces.lock().await;
        let entry = require_workspace_entry(&workspaces, &workspace_id)?;
        prompt_roots_for_workspace(settings_path, &entry)?
    };
    ensure_path_within_roots(&target_path, &roots)?;
    let file_name = target_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("Invalid prompt path.".to_string())?;
    let target_dir = {
        let workspaces = workspaces.lock().await;
        let entry = require_workspace_entry(&workspaces, &workspace_id)?;
        match scope.as_str() {
            "workspace" => workspace_prompts_dir(settings_path, &entry)?,
            "global" => {
                default_prompts_dir().ok_or("Unable to resolve Pi agent directory".to_string())?
            }
            _ => return Err("Invalid scope.".to_string()),
        }
    };
    let next_path = target_dir.join(file_name);
    if next_path == target_path {
        return Err("Prompt is already in that scope.".to_string());
    }
    if next_path.exists() {
        return Err("Prompt with that name already exists.".to_string());
    }
    if let Some(parent) = next_path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    move_file(&target_path, &next_path)?;
    let content = fs::read_to_string(&next_path).unwrap_or_default();
    let (description, argument_hint, body) = parse_prompt_document(&content);
    let name = next_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_string();
    Ok(CustomPromptEntry {
        name,
        path: next_path.to_string_lossy().to_string(),
        description,
        argument_hint,
        content: body,
        scope: Some(scope),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_metadata_uses_the_shared_frontmatter_parser() {
        let (description, argument_hint, body) = parse_prompt_document(
            "\u{feff}---\r\ndescription: |\r\n  Review carefully\r\nargument_hint: <file>\r\n---\r\n\r\nReview it.\r\n",
        );

        assert_eq!(description.as_deref(), Some("Review carefully\n"));
        assert_eq!(argument_hint.as_deref(), Some("<file>"));
        assert_eq!(body, "Review it.");
    }

    #[test]
    fn non_mapping_prompt_frontmatter_is_ignored() {
        let (description, argument_hint, body) =
            parse_prompt_document("---\nmetadata\n---\nReview it.");

        assert_eq!(description, None);
        assert_eq!(argument_hint, None);
        assert_eq!(body, "Review it.");
    }
}
