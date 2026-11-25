use super::*;
use crate::ToolSelectionAvailability::{Denied, TemporarilyUnavailable, UnresolvedUntilInventory};

fn classified(availability: crate::ToolSelectionAvailability) -> AgentSessionOptions {
    options(true)
        .tool_selection_availability([("later".into(), availability)].into_iter().collect())
}

#[tokio::test]
async fn registered_names_distinguish_temporary_unresolved_and_denied() {
    for availability in [TemporarilyUnavailable, UnresolvedUntilInventory, Denied] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        saved(dir.path(), &path, &["later", "known"]);
        let original_leaf = SessionStore::open_handle(&path).unwrap().leaf_id().unwrap();
        let (generation, provider) = runtime(dir.path(), true);
        let session = AgentSession::open(generation, &path, classified(availability))
            .await
            .unwrap();
        let expected_desired = if availability == Denied {
            vec!["known"]
        } else {
            vec!["later", "known"]
        };
        let expected_effective = if availability == TemporarilyUnavailable {
            vec!["later", "known"]
        } else {
            vec!["known"]
        };
        assert_eq!(session.desired_active_tools().unwrap(), expected_desired);
        assert_eq!(session.snapshot().agent.active_tools, expected_effective);
        // This generic fixture retains the alias: selection projection is not
        // immutable registry visibility or an execution-authorization boundary.
        assert!(
            session
                .catalog()
                .tool_specs()
                .iter()
                .any(|tool| tool.name == "later")
        );
        if availability == Denied {
            let leaf = session.history().leaf_id();
            assert!(
                session
                    .set_active_tools(["later"])
                    .unwrap_err()
                    .to_string()
                    .contains("denied")
            );
            use pi_plugin::SessionContextAccess;
            let adapter = crate::PiPluginContext::new(
                pi_plugin::PresentationMode::Print,
                false,
                crate::PluginContextBinding::new(),
            );
            adapter.bind_generation_session(session.clone());
            assert!(
                adapter
                    .set_active_tools(vec!["later".into(), "unknown".into()])
                    .unwrap_err()
                    .to_string()
                    .contains("denied")
            );
            assert_eq!(session.history().leaf_id(), leaf);
        }
        session.checkout(Some(&original_leaf)).await.unwrap();
        assert_eq!(session.snapshot().agent.active_tools, expected_effective);
        session
            .prompt("Use only policy-resolved tools.")
            .await
            .unwrap();
        assert_eq!(session.desired_active_tools().unwrap(), expected_desired);
        assert_eq!(
            provider.requests()[0]
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            expected_effective
        );
        session.shutdown().await;
        let ready = AgentSession::open(runtime(dir.path(), true).0, &path, options(false))
            .await
            .unwrap();
        assert_eq!(ready.snapshot().agent.active_tools, expected_desired);
        ready.shutdown().await;
    }
}

#[tokio::test]
async fn fresh_header_never_publishes_unresolved_or_denied_registered_names() {
    for availability in [UnresolvedUntilInventory, Denied] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("draft.jsonl");
        let (generation, _) = runtime(dir.path(), true);
        generation.set_active_tools(["later", "known"]).unwrap();
        let session = AgentSession::create(generation, &path, classified(availability))
            .await
            .unwrap();
        assert_eq!(session.snapshot().agent.active_tools, ["known"]);
        assert_eq!(
            session.desired_active_tools().unwrap(),
            if availability == Denied {
                vec!["known"]
            } else {
                vec!["later", "known"]
            }
        );
        assert!(!path.exists());
        session.shutdown().await;
    }
}

#[tokio::test]
async fn unresolved_registered_selection_stays_withheld_during_explicit_set_and_failed_run_repair()
{
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    saved(dir.path(), &path, &["known"]);
    let (generation, provider) = runtime(dir.path(), true);
    let session = AgentSession::open(generation, &path, classified(UnresolvedUntilInventory))
        .await
        .unwrap();
    session.set_active_tools(["later", "known"]).unwrap();
    assert_eq!(session.snapshot().agent.active_tools, ["known"]);
    session
        .runtime
        .agent()
        .subscribe(Arc::new(|event, _| async move {
            if matches!(event, AgentEvent::MessageStart { .. }) {
                Err(EventError("injected listener failure".into()))
            } else {
                Ok(())
            }
        }));
    let error = session
        .prompt("Rejected before provider admission.")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("injected listener failure"), "{error}");
    assert!(
        !error.contains("restoring the saved tool selection failed"),
        "{error}"
    );
    assert_eq!(session.snapshot().agent.active_tools, ["known"]);
    assert_eq!(session.desired_active_tools().unwrap(), ["later", "known"]);
    assert!(provider.requests().is_empty());
    session.shutdown().await;
}
