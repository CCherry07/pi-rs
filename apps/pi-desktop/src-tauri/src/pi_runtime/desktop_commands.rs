//! Desktop buttons enter the same registered-command/input pipeline as the composer.
use super::{ensure_forwarder, workspace_path, PiRuntimeState};
use crate::state::AppState;
use pi_session::AgentSession;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use tauri::{AppHandle, State};

#[derive(Default)]
pub(super) struct DesktopCommandScopes {
    scopes: Mutex<HashMap<String, (Weak<AgentSession>, String)>>,
}

impl DesktopCommandScopes {
    pub(super) fn issue(
        &self,
        thread_id: &str,
        handle: &pi_session::PiSession,
        session: &Arc<AgentSession>,
    ) -> Option<String> {
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A delayed old snapshot must not overwrite a token already issued
        // for the replacement. Check current ownership under the issuance lock.
        if session.is_closed()
            || session.log().header().id != thread_id
            || !Arc::ptr_eq(&handle.current(), session)
        {
            return None;
        }
        scopes.retain(|_, (session, _)| session.strong_count() > 0);
        let entry = scopes
            .entry(thread_id.to_string())
            .or_insert_with(|| (Arc::downgrade(session), uuid::Uuid::new_v4().to_string()));
        if !entry
            .0
            .upgrade()
            .is_some_and(|previous| Arc::ptr_eq(&previous, session))
        {
            *entry = (Arc::downgrade(session), uuid::Uuid::new_v4().to_string());
        }
        Some(entry.1.clone())
    }

    fn validates(&self, thread_id: &str, token: &str, session: &Arc<AgentSession>) -> bool {
        self.scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(thread_id)
            .is_some_and(|(registered, expected)| {
                token == expected
                    && !session.is_closed()
                    && registered
                        .upgrade()
                        .is_some_and(|registered| Arc::ptr_eq(&registered, session))
            })
    }
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn pi_desktop_command(
    workspace_id: String,
    thread_id: String,
    name: String,
    args: String,
    scope_token: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    let cwd = workspace_path(&state, &workspace_id).await?;
    let (_, handle) = pi
        .store
        .handle(&thread_id)
        .ok_or("This session is no longer active")?;
    let session = handle.current();
    if !pi
        .desktop_command_scopes
        .validates(&thread_id, &scope_token, &session)
    {
        return Err("This desktop command belongs to a retired session view".into());
    }
    if session
        .runtime()
        .cwd()
        .canonicalize()
        .map_err(|error| error.to_string())?
        != cwd.canonicalize().map_err(|error| error.to_string())?
    {
        return Err("The session does not belong to this workspace".into());
    }
    if args.len() > 1024 * 1024 {
        return Err("Desktop command arguments exceed 1 MiB".into());
    }
    ensure_forwarder(&app, &pi, &workspace_id, thread_id).await?;
    let outcome = session
        .invoke_command(&name, &args)
        .await
        .map_err(|error| error.to_string())?;
    Ok(
        json!({"status":match outcome {pi_session::SubmitOutcome::Queued{..}=>"queued",_=>"handled"}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn command_scope_rotates_on_same_id_reload_and_rejects_retired_requests() {
        let directory = tempfile::tempdir().unwrap();
        let store = super::super::tests::scripted_store(directory.path().join("agent"));
        let old = store.create(directory.path()).await.unwrap();
        let id = old.log().header().id;
        let (_, handle) = store.handle(&id).unwrap();
        let scopes = DesktopCommandScopes::default();
        let old_token = scopes.issue(&id, &handle, &old).unwrap();
        assert_eq!(scopes.issue(&id, &handle, &old), Some(old_token.clone()));
        assert!(scopes.validates(&id, &old_token, &old));
        let current = store.reload(&id).await.unwrap();
        assert_eq!(current.log().header().id, id);
        assert!(!scopes.validates(&id, &old_token, &current));
        assert!(!scopes.validates(&id, &old_token, &old));
        let new_token = scopes.issue(&id, &handle, &current).unwrap();
        assert_ne!(new_token, old_token);
        assert!(scopes.validates(&id, &new_token, &current));
        assert!(scopes.issue(&id, &handle, &old).is_none());
        assert!(scopes.validates(&id, &new_token, &current));
        assert!(!scopes.validates("different-thread", &new_token, &current));
        assert!(matches!(
            old.invoke_command("native", "").await,
            Err(pi_session::SessionError::Closed)
        ));
        current.shutdown().await;
    }
}
