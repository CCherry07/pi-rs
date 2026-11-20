use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use pi_agent::AgentOptions;
use pi_core::PluginId;
use pi_plugin::{
    Plugin, PluginError, SessionPluginContext, SessionShutdownEvent, SessionStartEvent,
};
use pi_runtime::PiRuntime;
use pi_session::{
    AgentSession, AgentSessionOptions, CreateSessionOptions, MultiSessionManager, PiSession,
    PreparedSessionGeneration, SessionGenerationRequest,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};
use tokio::sync::Notify;

use super::*;

fn connection(capacity: usize) -> (RpcConnection, DuplexStream, BufReader<DuplexStream>) {
    let (input, reader) = tokio::io::duplex(256);
    let (writer, output) = tokio::io::duplex(capacity);
    (
        RpcConnection::with_reader(BufReader::new(reader), RpcOutput::with_writer(writer)),
        input,
        BufReader::new(output),
    )
}

fn export_html(_: &AgentSession, _: Option<&str>) -> Result<PathBuf, String> {
    Err("not used".into())
}

struct StartupBurst {
    session: Arc<Mutex<Weak<AgentSession>>>,
    count: usize,
    shutdown: Option<Arc<Notify>>,
}
#[pi_plugin::plugin]
impl Plugin for StartupBurst {
    fn id(&self) -> PluginId {
        PluginId::new("startup-burst")
    }
    async fn session_start(
        &self,
        _: &SessionPluginContext,
        _: &SessionStartEvent,
    ) -> Result<(), PluginError> {
        let session = self.session.lock().unwrap().upgrade().unwrap();
        for i in 0..self.count {
            session.notify_plugin(format!("startup:{i}"), NoticeLevel::Info);
        }
        Ok(())
    }
    async fn session_shutdown(
        &self,
        _: &SessionPluginContext,
        _: &SessionShutdownEvent,
    ) -> Result<(), PluginError> {
        if let Some(queued) = &self.shutdown {
            let session = self.session.lock().unwrap().upgrade().unwrap();
            session.notify_plugin("shutdown-tail".into(), NoticeLevel::Info);
            queued.notify_one();
        }
        Ok(())
    }
}

async fn managed(
    cwd: &Path,
    bridge: Arc<RpcUiBridge>,
    burst: usize,
) -> (MultiSessionManager, PiSession) {
    managed_with_turns(cwd, bridge, burst, vec![], None).await
}

async fn managed_with_turns(
    cwd: &Path,
    bridge: Arc<RpcUiBridge>,
    burst: usize,
    turns: Vec<ScriptedTurn>,
    shutdown: Option<Arc<Notify>>,
) -> (MultiSessionManager, PiSession) {
    let manager = MultiSessionManager::new(move |request: SessionGenerationRequest| {
        let bridge = bridge.clone();
        let turns = turns.clone();
        let shutdown = shutdown.clone();
        async move {
            let owner = Arc::new(Mutex::new(Weak::<AgentSession>::new()));
            let runtime = PiRuntime::builder()
                .provider_plugin(ScriptedProviderPlugin::scripted(turns))
                .plugin(StartupBurst {
                    session: owner.clone(),
                    count: burst,
                    shutdown,
                })
                .agent_options(AgentOptions {
                    cwd: request.cwd,
                    provider_id: "scripted".into(),
                    model_id: "test".into(),
                    ..Default::default()
                })
                .build()?;
            let mut options = AgentSessionOptions::default();
            options.retry.enabled = false;
            Ok(
                PreparedSessionGeneration::new(runtime, options).bind_session(move |session| {
                    *owner.lock().unwrap() = Arc::downgrade(&session);
                    bridge.bind_session(&session);
                }),
            )
        }
    });
    let mut options = CreateSessionOptions::new(cwd, cwd.join("memory.jsonl"));
    options.in_memory = true;
    let session = manager.create_session(options).await.unwrap();
    (manager, session)
}

