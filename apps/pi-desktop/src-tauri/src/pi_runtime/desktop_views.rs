//! Desktop presentation capabilities carry plugin data without interpreting its
//! business schema. Widgets use Pi's existing custom-entry persistence seam.

use std::collections::BTreeMap;
use std::sync::Arc;

use pi_session::{SessionDocument, SessionEntry, SessionRecord};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, State};

use crate::state::AppState;

use super::{projection, session_store::SessionStore, PiRuntimeState};

const WIDGET_TYPE: &str = "pi.ui.widget";
const MAX_WIDGET_BYTES: usize = 256 * 1024;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DesktopSessionReference {
    session_id: Option<String>,
    isolated_session_id: Option<String>,
    owner_session_id: Option<String>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopWidgets {
    scope_token: Option<String>,
    widgets: BTreeMap<String, Value>,
    // Deletions retain their version so a delayed event cannot resurrect a key.
    versions: BTreeMap<String, u64>,
}

pub(super) fn widget_update(record: &SessionRecord) -> Option<(&str, &Value)> {
    let SessionEntry::Custom(entry) = &record.entry else {
        return None;
    };
    if entry.custom_type != WIDGET_TYPE {
        return None;
    }
    let data = entry.data.as_ref()?.as_object()?;
    let key = data.get("key")?.as_str()?;
    if key.is_empty()
        || key.len() > 128
        || !key.contains('.')
        || key.split('.').any(str::is_empty)
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return None;
    }
    let value = data.get("value")?;
    if serde_json::to_vec(value).ok()?.len() > MAX_WIDGET_BYTES {
        return None;
    }
    Some((key, value))
}

fn widgets_from_document(document: &SessionDocument) -> Result<DesktopWidgets, String> {
    let mut result = DesktopWidgets::default();
    for record in document.branch().map_err(|error| error.to_string())? {
        let Some((key, value)) = widget_update(record) else {
            continue;
        };
        result.versions.insert(key.to_string(), record.seq);
        if value.is_null() {
            result.widgets.remove(key);
        } else {
            result.widgets.insert(key.to_string(), value.clone());
        }
    }
    Ok(result)
}

fn validate_reference(reference: &DesktopSessionReference) -> Result<(), String> {
    if reference.session_id.is_none() && reference.isolated_session_id.is_none() {
        return Err("Related session reference requires a live handle or saved session id".into());
    }
    for id in [
        reference.session_id.as_deref(),
        reference.isolated_session_id.as_deref(),
        reference.owner_session_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err("Invalid related session reference".into());
        }
    }
    Ok(())
}

fn validate_workspace(document: &SessionDocument, cwd: &std::path::Path) -> Result<(), String> {
    let expected = std::fs::canonicalize(cwd).map_err(|error| error.to_string())?;
    let actual = std::fs::canonicalize(&document.header.cwd).map_err(|error| error.to_string())?;
    if expected != actual {
        return Err("Selected thread does not belong to the selected workspace".into());
    }
    Ok(())
}

fn stored_related_session(
    store: &SessionStore,
    owner: &str,
    child_id: &str,
) -> Result<Value, String> {
    let stored = store.read_isolated(child_id)?;
    if stored.parent_thread_id != owner || child_id == owner {
        return Err("Related session is not a direct child of its declared owner".into());
    }
    super::thread_from_stored_isolated(&stored)
}

