//! Pi's semantic extension dialogs and notifications over the shared RPC connection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_plugin::{NoticeLevel, UiDialogRequest, UiDialogResponse, UiNotification};
use pi_session::{AgentSession, PluginUiBridge};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::sync::{mpsc, oneshot};

use crate::output::RpcOutput;

enum AnswerKind {
    Confirmation,
    Text,
}

struct Pending {
    kind: AnswerKind,
    answer: oneshot::Sender<UiDialogResponse>,
}

#[derive(Default)]
struct State {
    closed: bool,
    pending: HashMap<String, Pending>,
}

/// A frontend bridge installed before session startup, so startup hooks may ask questions too.
pub struct RpcUiBridge {
    state: Arc<Mutex<State>>,
    pub(crate) output: RpcOutput,
    events: crate::events::RpcEventStream,
}

impl RpcUiBridge {
    fn new(output: RpcOutput) -> Self {
        Self {
            state: Arc::default(),
            events: crate::events::RpcEventStream::new(output.clone()),
            output,
        }
    }

    /// Install the event observer before this generation's startup hooks run.
    pub fn bind_session(&self, session: &Arc<AgentSession>) {
        self.events.bind(session);
    }

    pub(crate) async fn publication_barrier(
        &self,
        session: &Arc<AgentSession>,
    ) -> Result<(), String> {
        self.bind_session(session);
        self.events.publication_barrier(session).await
    }

    pub(crate) fn close_events(&self) {
        self.events.close();
    }

    /// Drains synchronous notices after startup failure or session shutdown.
    pub async fn flush(&self) -> Result<(), String> {
        self.output.flush().await
    }

    /// Routes only UI responses. Unknown IDs are deliberately ignored, as in Pi.
    fn response(&self, value: &Value) -> bool {
        if value["type"] != "extension_ui_response" {
            return false;
        }
        if let Some(id) = value["id"].as_str()
            && let Some(pending) = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pending
                .remove(id)
        {
            let answer = if value["cancelled"] == true {
                UiDialogResponse::Cancelled
            } else {
                match pending.kind {
                    AnswerKind::Confirmation => {
                        UiDialogResponse::Confirmed(value["confirmed"] == true)
                    }
                    AnswerKind::Text => value["value"]
                        .as_str()
                        .map_or(UiDialogResponse::Cancelled, |value| {
                            UiDialogResponse::Value(value.to_owned())
                        }),
                }
            };
            let _ = pending.answer.send(answer);
        }
        true
    }

    pub(crate) fn cancel_pending(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .clear();
    }

    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        state.pending.clear();
    }
}

struct Question {
    id: String,
    state: Arc<Mutex<State>>,
}

impl Drop for Question {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .remove(&self.id);
    }
}

#[async_trait]
impl PluginUiBridge for RpcUiBridge {
    fn supports_confirm(&self) -> bool {
        true
    }

    async fn confirm(&self, title: String, message: String) -> Result<Option<bool>, String> {
        let answer = self
            .dialog(UiDialogRequest::Confirm { title, message }, None)
            .await?;
        Ok(Some(matches!(
            answer,
            Some(UiDialogResponse::Confirmed(true))
        )))
    }

