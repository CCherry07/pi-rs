use super::*;

struct TreeHook {
    entered: Arc<tokio::sync::Notify>,
    block: bool,
}

#[pi_plugin::plugin]
impl Plugin for TreeHook {
    fn id(&self) -> PluginId {
        PluginId::new("tree-hook")
    }

    async fn session_before_tree(
        &self,
        _: &SessionPluginContext,
        event: &SessionBeforeTreeEvent,
    ) -> Result<Option<SessionBeforeTreeResult>, PluginError> {
        self.entered.notify_one();
        assert_eq!(
            event.preparation.custom_instructions.as_deref(),
            Some("focus")
        );
        if self.block {
            event.signal.wait().await;
        }
        Ok(Some(SessionBeforeTreeResult {
            summary: Some(pi_plugin::SessionTreeSummary {
                summary: "plugin summary".into(),
                details: Some(serde_json::json!({"source":"plugin"})),
                usage: None,
            }),
            label: Some("plugin label".into()),
            ..Default::default()
        }))
    }
}

async fn tree_fixture(
    block: bool,
) -> (
    tempfile::TempDir,
    Arc<AgentSession>,
    Arc<tokio::sync::Notify>,
    String,
) {
    let directory = tempfile::tempdir().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let runtime = PiRuntime::builder()
        .plugin(TreeHook {
            entered: entered.clone(),
            block,
        })
        // No third provider response: the before-tree hook must avoid that call.
        .provider_plugin(ScriptedProviderPlugin::scripted([
            ScriptedTurn::Text("root".into()),
            ScriptedTurn::Text("branch".into()),
        ]))
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    session.prompt("first").await.unwrap();
    let target = session.store().leaf_id().unwrap();
    session.prompt("second").await.unwrap();
    (directory, session, entered, target)
}

#[tokio::test]
async fn tree_hook_replaces_summary_before_provider_and_publishes_immutable_history() {
    let (_directory, session, _, target) = tree_fixture(false).await;
    let mut events = session.subscribe();
    let id = session
        .summarize_branch_and_checkout(&target, Some("focus".into()), false, None)
        .await
        .unwrap();
    let tree = loop {
        if let AgentSessionEvent::TreeChanged { document } =
            events.events.recv().await.unwrap().event
        {
            break document;
        }
    };
    assert_eq!(tree.leaf_id(MAIN_LANE).unwrap(), Some(id.as_str()));
    assert_eq!(tree.labels[&id], "plugin label");
    let entry = tree.entries.iter().find(|entry| entry.id == id).unwrap();
    assert_eq!(entry.parent_id.as_deref(), Some(target.as_str()));
    assert!(
        matches!(&entry.entry, SessionEntry::BranchSummary(summary) if summary.summary == "plugin summary")
    );
    // A retained event is frozen; later writes must not mutate its tree.
    session.set_name(Some("later".into())).await.unwrap();
    assert!(tree.name.is_none());
    assert!(session.is_idle());
}

#[tokio::test]
async fn aborting_tree_preparation_preserves_history_and_excludes_other_mutations() {
    let (_directory, session, entered, target) = tree_fixture(true).await;
    let before = std::fs::read(session.store().path()).unwrap();
    let leaf = session.store().leaf_id();
    let task = {
        let session = session.clone();
        tokio::spawn(async move {
            session
                .summarize_branch_and_checkout(&target, Some("focus".into()), false, None)
                .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    assert!(!session.is_idle());
    assert!(matches!(
        session.set_thinking_level(ThinkingLevel::Off),
        Err(SessionError::Busy)
    ));
    session.abort();
    assert!(matches!(
        task.await.unwrap(),
        Err(SessionError::Cancelled(_))
    ));
    assert_eq!(session.store().leaf_id(), leaf);
    assert_eq!(std::fs::read(session.store().path()).unwrap(), before);
    assert!(session.is_idle());
}