#[tauri::command]
pub(crate) async fn pi_observe_desktop_session(
    workspace_id: String,
    thread_id: String,
    reference: DesktopSessionReference,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<Value, String> {
    validate_reference(&reference)?;
    let cwd = super::workspace_path(&state, &workspace_id).await?;
    validate_workspace(&pi.store.document(&thread_id)?, &cwd)?;
    let resolved = resolve_related_session(&pi.store, &thread_id, &reference)?;
    if let Some(live) = resolved.live {
        let child_id = resolved.thread["id"]
            .as_str()
            .expect("projected session id")
            .to_string();
        if pi.forwarders.lock().await.insert(child_id.clone()) {
            super::emit(
                &app,
                &workspace_id,
                "thread/started",
                json!({ "thread": resolved.thread }),
            );
            projection::spawn_session_forwarder(
                app,
                workspace_id,
                child_id.clone(),
                child_id,
                live,
                pi.store.clone(),
                Arc::clone(&pi.forwarders),
            );
        }
    }
    Ok(json!({ "thread": resolved.thread }))
}

struct ResolvedDesktopSession {
    thread: Value,
    live: Option<super::session_store::LiveSession>,
}

fn resolve_related_session(
    store: &SessionStore,
    thread_id: &str,
    reference: &DesktopSessionReference,
) -> Result<ResolvedDesktopSession, String> {
    validate_reference(reference)?;
    let owner = reference.owner_session_id.as_deref().unwrap_or(thread_id);
    store.validate_observation_owner(thread_id, owner)?;
    if reference.session_id.as_deref() == Some(thread_id) {
        return Err("A session cannot embed its own ancestor".into());
    }
    // Saved views after a restart have no live handle. Reading their document
    // must not instantiate a primary session or restart plugin execution.
    let observed_id = reference.session_id.as_deref().and_then(|id| {
        store
            .observed_isolated(id)
            .filter(|observed| observed.parent_thread_id == owner)
            .map(|observed| observed.observation.isolated_id().as_str().to_string())
    });
    let isolated_id = reference
        .isolated_session_id
        .as_deref()
        .or(observed_id.as_deref());
    if let Some(isolated_id) = isolated_id {
        match store.subscribe_isolated(owner, isolated_id, "agent", None) {
            Ok((observation, live)) => {
                let child_id = observation.session_id();
                if child_id == thread_id
                    || child_id == owner
                    || reference
                        .session_id
                        .as_ref()
                        .is_some_and(|id| id != &child_id)
                {
                    return Err("Related session live and saved identities do not match".into());
                }
                return Ok(ResolvedDesktopSession {
                    thread: super::thread_from_observation(&observation, owner, "agent", None),
                    live: Some(live),
                });
            }
            Err(error) if reference.session_id.is_none() => return Err(error),
            Err(_) => {}
        }
    }
    let child_id = reference.session_id.as_deref().ok_or_else(|| {
        "Related session is no longer active and has no saved identity".to_string()
    })?;
    Ok(ResolvedDesktopSession {
        thread: stored_related_session(store, owner, child_id)?,
        live: None,
    })
}

#[tauri::command]
pub(crate) async fn pi_get_desktop_widgets(
    workspace_id: String,
    thread_id: String,
    state: State<'_, AppState>,
    pi: State<'_, PiRuntimeState>,
    app: AppHandle,
) -> Result<DesktopWidgets, String> {
    let cwd = super::workspace_path(&state, &workspace_id).await?;
    let document = pi.store.document(&thread_id)?;
    validate_workspace(&document, &cwd)?;
    if pi.store.handle(&thread_id).is_some() {
        super::ensure_forwarder(&app, &pi, &workspace_id, thread_id.clone()).await?;
    }
    // Subscribe before the final read so an append between reading and
    // listener installation cannot be omitted from both channels.
    let primary = pi.store.handle(&thread_id).map(|(_, handle)| {
        let session = handle.current();
        (handle, session)
    });
    let document = match &primary {
        Some((_, session)) => session.log().load().map_err(|error| error.to_string())?,
        None => pi.store.document(&thread_id)?,
    };
    let mut widgets = widgets_from_document(&document)?;
    if let Some((handle, session)) = primary {
        widgets.scope_token = pi
            .desktop_command_scopes
            .issue(&thread_id, &handle, &session);
    }
    Ok(widgets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_session::{CustomEntry, SessionHeader, SessionLog};

    fn append_widget(log: &SessionLog, key: &str, value: Value) -> SessionRecord {
        log.append_session_record(SessionEntry::Custom(CustomEntry {
            custom_type: WIDGET_TYPE.into(),
            data: Some(json!({ "key": key, "value": value })),
        }))
        .unwrap()
    }

    #[test]
    fn widget_snapshots_follow_active_branch_and_keep_deletion_versions_out_of_context() {
        let directory = tempfile::tempdir().unwrap();
        let log = SessionLog::create(
            directory.path().join("session.jsonl"),
            SessionHeader::new("session", directory.path()),
        )
        .unwrap();
        let initial = append_widget(&log, "example.jobs", json!({ "status": "running" }));
        append_widget(&log, "example.other", json!([1, 2]));
        let complete = append_widget(&log, "example.jobs", json!({ "status": "complete" }));
        let deleted = append_widget(&log, "example.other", Value::Null);
        let document = log.load().unwrap();
        let snapshot = widgets_from_document(&document).unwrap();
        assert_eq!(snapshot.widgets["example.jobs"]["status"], "complete");
        assert_eq!(snapshot.versions["example.jobs"], complete.seq);
        assert!(!snapshot.widgets.contains_key("example.other"));
        assert_eq!(snapshot.versions["example.other"], deleted.seq);
        assert!(document.context().unwrap().messages.is_empty());

        log.branch(Some(&initial.id)).unwrap();
        let branch = widgets_from_document(&log.load().unwrap()).unwrap();
        assert_eq!(branch.widgets["example.jobs"]["status"], "running");
        assert_eq!(branch.versions["example.jobs"], initial.seq);
        assert!(!branch.versions.contains_key("example.other"));
    }

    #[test]
    fn malformed_or_unbounded_widget_entries_are_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let log = SessionLog::create(
            directory.path().join("session.jsonl"),
            SessionHeader::new("session", directory.path()),
        )
        .unwrap();
        for (key, value) in [
            ("unscoped", json!(true)),
            (".", json!(true)),
            ("example..empty", json!(true)),
            ("example.with space", json!(true)),
            ("example.large", json!("x".repeat(MAX_WIDGET_BYTES))),
        ] {
            let record = append_widget(&log, key, value);
            assert!(widget_update(&record).is_none());
        }
        let wrong_type = log
            .append_session_record(SessionEntry::Custom(CustomEntry {
                custom_type: "plugin.state".into(),
                data: Some(json!({ "key": "example.jobs", "value": 3 })),
            }))
            .unwrap();
        assert!(widget_update(&wrong_type).is_none());
        assert!(widgets_from_document(&log.load().unwrap())
            .unwrap()
            .widgets
            .is_empty());
    }

    #[test]
    fn nested_saved_references_require_real_direct_ownership_and_reject_cycles() {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let store = super::super::tests::scripted_store(agent_dir.clone());
        let child_path = agent_dir.join("sessions/project/root/isolated/child.jsonl");
        for (path, id, parent) in [
            (child_path.clone(), "child", "root"),
            (
                child_path
                    .with_extension("")
                    .join("isolated/grandchild.jsonl"),
                "grandchild",
                "child",
            ),
            (
                agent_dir.join("sessions/project/cycle/isolated/loop.jsonl"),
                "loop",
                "loop",
            ),
        ] {
            let log = SessionLog::create(path, SessionHeader::new(id, directory.path())).unwrap();
            log.append_session_record(SessionEntry::Custom(CustomEntry {
                custom_type: "pi.isolated_origin".into(),
                data: Some(json!({ "parentSessionId": parent })),
            }))
            .unwrap();
        }
        let reference = DesktopSessionReference {
            session_id: Some("grandchild".into()),
            owner_session_id: Some("child".into()),
            ..Default::default()
        };
        let nested = resolve_related_session(&store, "root", &reference).unwrap();
        assert_eq!(nested.thread["contextOrigin"]["parentThreadId"], "child");
        assert!(nested.live.is_none());
        assert!(resolve_related_session(&store, "unrelated", &reference).is_err());
        assert!(resolve_related_session(
            &store,
            "root",
            &DesktopSessionReference {
                session_id: Some("grandchild".into()),
                ..Default::default()
            }
        )
        .is_err());
        assert!(store.validate_observation_owner("root", "loop").is_err());
        assert!(store.handle("child").is_none());
        assert!(store.handle("grandchild").is_none());
    }

    #[tokio::test]
    async fn generic_observation_resolves_live_and_saved_children_without_tool_metadata_or_promotion(
    ) {
        let directory = tempfile::tempdir().unwrap();
        let agent_dir = directory.path().join("agent");
        let store = super::super::tests::scripted_store(agent_dir.clone());
        let parent = store.create(directory.path()).await.unwrap();
        let parent_id = parent.log().header().id;
        let (_, handle) = store.handle(&parent_id).unwrap();
        let isolated = handle
            .launch_isolated_session(pi_core::IsolatedSessionRequest::new(
                pi_core::CustomMessageContent::Text("A generic plugin task".into()),
            ))
            .await
            .unwrap();
        let live = resolve_related_session(
            &store,
            &parent_id,
            &DesktopSessionReference {
                isolated_session_id: Some(isolated.as_str().to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        let child_id = live.thread["id"].as_str().unwrap().to_string();
        assert!(live.live.as_ref().unwrap().primary().is_none());
        assert!(store.handle(&child_id).is_none());
        handle.wait_for_isolated_session(&isolated).await.unwrap();
        assert!(!parent.log().path().exists());
        assert_eq!(live.thread["contextOrigin"]["parentThreadId"], parent_id);

        let invalid = DesktopSessionReference {
            isolated_session_id: Some(isolated.as_str().to_string()),
            session_id: Some("different-child".into()),
            ..Default::default()
        };
        assert!(resolve_related_session(&store, &parent_id, &invalid).is_err());
        assert!(resolve_related_session(
            &store,
            &parent_id,
            &DesktopSessionReference {
                session_id: Some(parent_id.clone()),
                ..Default::default()
            }
        )
        .is_err());

        let reopened = super::super::tests::scripted_store(agent_dir);
        let saved = resolve_related_session(
            &reopened,
            &parent_id,
            &DesktopSessionReference {
                session_id: Some(child_id.clone()),
                isolated_session_id: Some("retired-handle".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(saved.live.is_none());
        assert_eq!(saved.thread["contextOrigin"]["parentThreadId"], parent_id);
        assert!(saved.thread.to_string().contains("hello from pi-rs"));
        assert!(reopened.handle(&child_id).is_none());
        assert!(reopened.list(directory.path()).unwrap().is_empty());
        assert!(resolve_related_session(
            &reopened,
            "unrelated-session",
            &DesktopSessionReference {
                session_id: Some(child_id),
                ..Default::default()
            }
        )
        .is_err());
        parent.shutdown().await;
    }

    #[tokio::test]
    async fn widget_live_event_sequence_matches_snapshot_including_final_state_and_tombstone() {
        let directory = tempfile::tempdir().unwrap();
        let store = super::super::tests::scripted_store(directory.path().join("agent"));
        let session = store.create(directory.path()).await.unwrap();
        let mut events = session.subscribe().events;
        for value in [
            json!({ "status": "running" }),
            json!({ "status": "completed" }),
            Value::Null,
        ] {
            session
                .append_custom_entry(
                    WIDGET_TYPE,
                    Some(json!({ "key": "example.jobs", "value": value })),
                )
                .unwrap();
            let event = events.recv().await.unwrap();
            let pi_session::AgentSessionEvent::EntryAppended { entry } = event.event else {
                panic!("custom entry must publish its durable event");
            };
            let (key, projected) = widget_update(&entry).unwrap();
            assert_eq!(projected, &value);
            let snapshot =
                widgets_from_document(&store.document(&session.log().header().id).unwrap())
                    .unwrap();
            assert_eq!(snapshot.versions[key], entry.seq);
            assert_eq!(
                snapshot.widgets.get(key),
                (!value.is_null()).then_some(&value)
            );
            assert!(session.snapshot().agent.messages.is_empty());
        }
        assert!(!session.log().path().exists());
        session.shutdown().await;
    }
}
