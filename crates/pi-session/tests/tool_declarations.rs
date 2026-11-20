use pi_agent::AgentOptions;
use pi_core::{Message, UserMessage};
use pi_runtime::PiRuntime;
use pi_session::{AgentSession, LaneRecordEntry, OperationIntent};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn, TestToolsPlugin};
use serde_json::json;

#[tokio::test]
async fn normalized_tool_intent_commits_the_provisioned_system_identity_and_replays() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("declarations.jsonl");
    let runtime = PiRuntime::builder()
        .plugin(TestToolsPlugin::new())
        .provider_plugin(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
            "answer".into(),
        )]))
        .agent_options(AgentOptions {
            active_tools: vec!["echo".into()],
            ..Default::default()
        })
        .build()
        .unwrap();
    let session = AgentSession::create(runtime, &path, Default::default())
        .await
        .unwrap();
    let intent: Message = serde_json::from_value(json!({
        "role":"system","content":"caller instruction","timestamp":1,
        "toolsAdded":[{"name":"fiction","description":"not executable","parameters":{}}]
    }))
    .unwrap();
    session
        .run_messages(vec![intent, Message::User(UserMessage::text("run", 2))])
        .await
        .unwrap();
    let history = session.history().load().unwrap();
    let provisioned = history
        .records
        .iter()
        .find_map(|record| match &record.record {
            LaneRecordEntry::OperationStarted {
                intent:
                    OperationIntent::Run {
                        initial_messages, ..
                    },
                ..
            } => Some(initial_messages),
            _ => None,
        })
        .unwrap();
    let record = session
        .history()
        .get_entry(&provisioned[0].id)
        .expect("normalized system intent must commit its provisioned identity");
    let value = serde_json::to_value(&record.entry).unwrap();
    assert_eq!(value["message"]["toolsAdded"][0]["name"], "echo");
    let expected = session.snapshot().agent.messages.clone();
    session.shutdown().await;
    drop(session);
    let reopened = AgentSession::open(
        PiRuntime::builder()
            .plugin(TestToolsPlugin::new())
            .provider_plugin(ScriptedProviderPlugin::scripted([]))
            .build()
            .unwrap(),
        &path,
        Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(reopened.snapshot().agent.messages, expected);
    assert!(reopened.snapshot().queue.items.is_empty());
}