    async fn dialog(
        &self,
        request: UiDialogRequest,
        timeout_ms: Option<u64>,
    ) -> Result<Option<UiDialogResponse>, String> {
        let kind = match &request {
            UiDialogRequest::Confirm { .. } => AnswerKind::Confirmation,
            UiDialogRequest::Select { .. }
            | UiDialogRequest::Input { .. }
            | UiDialogRequest::Editor { .. } => AnswerKind::Text,
        };
        let mut request = serde_json::to_value(request).map_err(|error| error.to_string())?;
        // Pi's editor wire request has no timeout field.
        let timeout_ms = timeout_ms.filter(|value| *value > 0 && request["method"] != "editor");
        let id = uuid::Uuid::now_v7().to_string();
        let (answer, result) = oneshot::channel();
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.closed {
                return Ok(Some(UiDialogResponse::Cancelled));
            }
            state.pending.insert(id.clone(), Pending { kind, answer });
        }
        let _question = Question {
            id: id.clone(),
            state: self.state.clone(),
        };
        request["type"] = json!("extension_ui_request");
        request["id"] = json!(id);
        if let Some(timeout_ms) = timeout_ms {
            request["timeout"] = json!(timeout_ms);
        }
        let answer = async {
            let mut result = result;
            tokio::select! {
                biased;
                // EOF/abort clears the pending sender. Observe that even while
                // the writer is backpressured; dropping emit does not remove its
                // already enqueued frame from the connection's output queue.
                answer = &mut result => answer.unwrap_or(UiDialogResponse::Cancelled),
                written = self.output.emit(request) => {
                    if written.is_err() {
                        self.close();
                        UiDialogResponse::Cancelled
                    } else {
                        result.await.unwrap_or(UiDialogResponse::Cancelled)
                    }
                }
            }
        };
        Ok(Some(match timeout_ms {
            Some(timeout_ms) => {
                tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), answer)
                    .await
                    .unwrap_or(UiDialogResponse::Cancelled)
            }
            None => answer.await,
        }))
    }

    fn notify(&self, level: NoticeLevel, message: String) -> Result<bool, String> {
        self.notification(UiNotification::Notify {
            message,
            level: Some(level),
        })
    }

    fn notification(&self, notification: UiNotification) -> Result<bool, String> {
        let mut request = match notification {
            UiNotification::Notify { message, level } => {
                let mut request = json!({"method":"notify", "message":message});
                if let Some(level) = level {
                    request["notifyType"] = json!(level);
                }
                request
            }
            UiNotification::Status { key, text } => {
                let mut request = json!({"method":"setStatus", "statusKey":key});
                if let Some(text) = text {
                    request["statusText"] = json!(text);
                }
                request
            }
            UiNotification::Widget {
                key,
                lines,
                placement,
            } => {
                let mut request = json!({"method":"setWidget", "widgetKey":key});
                if let Some(lines) = lines {
                    request["widgetLines"] = json!(lines);
                }
                if let Some(placement) = placement {
                    request["widgetPlacement"] = json!(placement);
                }
                request
            }
            UiNotification::Title { title } => json!({"method":"setTitle", "title":title}),
            UiNotification::EditorText { text } => {
                json!({"method":"set_editor_text", "text":text})
            }
        };
        request["type"] = json!("extension_ui_request");
        request["id"] = json!(uuid::Uuid::now_v7().to_string());
        self.output.enqueue(request)?;
        Ok(true)
    }
}

/// Owns stdin from startup through shutdown. The reader answers UI messages while startup or
/// command hooks await them; ordinary commands wait until the managed session is ready.
pub struct RpcConnection {
    pub(crate) bridge: Arc<RpcUiBridge>,
    pub(crate) commands: mpsc::UnboundedReceiver<Result<Vec<u8>, String>>,
    reader: tokio::task::JoinHandle<()>,
}

impl RpcConnection {
    pub fn stdio() -> Self {
        Self::with_reader(
            tokio::io::BufReader::new(tokio::io::stdin()),
            RpcOutput::new(),
        )
    }

    fn with_reader(reader: impl AsyncBufRead + Unpin + Send + 'static, output: RpcOutput) -> Self {
        let bridge = Arc::new(RpcUiBridge::new(output));
        let (sender, commands) = mpsc::unbounded_channel();
        let reading = bridge.clone();
        let reader = tokio::spawn(async move {
            read_input(reader, &reading, sender).await;
            reading.close();
        });
        Self {
            bridge,
            commands,
            reader,
        }
    }

    pub fn ui_bridge(&self) -> Arc<RpcUiBridge> {
        self.bridge.clone()
    }
}

impl Drop for RpcConnection {
    fn drop(&mut self) {
        self.bridge.close_events();
        self.bridge.close();
        self.reader.abort();
    }
}