async fn record(reader: &mut BufReader<DuplexStream>) -> Value {
    let mut line = String::new();
    let count = tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert!(count > 0, "unexpected end of RPC output");
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn startup_hook_failure_is_published_once_before_later_hooks_continue() {
    struct Failing;
    #[pi_plugin::plugin]
    impl Plugin for Failing {
        fn id(&self) -> PluginId {
            PluginId::new("bad-extension")
        }
        async fn session_start(
            &self,
            _: &SessionPluginContext,
            _: &SessionStartEvent,
        ) -> Result<(), PluginError> {
            Err(PluginError::Failure("fixture failure".into()))
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let (connection, _input, mut output) = connection(4096);
    let bridge = connection.ui_bridge();
    let manager = MultiSessionManager::new(move |request: SessionGenerationRequest| {
        let bridge = bridge.clone();
        async move {
            let owner = Arc::new(Mutex::new(Weak::<AgentSession>::new()));
            let runtime = PiRuntime::builder()
                .provider_plugin(ScriptedProviderPlugin::scripted([]))
                .plugin(Failing)
                .plugin(StartupBurst {
                    session: owner.clone(),
                    count: 1,
                    shutdown: None,
                })
                .agent_options(AgentOptions {
                    cwd: request.cwd,
                    provider_id: "scripted".into(),
                    model_id: "test".into(),
                    ..Default::default()
                })
                .build()?;
            Ok(
                PreparedSessionGeneration::new(runtime, AgentSessionOptions::default())
                    .bind_session(move |session| {
                        *owner.lock().unwrap() = Arc::downgrade(&session);
                        bridge.bind_session(&session);
                    }),
            )
        }
    });
    let session = manager
        .create_session(CreateSessionOptions::new(
            directory.path(),
            directory.path().join("memory.jsonl"),
        ))
        .await
        .unwrap();
    let error = record(&mut output).await;
    assert_eq!(error["type"], "extension_error");
    assert_eq!(error["extensionPath"], "bad-extension");
    assert_eq!(error["event"], "session_start");
    assert_eq!(error["error"], "fixture failure");
    assert_eq!(record(&mut output).await["message"], "startup:0");
    let diagnostics = session.current().catalog().plugin_diagnostics();
    assert_eq!(
        diagnostics
            .iter()
            .filter(|item| item.plugin_id.as_str() == "bad-extension")
            .count(),
        1
    );
    assert_eq!(
        diagnostics[0].raw_message.as_deref(),
        Some("fixture failure")
    );
    assert_eq!(diagnostics[0].message, "plugin failed: fixture failure");
    drop(connection);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn prompt_preflight_response_precedes_agent_events_and_is_not_repeated_on_stream_error() {
    for turn in [
        ScriptedTurn::Text("done".into()),
        ScriptedTurn::Error("fixture failure".into()),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let (connection, mut input, mut output) = connection(1);
        let (manager, session) = managed_with_turns(
            directory.path(),
            connection.ui_bridge(),
            0,
            vec![turn],
            None,
        )
        .await;
        let mut native = session.current().subscribe();
        let running = tokio::spawn(crate::rpc::run_connection(session, export_html, connection));
        input
            .write_all(b"{\"id\":\"prompt\",\"type\":\"prompt\",\"message\":\"hello\"}\n")
            .await
            .unwrap();
        assert_eq!(
            record(&mut output).await,
            json!({"id":"prompt","type":"response","command":"prompt","success":true,"data":{"disposition":"started"}})
        );
        let mut count = 0;
        loop {
            let event = record(&mut output).await;
            assert_ne!(
                event["type"], "response",
                "accepted prompt received a second response"
            );
            assert_ne!(
                event["type"], "entry_appended",
                "agent persistence emitted an extra RPC entry event"
            );
            assert_ne!(
                event["type"], "queue_update",
                "settlement did not change this empty queue"
            );
            count += 1;
            assert!(count < 100, "run did not settle");
            if event["type"] == "agent_settled" {
                break;
            }
        }
        let mut committed = Vec::new();
        while let Ok(event) = native.events.try_recv() {
            if let pi_session::AgentSessionEvent::EntryAppended { entry, origin } = event.event
                && origin == pi_session::EntryAppendOrigin::Agent
            {
                committed.push(entry);
            }
        }
        assert!(committed.len() >= 2, "native history updates were removed");
        input
            .write_all(b"{\"id\":\"history\",\"type\":\"get_entries\"}\n")
            .await
            .unwrap();
        let history = record(&mut output).await;
        assert_eq!(history["id"], "history");
        let entries = history["data"]["entries"].as_array().unwrap();
        for role in ["user", "assistant"] {
            assert!(
                entries.iter().any(|entry| entry["message"]["role"] == role),
                "{role} history was removed"
            );
        }
        input.shutdown().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        manager.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn clear_queue_event_is_queued_before_its_response() {
    let directory = tempfile::tempdir().unwrap();
    let (connection, mut input, mut output) = connection(32);
    let (manager, session) = managed(directory.path(), connection.ui_bridge(), 0).await;
    let running = tokio::spawn(crate::rpc::run_connection(session, export_html, connection));
    input
        .write_all(b"{\"id\":\"clear\",\"type\":\"clear_queue\"}\n")
        .await
        .unwrap();
    assert_eq!(
        record(&mut output).await,
        json!({"type":"queue_update","steering":[],"followUp":[]})
    );
    assert_eq!(
        record(&mut output).await,
        json!({"id":"clear","type":"response","command":"clear_queue","success":true,"data":{"steering":[],"followUp":[]}})
    );
    input.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn queue_retiming_without_a_wire_frame_still_acknowledges_abort_and_prompt_fences() {
    let directory = tempfile::tempdir().unwrap();
    let (connection, mut input, mut output) = connection(1);
    let (manager, session) = managed_with_turns(
        directory.path(),
        connection.ui_bridge(),
        0,
        vec![
            ScriptedTurn::WaitForAbort,
            ScriptedTurn::Text("answer".into()),
            ScriptedTurn::Text("queued answer".into()),
        ],
        None,
    )
    .await;
    let current = session.current();
    let mut native = current.subscribe();
    let running = tokio::spawn(crate::rpc::run_connection(session, export_html, connection));
    input
        .write_all(b"{\"id\":\"first\",\"type\":\"prompt\",\"message\":\"first\"}\n")
        .await
        .unwrap();
    loop {
        let event = record(&mut output).await;
        if event["type"] == "message_end" && event["message"]["role"] == "user" {
            break;
        }
    }
    input
        .write_all(b"{\"id\":\"queued\",\"type\":\"follow_up\",\"message\":\"retained\"}\n")
        .await
        .unwrap();
    let mut queue_updates = 0;
    loop {
        let event = record(&mut output).await;
        if event["type"] == "queue_update" {
            queue_updates += 1;
            assert_eq!(event["followUp"], json!(["retained"]));
        }
        if event["id"] == "queued" {
            assert_eq!(event["success"], true);
            break;
        }
    }
    assert_eq!(queue_updates, 1);
    let original_id = current.snapshot().queue.items[0].entry_id.clone();
    input
        .write_all(b"{\"id\":\"abort\",\"type\":\"abort\"}\n")
        .await
        .unwrap();
    let mut settled = false;
    loop {
        let event = record(&mut output).await;
        assert_ne!(
            event["type"], "queue_update",
            "retiming is not a user queue mutation"
        );
        if event["type"] == "agent_settled" {
            assert_eq!(event["aborted"], true);
            settled = true;
        }
        if event["id"] == "abort" {
            assert!(settled);
            assert_eq!(event["success"], true);
            break;
        }
    }
    let parked_id = current.snapshot().queue.items[0].entry_id.clone();
    assert_ne!(parked_id, original_id);
    let mut retimed = Vec::new();
    while let Ok(event) = native.events.try_recv() {
        if let pi_session::AgentSessionEvent::QueueUpdate {
            origin: pi_session::QueueUpdateOrigin::Retimed,
            items,
            ..
        } = event.event
        {
            retimed.push(items);
        }
    }
    assert_eq!(retimed, [current.snapshot().queue.items]);
    input
        .write_all(b"{\"id\":\"resume\",\"type\":\"prompt\",\"message\":\"resume\"}\n")
        .await
        .unwrap();
    let mut answers = Vec::new();
    loop {
        let event = record(&mut output).await;
        if event["type"] == "queue_update" {
            queue_updates += 1;
            assert_eq!(
                event["followUp"],
                json!([]),
                "only consumption changes the wire queue"
            );
        }
        if event["type"] == "message_start"
            && event["message"]["role"] == "user"
            && event["message"]["content"][0]["text"] == "retained"
        {
            assert_eq!(
                queue_updates, 2,
                "the writer must acknowledge consumption before the queued user starts"
            );
        }
        if event["type"] == "message_end" && event["message"]["role"] == "assistant" {
            answers.push(event["message"]["content"][0]["text"].clone());
        }
        if event["type"] == "agent_settled" {
            assert_eq!(event["aborted"], false);
            break;
        }
    }
    assert_eq!(answers, [json!("answer"), json!("queued answer")]);
    assert_eq!(queue_updates, 2);
    assert!(current.snapshot().queue.items.is_empty());
    input.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn piped_query_still_enqueues_its_response_before_eof_cancels_handlers() {
    let directory = tempfile::tempdir().unwrap();
    let (connection, mut input, mut output) = connection(32);
    let (manager, session) = managed(directory.path(), connection.ui_bridge(), 0).await;
    input
        .write_all(b"{\"id\":\"last\",\"type\":\"get_last_assistant_text\"}\n")
        .await
        .unwrap();
    input.shutdown().await.unwrap();
    let running = tokio::spawn(crate::rpc::run_connection(session, export_html, connection));
    assert_eq!(
        record(&mut output).await,
        json!({"id":"last","type":"response","command":"get_last_assistant_text","success":true,"data":{}})
    );
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn synchronous_startup_burst_precedes_connection_ready_without_lag_or_deadlock() {
    let directory = tempfile::tempdir().unwrap();
    let (connection, mut input, mut output) = connection(1);
    // No client is reading and run_connection has not started. This exceeds the
    // old broadcast buffer and exercises the explicitly supported synchronous queue.
    let (manager, session) = tokio::time::timeout(
        Duration::from_secs(5),
        managed(directory.path(), connection.ui_bridge(), 600),
    )
    .await
    .unwrap();
    let running = tokio::spawn(crate::rpc::run_connection(session, export_html, connection));
    input
        .write_all(b"{\"id\":\"ready\",\"type\":\"get_state\"}\n")
        .await
        .unwrap();
    for i in 0..600 {
        let event = record(&mut output).await;
        assert_eq!(event["type"], "extension_ui_request");
        assert_eq!(event["method"], "notify");
        assert_eq!(event["message"], format!("startup:{i}"));
        assert_eq!(event["notifyType"], "info");
        assert!(event["id"].is_string());
    }
    let ready = record(&mut output).await;
    assert_eq!(ready["id"], "ready");
    assert_eq!(ready["success"], true);
    input.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn normal_replacement_delivers_old_tail_before_new_startup_and_response() {
    replacement_tail(false).await;
}

#[tokio::test]
async fn eof_during_graceful_publication_fence_releases_old_observer_leases() {
    replacement_tail(true).await;
}

async fn replacement_tail(cancel: bool) {
    struct Gate {
        entered: std::sync::mpsc::Sender<()>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    #[async_trait]
    impl pi_session::SessionEventObserver for Gate {
        fn on_event(&self, event: &pi_session::RevisionedAgentSessionEvent) -> Result<(), String> {
            if matches!(&event.event, pi_session::AgentSessionEvent::PluginNotice { message, .. } if message == "held")
            {
                self.entered.send(()).map_err(|e| e.to_string())?;
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let (connection, mut input, mut output) = connection(16 * 1024);
    let queued = Arc::new(Notify::new());
    let (manager, session) = managed_with_turns(
        directory.path(),
        connection.ui_bridge(),
        1,
        vec![],
        Some(queued.clone()),
    )
    .await;
    assert_eq!(record(&mut output).await["message"], "startup:0");
    let old = session.current();
    let (entered, entering) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    // RPC was registered first. This observer freezes a legal concurrent
    // publication after RPC accepted E1, without holding any session lock.
    let gate = Arc::new(Gate {
        entered,
        release: Mutex::new(released),
    });
    let _guard = old.observe_events(gate.clone());
    let publishing = old.clone();
    let producer =
        std::thread::spawn(move || publishing.notify_plugin("held".into(), NoticeLevel::Info));
    entering.recv_timeout(Duration::from_secs(5)).unwrap();
    let running = tokio::spawn(crate::rpc::run_connection(session, export_html, connection));
    input
        .write_all(b"{\"id\":\"replace\",\"type\":\"new_session\"}\n")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), queued.notified())
        .await
        .unwrap();
    if cancel {
        assert!(
            !old.is_closed(),
            "normal retirement did not wait for its published tail"
        );
        input.shutdown().await.unwrap();
        let finished = tokio::time::timeout(Duration::from_secs(5), running).await;
        release.send(()).unwrap();
        producer.join().unwrap();
        finished.unwrap().unwrap().unwrap();
        // Managed replacement is complete-on-drop: disposal joins its transaction
        // after the external callback is released, rather than rolling it back.
        tokio::time::timeout(Duration::from_secs(5), manager.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert!(old.is_closed());
        assert_eq!(
            Arc::strong_count(&gate),
            1,
            "disposal after EOF retained an old observer"
        );
        return;
    }
    release.send(()).unwrap();
    producer.join().unwrap();
    assert_eq!(record(&mut output).await["message"], "held");
    assert_eq!(record(&mut output).await["message"], "shutdown-tail");
    assert_eq!(record(&mut output).await["message"], "startup:0");
    assert_eq!(
        record(&mut output).await,
        json!({"id":"replace","type":"response","command":"new_session","success":true,"data":{"cancelled":false}})
    );
    assert_eq!(
        Arc::strong_count(&gate),
        1,
        "normal retirement retained an old observer"
    );
    input.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn startup_ui_cancel_is_observed_while_its_request_waits_for_writer_ack() {
    for eof in [false, true] {
        let (mut connection, mut input, mut output) = connection(1);
        let bridge = connection.ui_bridge();
        let asking = bridge.clone();
        let answer = tokio::spawn(async move {
            asking
                .confirm("Confirm".into(), "startup question".into())
                .await
        });
        // Read one byte only: the request has entered the writer, but its full
        // frame cannot fit and run_connection has not started.
        let mut first = [0u8; 1];
        tokio::time::timeout(Duration::from_secs(5), output.read_exact(&mut first))
            .await
            .unwrap()
            .unwrap();
        if eof {
            input.shutdown().await.unwrap();
        } else {
            input.write_all(b"{\"type\":\"abort\"}\n").await.unwrap();
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), answer)
                .await
                .expect("UI waiter ignored cancellation while writer ack was blocked")
                .unwrap()
                .unwrap(),
            Some(false)
        );
        assert!(bridge.state.lock().unwrap().pending.is_empty());

        // Cancelling a UI waiter must not remove an already enqueued frame.
        let mut rest = String::new();
        output.read_line(&mut rest).await.unwrap();
        let request: Value =
            serde_json::from_str(&format!("{}{}", char::from(first[0]), rest)).unwrap();
        assert_eq!(request["method"], "confirm");
        bridge.flush().await.unwrap();
        if eof {
            bridge.response(
                &json!({"type":"extension_ui_response","id":request["id"],"confirmed":true}),
            );
            assert_eq!(
                bridge
                    .confirm("closed".into(), "closed".into())
                    .await
                    .unwrap(),
                Some(false)
            );
        } else {
            let command = connection.commands.recv().await.unwrap().unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&command).unwrap()["type"],
                "abort"
            );
            let asking = bridge.clone();
            let next = tokio::spawn(async move {
                asking
                    .confirm("Next".into(), "still connected".into())
                    .await
            });
            let question = record(&mut output).await;
            assert_ne!(question["id"], request["id"]);
            input
                .write_all(
                    format!(
                        "{}\n{}\n",
                        json!({"type":"extension_ui_response","id":request["id"],"confirmed":true}),
                        json!({"type":"extension_ui_response","id":question["id"],"cancelled":true})
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), next)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                Some(false)
            );
            input.shutdown().await.unwrap();
        }
        drop(connection);
    }
}

#[tokio::test]
async fn pending_ui_and_agent_writer_ack_retire_together_without_losing_the_old_tail() {
    struct BackgroundQuestion {
        starts: Arc<std::sync::atomic::AtomicUsize>,
        replaced: Arc<Notify>,
        answer: tokio::sync::mpsc::UnboundedSender<Result<bool, pi_plugin::PluginContextError>>,
    }
    #[pi_plugin::plugin]
    impl Plugin for BackgroundQuestion {
        fn id(&self) -> PluginId {
            PluginId::new("background-question")
        }
        async fn session_start(
            &self,
            context: &SessionPluginContext,
            _: &SessionStartEvent,
        ) -> Result<(), PluginError> {
            if self
                .starts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                let ui = context.ui.clone();
                let answer = self.answer.clone();
                tokio::spawn(async move {
                    let _ = answer.send(ui.confirm("Confirm", "old generation question").await);
                });
            } else {
                context.ui.notify(NoticeLevel::Info, "new-start")?;
                self.replaced.notify_one();
            }
            Ok(())
        }
        async fn session_shutdown(
            &self,
            context: &SessionPluginContext,
            _: &SessionShutdownEvent,
        ) -> Result<(), PluginError> {
            context.ui.notify(NoticeLevel::Info, "shutdown-tail")?;
            Ok(())
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let (connection, mut input, mut output) = connection(1);
    let bridge = connection.ui_bridge();
    let factory_bridge = bridge.clone();
    let (answer, mut answers) = tokio::sync::mpsc::unbounded_channel();
    let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let replaced = Arc::new(Notify::new());
    let replacement_started = replaced.clone();
    let scripted = Arc::new(ScriptedProviderPlugin::scripted([]));
    let provider = scripted.provider();
    let manager = MultiSessionManager::new(move |request: SessionGenerationRequest| {
        let bridge = factory_bridge.clone();
        let answer = answer.clone();
        let starts = starts.clone();
        let replaced = replaced.clone();
        let scripted = scripted.clone();
        async move {
            let context = Arc::new(
                pi_session::PiPluginContext::new(
                    pi_plugin::PresentationMode::Rpc,
                    false,
                    pi_session::PluginContextBinding::new(),
                )
                .with_ui_bridge(bridge.clone()),
            );
            let runtime = PiRuntime::builder()
                .plugin_context(context.clone())
                .provider_plugin_arc(scripted)
                .plugin(BackgroundQuestion {
                    starts,
                    replaced,
                    answer,
                })
                .agent_options(AgentOptions {
                    cwd: request.cwd,
                    provider_id: "scripted".into(),
                    model_id: "test".into(),
                    ..Default::default()
                })
                .build()?;
            Ok(
                PreparedSessionGeneration::new(runtime, AgentSessionOptions::default())
                    .bind_session(move |session| {
                        context.bind_generation_session(session.clone());
                        bridge.bind_session(&session);
                    }),
            )
        }
    });
    let mut options =
        CreateSessionOptions::new(directory.path(), directory.path().join("memory.jsonl"));
    options.in_memory = true;
    let session = manager.create_session(options).await.unwrap();
    let old = session.current();
    let mut native = old.subscribe();
    // A real generation-scoped dialog is inside the actual writer. Its frame
    // cannot finish; no complete output record is read until after retirement.
    let mut first = [0u8; 1];
    tokio::time::timeout(Duration::from_secs(5), output.read_exact(&mut first))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bridge.state.lock().unwrap().pending.len(), 1);
    let running = tokio::spawn(crate::rpc::run_connection(session, export_html, connection));
    input
        .write_all(b"{\"id\":\"work\",\"type\":\"prompt\",\"message\":\"hello\"}\n")
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(5), native.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event.event, pi_session::AgentSessionEvent::Agent(event) if matches!(*event, pi_core::AgentEvent::AgentStart))
    );
    assert!(
        provider.requests().is_empty(),
        "agent must still await actual writer acknowledgement"
    );
    assert!(!old.is_idle());
    assert_eq!(bridge.state.lock().unwrap().pending.len(), 1);
    assert!(answers.try_recv().is_err());
    input
        .write_all(b"{\"id\":\"replace\",\"type\":\"new_session\"}\n")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), replacement_started.notified())
        .await
        .expect("normal retirement waited for stdout instead of accepted publication");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), answers.recv())
            .await
            .unwrap()
            .unwrap(),
        Err(pi_plugin::PluginContextError::Retired)
    ));
    assert!(old.is_closed());
    assert!(bridge.state.lock().unwrap().pending.is_empty());
    assert!(provider.requests().is_empty());

    let mut rest = String::new();
    output.read_line(&mut rest).await.unwrap();
    let question: Value =
        serde_json::from_str(&format!("{}{}", char::from(first[0]), rest)).unwrap();
    assert_eq!(question["method"], "confirm");
    let mut records = Vec::new();
    loop {
        let value = record(&mut output).await;
        let done = value["id"] == "replace";
        records.push(value);
        assert!(records.len() < 100);
        if done {
            break;
        }
    }
    let index = |predicate: fn(&Value) -> bool| {
        records
            .iter()
            .position(predicate)
            .expect("missing queued frame")
    };
    assert!(index(|v| v["id"] == "work") < index(|v| v["type"] == "agent_start"));
    assert!(index(|v| v["type"] == "agent_settled") < index(|v| v["message"] == "shutdown-tail"));
    assert!(index(|v| v["message"] == "shutdown-tail") < index(|v| v["message"] == "new-start"));
    assert_eq!(records.last().unwrap()["success"], true);
    input
        .write_all(
            format!(
                "{}\n{}\n",
                json!({"type":"extension_ui_response","id":question["id"],"confirmed":true}),
                json!({"id":"probe","type":"get_state"})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(
        record(&mut output).await["id"],
        "probe",
        "late retired answer caused an extra event"
    );
    assert!(answers.try_recv().is_err());
    input.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn actual_writer_backpressure_stops_the_agent_and_output_disconnect_releases_it() {
    let directory = tempfile::tempdir().unwrap();
    let (connection, _input, output) = connection(1);
    let scripted = ScriptedProviderPlugin::scripted([ScriptedTurn::Text("done".into())]);
    let provider = scripted.provider();
    let runtime = PiRuntime::builder()
        .provider_plugin(scripted)
        .agent_options(AgentOptions {
            cwd: directory.path().to_path_buf(),
            provider_id: "scripted".into(),
            model_id: "test".into(),
            ..Default::default()
        })
        .build()
        .unwrap();
    let mut options = AgentSessionOptions::default();
    options.in_memory = true;
    let session = AgentSession::create(runtime, directory.path().join("memory.jsonl"), options)
        .await
        .unwrap();
    connection.ui_bridge().bind_session(&session);
    let mut native = session.subscribe();
    let active = session.clone();
    let running = tokio::spawn(async move { active.prompt("hello").await });
    let first = tokio::time::timeout(Duration::from_secs(5), native.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(first.event, pi_session::AgentSessionEvent::Agent(event) if matches!(*event, pi_core::AgentEvent::AgentStart))
    );
    assert!(
        provider.requests().is_empty(),
        "agent requested a provider before stdout acknowledged agent_start"
    );
    drop(output); // Input remains open; this is stdout failure, not stdin EOF.
    let _outcome = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();
    assert!(session.is_idle());
    drop(connection);
    session.shutdown().await;
}
