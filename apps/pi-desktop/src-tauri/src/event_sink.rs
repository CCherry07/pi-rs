use tauri::{AppHandle, Emitter};

use crate::backend::events::{EventSink, TerminalExit, TerminalOutput};

#[derive(Clone)]
pub(crate) struct TauriEventSink {
    app: AppHandle,
}

impl TauriEventSink {
    pub(crate) fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl EventSink for TauriEventSink {
    fn emit_terminal_output(&self, event: TerminalOutput) {
        let _ = self.app.emit("terminal-output", event);
    }

    fn emit_terminal_exit(&self, event: TerminalExit) {
        let _ = self.app.emit("terminal-exit", event);
    }
}
