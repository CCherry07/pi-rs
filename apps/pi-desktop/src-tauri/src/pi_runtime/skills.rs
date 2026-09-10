//! Desktop Adapter for skill-file management; never creates a PiSession.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pi_plugin_skills::management::{ManagedSkill, SkillDocument};
use serde::{Deserialize, Serialize};
use tauri::State;

use super::{workspace_path, PiRuntimeState};
use crate::state::AppState;

pub(super) type SkillMutationGate = Arc<Mutex<()>>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibrarySnapshot {
    skills: Vec<ManagedSkill>,
    destination: Option<PathBuf>,
    project_trusted: bool,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum SkillOperation {
    Read {
        path: PathBuf,
    },
    Save {
        path: PathBuf,
        revision: String,
        content: String,
    },
    Create {
        name: String,
        content: String,
    },
    Import {
        source: PathBuf,
    },
    Trash {
        path: PathBuf,
        revision: String,
    },
}

#[tauri::command]
pub(crate) async fn pi_skill_library_list(
    workspace_id: Option<String>,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<LibrarySnapshot, String> {
    let cwd = match workspace_id {
        Some(id) => Some(workspace_path(&state, &id).await?),
        None => None,
    };
    let agent_dir = pi.info.agent_dir.clone();
    let trust = pi.project_trust.clone();
    let gate = pi.skill_mutation_gate.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = gate.lock().map_err(|e| e.to_string())?;
        let (library, project_trusted) =
            pi_sdk::skills::desktop_skill_library(&agent_dir, cwd.as_deref(), &trust)?;
        Ok(LibrarySnapshot {
            skills: library.list(),
            destination: library.destination().map(PathBuf::from),
            project_trusted,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn pi_skill_library_operation(
    workspace_id: Option<String>,
    operation: SkillOperation,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
) -> Result<Option<SkillDocument>, String> {
    let cwd = match workspace_id {
        Some(id) => Some(workspace_path(&state, &id).await?),
        None => None,
    };
    let agent_dir = pi.info.agent_dir.clone();
    let trust = pi.project_trust.clone();
    let gate = pi.skill_mutation_gate.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = gate.lock().map_err(|e| e.to_string())?;
        let (library, _) =
            pi_sdk::skills::desktop_skill_library(&agent_dir, cwd.as_deref(), &trust)?;
        match operation {
            SkillOperation::Read { path } => library.read(&path).map(Some),
            SkillOperation::Save {
                path,
                revision,
                content,
            } => {
                library.save(&path, &revision, &content)?;
                library.read(&path).map(Some)
            }
            SkillOperation::Create { name, content } => {
                let path = library.create(&name, &content)?;
                library.read(&path).map(Some)
            }
            SkillOperation::Import { source } => {
                let path = library.import(&source)?;
                library.read(&path).map(Some)
            }
            SkillOperation::Trash { path, revision } => {
                library.trash(&path, &revision)?;
                Ok(None)
            }
        }
    })
    .await
    .map_err(|e| e.to_string())?
}
