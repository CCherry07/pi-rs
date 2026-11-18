//! Process signals keep RPC disposal alive until cleanup completes or a signal repeats.

use std::sync::{Arc, Mutex};

use tokio::sync::watch;

#[derive(Default)]
struct State {
    shutting_down: bool,
    signal_count: usize,
    exit_code: i32,
}

pub(crate) struct RpcSignals {
    state: Arc<Mutex<State>>,
    requested: watch::Receiver<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl RpcSignals {
    pub(crate) fn new() -> Result<Self, String> {
        let state = Arc::new(Mutex::new(State::default()));
        let (sender, requested) = watch::channel(false);
        let signal_state = Arc::clone(&state);
        #[cfg(unix)]
        let task = {
            use tokio::signal::unix::{SignalKind, signal};

            let mut terminate =
                signal(SignalKind::terminate()).map_err(|error| error.to_string())?;
            let mut hangup = signal(SignalKind::hangup()).map_err(|error| error.to_string())?;
            tokio::spawn(async move {
                loop {
                    let code = tokio::select! {
                        _ = terminate.recv() => 143,
                        _ = hangup.recv() => 129,
                    };
                    let repeated = {
                        let mut state = signal_state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.signal_count += 1;
                        if !state.shutting_down {
                            state.shutting_down = true;
                            state.exit_code = code;
                        }
                        state.signal_count > 1
                    };
                    if repeated {
                        std::process::exit(code);
                    }
                    sender.send_replace(true);
                }
            })
        };
        #[cfg(not(unix))]
        let task = tokio::spawn(async move {
            let _keep_alive = (signal_state, sender);
            std::future::pending::<()>().await;
        });
        Ok(Self {
            state,
            requested,
            task,
        })
    }

    pub(crate) async fn wait(&self) {
        let mut requested = self.requested.clone();
        while !*requested.borrow() {
            if requested.changed().await.is_err() {
                return;
            }
        }
    }

    pub(crate) fn begin_shutdown(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .shutting_down = true;
    }

    pub(crate) fn exit_code(&self) -> i32 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .exit_code
    }
}

impl Drop for RpcSignals {
    fn drop(&mut self) {
        self.task.abort();
    }
}
