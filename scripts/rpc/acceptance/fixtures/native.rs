//! Acceptance-only plugin. Build against the *same frozen SDK* as the tested host.
//! No signal handlers, runtime mocks, terminal access, or provider implementation.
use pi_plugin::prelude::*;
use std::{collections::BTreeMap, sync::Mutex, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone)]
struct Control {
    port: u16,
    runtime: tokio::runtime::Handle,
}

impl Control {
    async fn post(&self, path: &str, body: Value) -> std::result::Result<Value, PluginError> {
        // A cdylib has its own Tokio TLS. Do not assume the host's runtime is
        // visible to this separately linked copy of Tokio.
        let path = path.to_owned();
        let port = self.port;
        self.runtime.spawn(async move {
            let exchange = async {
                let body = body.to_string();
                let request = format!(
                    "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
                );
                let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
                socket.write_all(request.as_bytes()).await?;
                let mut response = Vec::new();
                socket.take(64 * 1024).read_to_end(&mut response).await?;
                let response = String::from_utf8(response).map_err(std::io::Error::other)?;
                let (headers, body) = response.split_once("\r\n\r\n").ok_or_else(|| std::io::Error::other("missing control HTTP header"))?;
                if !headers.starts_with("HTTP/1.1 200 ") { return Err(std::io::Error::other("control endpoint rejected fixture")); }
                serde_json::from_str(body).map_err(std::io::Error::other)
            };
            tokio::time::timeout(Duration::from_secs(20), exchange).await
                .map_err(|error| PluginError::Failure(error.to_string()))?
                .map_err(|error| PluginError::Failure(error.to_string()))
        }).await.map_err(|error| PluginError::Failure(error.to_string()))?
    }

    async fn event(&self, name: &str, data: Value) -> std::result::Result<(), PluginError> {
        self.post("/control/event", json!({ "name": name, "data": data }))
            .await?;
        Ok(())
    }
}

pub struct AcceptancePlugin {
    control: Control,
    runtime: Option<tokio::runtime::Runtime>,
    mode: String,
    hold_shutdown: bool,
    background: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for AcceptancePlugin {
    fn drop(&mut self) {
        if let Some(task) = self
            .background
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

#[pi_plugin::native_plugin(factory)]
impl Plugin for AcceptancePlugin {
    type Options = BTreeMap<String, String>;

    fn prepare(_context: &PrepareContext, _options: Self::Options) -> PrepareResult<Option<Self>> {
        let port = std::env::var("RPC_ACCEPTANCE_CONTROL_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .filter(|port| *port > 0)
            .ok_or_else(|| {
                PrepareError::initialization(
                    "RPC_ACCEPTANCE_CONTROL_PORT must be a loopback fixture port",
                )
            })?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|error| PrepareError::initialization(error.to_string()))?;
        Ok(Some(Self {
            control: Control {
                port,
                runtime: runtime.handle().clone(),
            },
            runtime: Some(runtime),
            mode: std::env::var("RPC_ACCEPTANCE_FIXTURE").unwrap_or_default(),
            hold_shutdown: std::env::var("RPC_ACCEPTANCE_HOLD_SHUTDOWN").as_deref() == Ok("1"),
            background: Mutex::new(None),
        }))
    }

    async fn session_start(
        &self,
        context: &SessionPluginContext,
        _event: &SessionStartEvent,
    ) -> std::result::Result<(), PluginError> {
        if self.mode == "background-ui" {
            let ui = context.ui.clone();
            let control = self.control.clone();
            let task = self.control.runtime.spawn(async move {
                let result = ui
                    .confirm("acceptance confirm", "No side effects will be executed")
                    .await;
                let data = match result {
                    Ok(answer) => json!({ "answer": answer }),
                    Err(error) => json!({ "error": error.to_string() }),
                };
                if let Err(error) = control.event("ui-released", data).await {
                    eprintln!("acceptance fixture: {error}");
                }
            });
            *self
                .background
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
        }
        self.control.event("fixture-ready", json!({})).await
    }

    async fn input(
        &self,
        context: InputContext,
        _event: InputEvent,
    ) -> std::result::Result<InputPatch, PluginError> {
        if self.mode != "input-ui" {
            return Ok(InputPatch::Continue);
        }
        let answer = context
            .ui
            .confirm("acceptance confirm", "No side effects will be executed")
            .await?;
        self.control
            .event("ui-released", json!({ "answer": answer }))
            .await?;
        Ok(InputPatch::Handled)
    }

    async fn tool_call(
        &self,
        context: AgentPluginContext,
        _event: ToolCallEvent,
    ) -> std::result::Result<ToolCallPatch, PluginError> {
        if self.mode != "tool-ui" {
            return Ok(ToolCallPatch::default());
        }
        let answer = context
            .ui
            .confirm("acceptance confirm", "No side effects will be executed")
            .await?;
        self.control
            .event("ui-released", json!({ "answer": answer }))
            .await?;
        Ok(ToolCallPatch {
            block: Some(ToolCallBlock {
                reason: "acceptance fixture blocks writes".into(),
                terminate: false,
            }),
            ..ToolCallPatch::default()
        })
    }

    async fn session_shutdown(
        &self,
        context: &SessionPluginContext,
        _event: &SessionShutdownEvent,
    ) -> std::result::Result<(), PluginError> {
        self.control.event("shutdown-enter", json!({})).await?;
        if self.hold_shutdown {
            loop {
                let value = self
                    .control
                    .post("/control/gate", json!({ "name": "shutdown" }))
                    .await?;
                if value["action"] == "release" {
                    break;
                }
                self.control.event("shutdown-pulse", json!({})).await?;
            }
        }
        self.control.event("shutdown-complete", json!({})).await?;
        context
            .ui
            .notify(NoticeLevel::Info, "acceptance shutdown complete")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };

    #[test]
    fn runtime_cleanup_drops_pending_tasks_and_stops_workers_without_blocking_host() {
        let (finished, completion) = mpsc::channel();
        let host_thread = std::thread::spawn(move || {
            let host = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            host.block_on(async {
                let (stopped, worker_stopped) = mpsc::channel();
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .on_thread_stop(move || {
                        let _ = stopped.send(());
                    })
                    .build()
                    .unwrap();
                let dropped = Arc::new(AtomicBool::new(false));
                struct Guard(Arc<AtomicBool>);
                impl Drop for Guard {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::SeqCst);
                    }
                }
                let (entered, entering) = tokio::sync::oneshot::channel();
                let observed = dropped.clone();
                let task = runtime.spawn(async move {
                    let _guard = Guard(observed);
                    let _ = entered.send(());
                    std::future::pending::<()>().await;
                });
                let plugin = AcceptancePlugin {
                    control: Control {
                        port: 9,
                        runtime: runtime.handle().clone(),
                    },
                    runtime: Some(runtime),
                    mode: String::new(),
                    hold_shutdown: false,
                    background: Mutex::new(Some(task)),
                };
                entering.await.unwrap();
                drop(plugin);
                tokio::task::yield_now().await;
                tokio::task::spawn_blocking(move || {
                    worker_stopped.recv_timeout(Duration::from_secs(2))
                })
                .await
                .unwrap()
                .unwrap();
                assert!(
                    dropped.load(Ordering::SeqCst),
                    "owned runtime left a pending future alive"
                );
            });
            finished.send(()).unwrap();
        });
        completion
            .recv_timeout(Duration::from_secs(5))
            .expect("fixture Drop blocked the current-thread host");
        host_thread.join().unwrap();
    }
}
