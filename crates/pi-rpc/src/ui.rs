//! Pi's semantic extension dialogs and notifications over the shared RPC connection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_plugin::{NoticeLevel, UiDialogRequest, UiDialogResponse};
use pi_session::PluginUiBridge;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::sync::{mpsc, oneshot};

use crate::output::RpcOutput;
use crate::rpc::MAX_RPC_LINE_BYTES;

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
}

impl RpcUiBridge {
    fn new(output: RpcOutput) -> Self {
        Self {
            state: Arc::default(),
            output,
        }
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
        let (kind, mut request) = match request {
            UiDialogRequest::Confirm { title, message } => (
                AnswerKind::Confirmation,
                json!({"method":"confirm", "title":title, "message":message}),
            ),
            UiDialogRequest::Select { title, options } => (
                AnswerKind::Text,
                json!({"method":"select", "title":title, "options":options}),
            ),
            UiDialogRequest::Input { title, placeholder } => (
                AnswerKind::Text,
                json!({"method":"input", "title":title, "placeholder":placeholder}),
            ),
            UiDialogRequest::Editor { title, prefill } => (
                AnswerKind::Text,
                json!({"method":"editor", "title":title, "prefill":prefill}),
            ),
        };
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
            if self.output.emit(request).await.is_err() {
                self.close();
                return UiDialogResponse::Cancelled;
            }
            result.await.unwrap_or(UiDialogResponse::Cancelled)
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
        self.output.enqueue(json!({
            "type":"extension_ui_request", "id":uuid::Uuid::now_v7().to_string(),
            "method":"notify", "message":message, "notifyType":level,
        }))?;
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
        if line.len() <= MAX_RPC_LINE_BYTES
            && let Ok(value) = serde_json::from_slice::<Value>(&line)
        {
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
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

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
                placeholder: "empty is valid".into(),
            },
            UiDialogRequest::Editor {
                title: "edit".into(),
                prefill: "old\ntext".into(),
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
                        placeholder: String::new(),
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
                        prefill: String::new(),
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
                        placeholder: String::new(),
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
