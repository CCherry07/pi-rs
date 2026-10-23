use async_trait::async_trait;
use pi_plugin::{UiSurfaceAction, UiSurfaceEvent, UiSurfaceFrame, UiSurfaceSnapshot};
use std::sync::{Arc, RwLock};

pub(crate) enum SurfaceRequest {
    Exchange(UiSurfaceFrame, oneshot::Sender<UiSurfaceEvent>),
    Action(String, UiSurfaceAction),
}
pub(crate) struct SurfaceReceiver {
    pub(crate) theme: Option<String>,
    pub(crate) external_editor: Option<String>,
    pub(crate) show_images: bool,
    pub(crate) startup: Option<crate::tui::extension_startup::StartupUi>,
    pub(crate) initial: std::collections::VecDeque<SurfaceRequest>,
    pub(crate) requests: mpsc::UnboundedReceiver<SurfaceRequest>,
    pub(crate) snapshot: Arc<RwLock<UiSurfaceSnapshot>>,
}

use pi_plugin::{UiMultiSelectRequest, UiMultiSelectResponse};
use pi_session::PluginUiBridge;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub(crate) struct PluginMultiSelectionRequest {
    pub(crate) request: UiMultiSelectRequest,
    pub(crate) response: oneshot::Sender<Option<UiMultiSelectResponse>>,
}

#[derive(Clone)]
pub(crate) struct PluginUiService {
    surfaces: mpsc::UnboundedSender<SurfaceRequest>,
    snapshot: Arc<RwLock<UiSurfaceSnapshot>>,
    multi_selections: mpsc::UnboundedSender<PluginMultiSelectionRequest>,
}

impl PluginUiService {
    pub(crate) fn channel() -> (
        Self,
        mpsc::UnboundedReceiver<PluginMultiSelectionRequest>,
        SurfaceReceiver,
    ) {
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
        let (multi_selections, multi_selection_receiver) = mpsc::unbounded_channel();
        (
            Self {
                surfaces,
                snapshot,
                multi_selections,
            },
            multi_selection_receiver,
            surface_receiver,
        )
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
    fn surface_action(&self, owner: String, action: UiSurfaceAction) -> Result<(), String> {
        self.surfaces
            .send(SurfaceRequest::Action(owner, action))
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

    async fn multi_select(
        &self,
        request: UiMultiSelectRequest,
    ) -> Result<Option<UiMultiSelectResponse>, String> {
        let (response, receiver) = oneshot::channel();
        self.multi_selections
            .send(PluginMultiSelectionRequest { request, response })
            .map_err(|_| "interactive multi-selection is unavailable".to_string())?;
        receiver
            .await
            .map_err(|_| "interactive multi-selection was dismissed".to_string())
    }
}

impl SurfaceReceiver {
    pub(crate) async fn recv(&mut self) -> Option<SurfaceRequest> {
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
        let (ui, _, mut surfaces) = PluginUiService::channel();
        assert!(
            ui.surface_exchange(UiSurfaceFrame::default())
                .await
                .is_err()
        );
        assert!(surfaces.requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn multi_selection_round_trips_the_frontend_action() {
        let (ui, mut multi_selections, _surfaces) = PluginUiService::channel();
        let request = UiMultiSelectRequest {
            title: "Skills".to_string(),
            options: Vec::new(),
            actions: Vec::new(),
            categories: Vec::new(),
            sort_modes: Vec::new(),
            initially_selected: Vec::new(),
            initial_query: String::new(),
            initial_active_categories: Vec::new(),
            initial_sort_mode: 0,
            summary_lines: Vec::new(),
        };
        let selection = tokio::spawn({
            let request = request.clone();
            async move { ui.multi_select(request).await }
        });
        let pending = multi_selections.recv().await.unwrap();
        assert_eq!(pending.request, request);
        pending
            .response
            .send(Some(UiMultiSelectResponse {
                selected: vec![0, 2],
                action_id: "move-global".to_string(),
                query: "memory".to_string(),
                active_categories: vec!["G".to_string()],
                sort_mode: 2,
            }))
            .unwrap();
        assert_eq!(
            selection.await.unwrap().unwrap(),
            Some(UiMultiSelectResponse {
                selected: vec![0, 2],
                action_id: "move-global".to_string(),
                query: "memory".to_string(),
                active_categories: vec!["G".to_string()],
                sort_mode: 2,
            })
        );
    }
}
