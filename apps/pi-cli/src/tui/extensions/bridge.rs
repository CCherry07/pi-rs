//! Product bridge from generation capabilities into the interactive extension frontend.
use async_trait::async_trait;
use pi_plugin::{UiCommand, UiSurfaceEvent, UiSurfaceFrame, UiSurfaceSnapshot};
use std::sync::{Arc, RwLock};

pub(in crate::tui) enum SurfaceRequest {
    Exchange(UiSurfaceFrame, oneshot::Sender<UiSurfaceEvent>),
    Command(String, UiCommand),
}
pub(crate) struct SurfaceReceiver {
    pub(crate) theme: Option<String>,
    pub(crate) external_editor: Option<String>,
    pub(crate) show_images: bool,
    pub(super) startup: Option<super::startup::StartupUi>,
    initial: std::collections::VecDeque<SurfaceRequest>,
    requests: mpsc::UnboundedReceiver<SurfaceRequest>,
    pub(crate) snapshot: Arc<RwLock<UiSurfaceSnapshot>>,
}

use pi_session::PluginUiBridge;
use tokio::sync::{mpsc, oneshot};

#[derive(Clone)]
pub(crate) struct PluginUiService {
    surfaces: mpsc::UnboundedSender<SurfaceRequest>,
    snapshot: Arc<RwLock<UiSurfaceSnapshot>>,
}

impl PluginUiService {
    pub(crate) fn channel() -> (Self, SurfaceReceiver) {
        let (surfaces, requests) = mpsc::unbounded_channel();
        let snapshot = Arc::new(RwLock::new(UiSurfaceSnapshot::default()));
        let surface_receiver = SurfaceReceiver {
            theme: None,
            external_editor: None,
            show_images: true,
            startup: None,
            initial: Default::default(),
            requests,
            snapshot: snapshot.clone(),
        };
        (Self { surfaces, snapshot }, surface_receiver)
    }
}

#[async_trait]
impl PluginUiBridge for PluginUiService {
    fn surface_snapshot(&self) -> Result<UiSurfaceSnapshot, String> {
        Ok(self
            .snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }
    fn ui_command(&self, owner: String, command: UiCommand) -> Result<(), String> {
        self.surfaces
            .send(SurfaceRequest::Command(owner, command))
            .map_err(|_| "extension UI closed".into())
    }
    async fn surface_exchange(&self, frame: UiSurfaceFrame) -> Result<UiSurfaceEvent, String> {
        frame.validate()?;
        let (sender, receiver) = oneshot::channel();
        self.surfaces
            .send(SurfaceRequest::Exchange(frame, sender))
            .map_err(|_| "extension UI closed".to_string())?;
        Ok(receiver.await.unwrap_or(UiSurfaceEvent::Closed))
    }
}

impl SurfaceReceiver {
    pub(in crate::tui) fn take_startup(&mut self) -> Option<super::startup::StartupUi> {
        self.startup.take()
    }

    pub(super) fn defer(&mut self, request: SurfaceRequest) {
        self.initial.push_back(request);
    }

    pub(super) async fn recv_live(&mut self) -> Option<SurfaceRequest> {
        self.requests.recv().await
    }

    pub(in crate::tui) async fn recv(&mut self) -> Option<SurfaceRequest> {
        if let Some(request) = self.initial.pop_front() {
            return Some(request);
        }
        self.requests.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn malformed_document_is_rejected_before_entering_frontend() {
        let (ui, mut surfaces) = PluginUiService::channel();
        assert!(
            ui.surface_exchange(UiSurfaceFrame::default())
                .await
                .is_err()
        );
        assert!(surfaces.requests.try_recv().is_err());
    }
}