async fn read_input(
    mut reader: impl AsyncBufRead + Unpin,
    bridge: &RpcUiBridge,
    commands: mpsc::UnboundedSender<Result<Vec<u8>, String>>,
) {
    loop {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) => return,
            Err(error) => {
                let _ = commands.send(Err(error.to_string()));
                return;
            }
            Ok(_) => {}
        }
        if let Ok(value) = serde_json::from_slice::<Value>(&line) {
            if bridge.response(&value) {
                continue;
            }
            if value["type"] == "abort" {
                bridge.cancel_pending();
            }
        }
        if commands.send(Ok(line)).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod connection_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use pi_plugin::{PluginContextEpoch, PluginContextError, PresentationMode, UiWidgetPlacement};
    use pi_session::{PiPluginContext, PluginContextBinding};
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn semantic_notifications_use_pi_wire_before_session_startup_and_retire() {
        let (writer, reader) = tokio::io::duplex(8192);
        let bridge = Arc::new(RpcUiBridge::new(RpcOutput::with_writer(writer)));
        let epoch = PluginContextEpoch::new(Arc::new(
            PiPluginContext::new(PresentationMode::Rpc, true, PluginContextBinding::new())
                .with_ui_bridge(bridge.clone()),
        ));
        let ui = epoch.context().ui;
        let cases = [
            (
                UiNotification::Notify {
                    message: "ready".into(),
                    level: None,
                },
                json!({"method":"notify", "message":"ready"}),
            ),
            (
                UiNotification::Notify {
                    message: "info".into(),
                    level: Some(NoticeLevel::Info),
                },
                json!({"method":"notify", "message":"info", "notifyType":"info"}),
            ),
            (
                UiNotification::Status {
                    key: "job".into(),
                    text: Some("running".into()),
                },
                json!({"method":"setStatus", "statusKey":"job", "statusText":"running"}),
            ),
            (
                UiNotification::Status {
                    key: "job".into(),
                    text: Some(String::new()),
                },
                json!({"method":"setStatus", "statusKey":"job", "statusText":""}),
            ),
            (
                UiNotification::Status {
                    key: "job".into(),
                    text: None,
                },
                json!({"method":"setStatus", "statusKey":"job"}),
            ),
            (
                UiNotification::Widget {
                    key: "job".into(),
                    lines: Some(vec!["第一行".into(), "second".into()]),
                    placement: None,
                },
                json!({"method":"setWidget", "widgetKey":"job", "widgetLines":["第一行", "second"]}),
            ),
            (
                UiNotification::Widget {
                    key: "job".into(),
                    lines: Some(vec![]),
                    placement: Some(UiWidgetPlacement::AboveEditor),
                },
                json!({"method":"setWidget", "widgetKey":"job", "widgetLines":[], "widgetPlacement":"aboveEditor"}),
            ),
            (
                UiNotification::Widget {
                    key: "job".into(),
                    lines: None,
                    placement: Some(UiWidgetPlacement::BelowEditor),
                },
                json!({"method":"setWidget", "widgetKey":"job", "widgetPlacement":"belowEditor"}),
            ),
            (
                UiNotification::Title {
                    title: "项目".into(),
                },
                json!({"method":"setTitle", "title":"项目"}),
            ),
            (
                UiNotification::EditorText {
                    text: "draft\nnext".into(),
                },
                json!({"method":"set_editor_text", "text":"draft\nnext"}),
            ),
        ];
        for (notification, _) in &cases {
            ui.notification(notification.clone()).unwrap();
        }
        bridge.flush().await.unwrap();
        let mut lines = tokio::io::BufReader::new(reader).lines();
        let mut ids = std::collections::HashSet::new();
        for (_, expected) in cases {
            let mut request: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let object = request.as_object_mut().unwrap();
            assert_eq!(object.remove("type"), Some(json!("extension_ui_request")));
            let id = object.remove("id").unwrap();
            assert!(ids.insert(id.as_str().unwrap().to_owned()));
            assert_eq!(request, expected);
        }
        assert!(bridge.state.lock().unwrap().pending.is_empty());
        epoch.retire();
        assert!(matches!(
            ui.notification(UiNotification::Title {
                title: "late".into()
            }),
            Err(PluginContextError::Retired)
        ));
    }

    #[tokio::test]
    async fn dialog_wire_distinguishes_omitted_and_empty_optional_text() {
        let (writer, reader) = tokio::io::duplex(4096);
        let bridge = Arc::new(RpcUiBridge::new(RpcOutput::with_writer(writer)));
        let mut lines = tokio::io::BufReader::new(reader).lines();
        for (request, field, expected) in [
            (
                UiDialogRequest::Input {
                    title: "input".into(),
                    placeholder: None,
                },
                "placeholder",
                None,
            ),
            (
                UiDialogRequest::Input {
                    title: "input".into(),
                    placeholder: Some(String::new()),
                },
                "placeholder",
                Some(json!("")),
            ),
            (
                UiDialogRequest::Editor {
                    title: "editor".into(),
                    prefill: None,
                },
                "prefill",
                None,
            ),
            (
                UiDialogRequest::Editor {
                    title: "editor".into(),
                    prefill: Some(String::new()),
                },
                "prefill",
                Some(json!("")),
            ),
        ] {
            let asking = bridge.clone();
            let pending = tokio::spawn(async move { asking.dialog(request, None).await });
            let wire: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(wire.get(field), expected.as_ref());
            bridge.response(
                &json!({"type":"extension_ui_response", "id":wire["id"], "cancelled":true}),
            );
            assert_eq!(
                pending.await.unwrap().unwrap(),
                Some(UiDialogResponse::Cancelled)
            );
        }
    }

    #[tokio::test]
    async fn confirmation_is_correlated_and_disconnect_fails_closed() {
        let (input, mut client_input) = tokio::io::duplex(4096);
        let (output, client_output) = tokio::io::duplex(4096);
        let connection = RpcConnection::with_reader(
            tokio::io::BufReader::new(input),
            RpcOutput::with_writer(output),
        );
        let bridge = connection.ui_bridge();
        let mut output = tokio::io::BufReader::new(client_output).lines();
        let request_bridge = bridge.clone();
        let pending = tokio::spawn(async move {
            request_bridge
                .confirm("permission".into(), "write?".into())
                .await
        });
        let request: Value =
            serde_json::from_str(&output.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(request["method"], "confirm");
        client_input
            .write_all(
                b"{\"type\":\"extension_ui_response\",\"id\":\"unknown\",\"confirmed\":true}\n",
            )
            .await
            .unwrap();
        assert!(!pending.is_finished());
        client_input
            .write_all(
                format!(
                    "{}\n",
                    json!({"type":"extension_ui_response", "id":request["id"], "confirmed":true})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        assert_eq!(pending.await.unwrap().unwrap(), Some(true));
        let request_bridge = bridge.clone();
        let pending = tokio::spawn(async move {
            request_bridge
                .confirm("permission".into(), "write again?".into())
                .await
        });
        output.next_line().await.unwrap();
        drop(client_input);
        assert_eq!(pending.await.unwrap().unwrap(), Some(false));
        assert_eq!(
            bridge
                .confirm("closed".into(), "closed".into())
                .await
                .unwrap(),
            Some(false)
        );
    }

    #[tokio::test]
    async fn dropped_confirmation_releases_its_response_slot() {
        let bridge = Arc::new(RpcUiBridge::new(RpcOutput::with_writer(tokio::io::sink())));
        let requesting = bridge.clone();
        let request = tokio::spawn(async move {
            requesting
                .confirm("permission".into(), "write?".into())
                .await
        });
        tokio::task::yield_now().await;
        assert_eq!(bridge.state.lock().unwrap().pending.len(), 1);
        request.abort();
        let _ = request.await;
        assert!(bridge.state.lock().unwrap().pending.is_empty());
    }

    #[tokio::test]
    async fn concurrent_dialogs_correlate_typed_answers_and_preserve_empty_text() {
        let (writer, reader) = tokio::io::duplex(4096);
        let bridge = Arc::new(RpcUiBridge::new(RpcOutput::with_writer(writer)));
        let mut lines = tokio::io::BufReader::new(reader).lines();
        let mut requests = Vec::new();
        for request in [
            UiDialogRequest::Select {
                title: "pick".into(),
                options: vec!["one".into(), "two".into()],
            },
            UiDialogRequest::Input {
                title: "type".into(),
                placeholder: Some("empty is valid".into()),
            },
            UiDialogRequest::Editor {
                title: "edit".into(),
                prefill: Some("old\ntext".into()),
            },
        ] {
            let asking = bridge.clone();
            let pending = tokio::spawn(async move { asking.dialog(request, None).await });
            let wire: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            requests.push((wire, pending));
        }
        assert_eq!(requests[0].0["options"], json!(["one", "two"]));
        assert_eq!(requests[1].0["placeholder"], "empty is valid");
        assert_eq!(requests[2].0["prefill"], "old\ntext");
        // Answer in reverse order, with a deliberately unrelated approval first.
        bridge
            .response(&json!({"type":"extension_ui_response", "id":"unrelated", "confirmed":true}));
        assert!(requests.iter().all(|(_, pending)| !pending.is_finished()));
        for ((wire, pending), answer) in requests.into_iter().rev().zip(["new\ntext", "", "two"]) {
            bridge.response(
                &json!({"type":"extension_ui_response", "id":wire["id"], "value":answer}),
            );
            assert_eq!(
                pending.await.unwrap().unwrap(),
                Some(UiDialogResponse::Value(answer.into()))
            );
        }
        assert!(bridge.state.lock().unwrap().pending.is_empty());
    }

    #[tokio::test]
    async fn timeout_and_cancellation_remove_dialogs_before_late_responses() {
        let (writer, reader) = tokio::io::duplex(4096);
        let bridge = Arc::new(RpcUiBridge::new(RpcOutput::with_writer(writer)));
        let mut lines = tokio::io::BufReader::new(reader).lines();
        let asking = bridge.clone();
        let timed = tokio::spawn(async move {
            asking
                .dialog(
                    UiDialogRequest::Input {
                        title: "timed".into(),
                        placeholder: None,
                    },
                    Some(10),
                )
                .await
        });
        let old: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(old["timeout"], 10);
        assert_eq!(
            timed.await.unwrap().unwrap(),
            Some(UiDialogResponse::Cancelled)
        );
        assert!(bridge.state.lock().unwrap().pending.is_empty());
        let asking = bridge.clone();
        let active = tokio::spawn(async move {
            asking
                .dialog(
                    UiDialogRequest::Editor {
                        title: "next".into(),
                        prefill: None,
                    },
                    None,
                )
                .await
        });
        let new: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert!(new.get("timeout").is_none());
        bridge.response(&json!({"type":"extension_ui_response", "id":old["id"], "value":"stale"}));
        assert_eq!(bridge.state.lock().unwrap().pending.len(), 1);
        bridge.response(&json!({"type":"extension_ui_response", "id":new["id"], "value":"discarded", "cancelled":true}));
        assert_eq!(
            active.await.unwrap().unwrap(),
            Some(UiDialogResponse::Cancelled)
        );
    }

    #[tokio::test]
    async fn startup_notices_are_written_before_the_next_question() {
        let (writer, reader) = tokio::io::duplex(4096);
        let bridge = Arc::new(RpcUiBridge::new(RpcOutput::with_writer(writer)));
        let mut lines = tokio::io::BufReader::new(reader).lines();
        assert!(
            bridge
                .notify(NoticeLevel::Warning, "startup".into())
                .unwrap()
        );
        let asking = bridge.clone();
        let pending = tokio::spawn(async move {
            asking
                .dialog(
                    UiDialogRequest::Input {
                        title: "startup question".into(),
                        placeholder: None,
                    },
                    None,
                )
                .await
        });
        let notice: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(notice["method"], "notify");
        assert_eq!(notice["message"], "startup");
        assert_eq!(notice["notifyType"], "warning");
        let question: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(question["method"], "input");
        bridge.close();
        assert_eq!(
            pending.await.unwrap().unwrap(),
            Some(UiDialogResponse::Cancelled)
        );
    }
}
