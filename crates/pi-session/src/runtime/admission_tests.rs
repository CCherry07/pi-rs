use super::*;
use crate::PromptDisposition;

#[tokio::test]
async fn prompt_receipt_follows_admission_and_precedes_provider_execution() {
    let directory = tempfile::tempdir().unwrap();
    let plugin = ScriptedProviderPlugin::scripted([ScriptedTurn::WaitForAbort]);
    let provider = plugin.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(plugin)
        .agent_options(AgentOptions {
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            ..AgentOptions::default()
        })
        .build()
        .unwrap();
    let session = AgentSession::create(
        runtime,
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let running = {
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            session
                .prompt_with_receipt("working", |disposition| {
                    assert!(provider.requests().is_empty());
                    assert!(session.accepts_live_messages());
                    sender.send(disposition).unwrap();
                })
                .await
        })
    };
    let disposition = tokio::time::timeout(std::time::Duration::from_secs(2), receiver)
        .await
        .expect("the receipt must not wait for provider completion")
        .unwrap();
    assert_eq!(disposition, PromptDisposition::Started);
    assert!(!running.is_finished());
    let queued = Arc::new(Mutex::new(None));
    session
        .prompt_with_receipt("next", |disposition| {
            *queued.lock().unwrap() = Some(disposition);
        })
        .await
        .unwrap();
    assert_eq!(*queued.lock().unwrap(), Some(PromptDisposition::Queued));
    session.abort();
    running.await.unwrap().unwrap();
    session.shutdown().await;
}

#[tokio::test]
async fn prompt_receipt_reports_handled_input_and_omits_failed_preparation() {
    let directory = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let session = AgentSession::create(
        explicit_command_runtime(Arc::clone(&calls), ScriptedTurn::WaitForAbort),
        directory.path().join("session.jsonl"),
        Default::default(),
    )
    .await
    .unwrap();
    session
        .prompt_with_receipt("/ui-action", |disposition| {
            assert_eq!(disposition, PromptDisposition::Handled);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(!session.accepts_live_messages());
        })
        .await
        .unwrap();
    session.shutdown().await;
    let error = session
        .prompt_with_receipt("closed", |_| {
            panic!("failed preparation cannot be acknowledged")
        })
        .await
        .unwrap_err();
    assert!(matches!(error, SessionError::Closed));
}
