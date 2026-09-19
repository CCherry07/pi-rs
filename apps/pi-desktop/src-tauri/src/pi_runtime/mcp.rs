//! Thin Desktop MCP configuration adapter. No sessions are constructed here.
use std::path::Path;

use pi_plugin_mcp::{McpDocument, McpLibrary, McpScope};
use pi_sdk::{ProjectTrustEvaluation, ProjectTrustService};
use tauri::State;

use super::{workspace_path, PiRuntimeState};
use crate::state::AppState;

async fn library(
    workspace_id: Option<String>,
    state: &AppState,
    pi: &PiRuntimeState,
) -> Result<McpLibrary, String> {
    let cwd = match workspace_id {
        Some(id) => Some(workspace_path(state, &id).await?),
        None => None,
    };
    let agent_dir = pi.info.agent_dir.clone();
    let trust = pi.project_trust.clone();
    tokio::task::spawn_blocking(move || desktop_library(&agent_dir, cwd.as_deref(), &trust))
        .await
        .map_err(|e| e.to_string())?
}

/// Consumes the existing project decision without prompting or constructing a session.
fn desktop_library(
    agent_dir: &Path,
    cwd: Option<&Path>,
    trust: &ProjectTrustService,
) -> Result<McpLibrary, String> {
    let trusted = match cwd {
        Some(cwd) => matches!(
            trust.evaluate(cwd).map_err(|error| error.to_string())?,
            ProjectTrustEvaluation::Known(true)
        ),
        None => false,
    };
    Ok(McpLibrary::new(agent_dir, cwd, trusted))
}

#[tauri::command]
pub(crate) async fn pi_mcp_read(
    workspace_id: Option<String>,
    scope: McpScope,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<McpDocument, String> {
    let library = library(workspace_id, &state, &pi).await?;
    tokio::task::spawn_blocking(move || library.read(scope))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn pi_mcp_save(
    workspace_id: Option<String>,
    scope: McpScope,
    revision: String,
    content: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<McpDocument, String> {
    let library = library(workspace_id, &state, &pi).await?;
    tokio::task::spawn_blocking(move || library.save(scope, &revision, &content))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn pi_mcp_test(
    workspace_id: Option<String>,
    name: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Vec<String>, String> {
    let library = library(workspace_id, &state, &pi).await?;
    library.test(&name).await
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    #[test]
    fn unknown_project_trust_does_not_prompt_or_read_project_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let agent = directory.path().join("agent");
        let project = directory.path().join("project");
        fs::create_dir_all(project.join(".pi")).unwrap();
        fs::write(
            project.join(".pi/mcp.json"),
            "invalid project configuration",
        )
        .unwrap();
        let (trust, mut prompts) =
            ProjectTrustService::new(&agent, None, true, Default::default()).unwrap();

        let library = desktop_library(&agent, Some(&project), &trust).unwrap();
        let document = library.read(McpScope::Global).unwrap();
        assert!(!document.project_trusted);
        assert!(document.servers.is_empty());
        assert!(document.diagnostic.is_none());
        assert!(library.read(McpScope::Project).is_err());
        assert!(library
            .save(
                McpScope::Project,
                "missing",
                r#"{"version":1,"mcpServers":{}}"#
            )
            .is_err());
        assert!(prompts.try_recv().is_err());
        assert!(matches!(
            trust.evaluate(&project).unwrap(),
            ProjectTrustEvaluation::Ask(_)
        ));
        assert!(!agent.join("trust.json").exists());
        assert!(!agent.join("mcp.json").exists());
        assert!(!agent.join("sessions").exists());
        assert!(!project.join(".pi/.mcp.json.lock").exists());
    }

    #[test]
    fn saved_project_decisions_are_respected_on_each_library_access() {
        let directory = tempfile::tempdir().unwrap();
        let agent = directory.path().join("agent");
        let parent = directory.path().join("parent");
        let project = parent.join("project");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(project.join(".pi")).unwrap();
        let parent = fs::canonicalize(parent).unwrap();
        let project = fs::canonicalize(project).unwrap();
        fs::write(
            project.join(".pi/mcp.json"),
            r#"{"version":1,"mcpServers":{"project-only":{"enabled":false}}}"#,
        )
        .unwrap();
        let path = agent.join("trust.json");
        let (trust, mut prompts) =
            ProjectTrustService::new(&agent, None, true, Default::default()).unwrap();

        for (decisions, expected) in [
            (json!({parent.to_str().unwrap(): true}), true),
            (
                json!({parent.to_str().unwrap(): true, project.to_str().unwrap(): false}),
                false,
            ),
            (
                json!({parent.to_str().unwrap(): false, project.to_str().unwrap(): true}),
                true,
            ),
        ] {
            let saved = decisions.to_string();
            fs::write(&path, &saved).unwrap();
            let library = desktop_library(&agent, Some(&project), &trust).unwrap();
            let document = library.read(McpScope::Global).unwrap();
            assert_eq!(document.project_trusted, expected);
            assert_eq!(document.servers.len(), usize::from(expected));
            assert_eq!(library.read(McpScope::Project).is_ok(), expected);
            assert_eq!(fs::read_to_string(&path).unwrap(), saved);
            assert!(prompts.try_recv().is_err());
        }
        fs::write(&path, "invalid trust store").unwrap();
        assert!(desktop_library(&agent, Some(&project), &trust).is_err());
        assert!(!agent.join("mcp.json").exists());
        assert!(!agent.join("sessions").exists());
    }

    #[test]
    fn global_library_without_project_never_evaluates_trust_or_writes_files() {
        let directory = tempfile::tempdir().unwrap();
        let agent = directory.path().join("absent-agent");
        let (trust, mut prompts) =
            ProjectTrustService::new(&agent, Some(true), true, Default::default()).unwrap();

        let library = desktop_library(&agent, None, &trust).unwrap();
        let document = library.read(McpScope::Global).unwrap();
        assert_eq!(document.path, agent.join("mcp.json"));
        assert_eq!(document.revision, "missing");
        assert!(!document.project_trusted);
        assert!(document.writable);
        assert!(document.servers.is_empty());
        assert!(document.diagnostic.is_none());
        assert!(library.read(McpScope::Project).is_err());
        assert!(prompts.try_recv().is_err());
        assert!(!agent.exists());
    }
}
