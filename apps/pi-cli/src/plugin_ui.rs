use async_trait::async_trait;
use pi_plugin::{UiSurfaceAction, UiSurfaceEvent, UiSurfaceFrame, UiSurfaceSnapshot};
use std::sync::{Arc, RwLock};

pub(crate) enum SurfaceRequest {
    Exchange(UiSurfaceFrame, oneshot::Sender<UiSurfaceEvent>),
    Action(String, UiSurfaceAction),
}
pub(crate) struct SurfaceReceiver {
    pub(crate) initial: std::collections::VecDeque<SurfaceRequest>,
    pub(crate) requests: mpsc::UnboundedReceiver<SurfaceRequest>,
    pub(crate) snapshot: Arc<RwLock<UiSurfaceSnapshot>>,
}

use pi_plugin::{UiMultiSelectRequest, UiMultiSelectResponse};
use pi_session::PluginUiBridge;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub(crate) struct PluginConfirmationRequest {
    pub(crate) title: String,
    pub(crate) message: String,
    pub(crate) response: oneshot::Sender<bool>,
}

#[derive(Debug)]
pub(crate) struct PluginSelectionRequest {
    pub(crate) title: String,
    pub(crate) options: Vec<String>,
    pub(crate) response: oneshot::Sender<Option<usize>>,
}

#[derive(Debug)]
pub(crate) struct PluginMultiSelectionRequest {
    pub(crate) request: UiMultiSelectRequest,
    pub(crate) response: oneshot::Sender<Option<UiMultiSelectResponse>>,
}

#[derive(Clone)]
pub(crate) struct PluginUiService {
    surfaces: mpsc::UnboundedSender<SurfaceRequest>,
    snapshot: Arc<RwLock<UiSurfaceSnapshot>>,
    confirmations: mpsc::UnboundedSender<PluginConfirmationRequest>,
    selections: mpsc::UnboundedSender<PluginSelectionRequest>,
    multi_selections: mpsc::UnboundedSender<PluginMultiSelectionRequest>,
}

impl PluginUiService {
    pub(crate) fn channel() -> (
        Self,
        mpsc::UnboundedReceiver<PluginConfirmationRequest>,
        mpsc::UnboundedReceiver<PluginSelectionRequest>,
        mpsc::UnboundedReceiver<PluginMultiSelectionRequest>,
        SurfaceReceiver,
    ) {
        let (surfaces, requests) = mpsc::unbounded_channel();
        let snapshot = Arc::new(RwLock::new(UiSurfaceSnapshot::default()));
        let surface_receiver = SurfaceReceiver {
            initial: Default::default(),
            requests,
            snapshot: snapshot.clone(),
        };
        let (confirmations, confirmation_receiver) = mpsc::unbounded_channel();
        let (selections, selection_receiver) = mpsc::unbounded_channel();
        let (multi_selections, multi_selection_receiver) = mpsc::unbounded_channel();
        (
            Self {
                surfaces,
                snapshot,
                confirmations,
                selections,
                multi_selections,
            },
            confirmation_receiver,
            selection_receiver,
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
        let (sender, receiver) = oneshot::channel();
        self.surfaces
            .send(SurfaceRequest::Exchange(frame, sender))
            .map_err(|_| "extension UI closed".to_string())?;
        Ok(receiver.await.unwrap_or(UiSurfaceEvent::Closed))
    }

    async fn confirm(&self, title: String, message: String) -> Result<bool, String> {
        let (response, receiver) = oneshot::channel();
        self.confirmations
            .send(PluginConfirmationRequest {
                title,
                message,
                response,
            })
            .map_err(|_| "interactive confirmation is unavailable".to_string())?;
        receiver
            .await
            .map_err(|_| "interactive confirmation was dismissed".to_string())
    }

    async fn select(&self, title: String, options: Vec<String>) -> Result<Option<usize>, String> {
        let (response, receiver) = oneshot::channel();
        self.selections
            .send(PluginSelectionRequest {
                title,
                options,
                response,
            })
            .map_err(|_| "interactive selection is unavailable".to_string())?;
        receiver
            .await
            .map_err(|_| "interactive selection was dismissed".to_string())
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
    async fn confirmation_round_trips_the_frontend_decision() {
        let (ui, mut requests, _selections, _multi_selections, _surfaces) =
            PluginUiService::channel();
        let confirmation = tokio::spawn(async move {
            ui.confirm("Import?".to_string(), "Replace session".to_string())
                .await
        });
        let request = requests.recv().await.unwrap();
        assert_eq!(request.title, "Import?");
        assert_eq!(request.message, "Replace session");
        request.response.send(true).unwrap();
        assert!(confirmation.await.unwrap().unwrap());
    }

    #[tokio::test]
    async fn selection_round_trips_the_frontend_choice() {
        let (ui, _confirmations, mut selections, _multi_selections, _surfaces) =
            PluginUiService::channel();
        let selection = tokio::spawn(async move {
            ui.select(
                "Choose".to_string(),
                vec!["One".to_string(), "Two".to_string()],
            )
            .await
        });
        let request = selections.recv().await.unwrap();
        assert_eq!(request.title, "Choose");
        assert_eq!(request.options, ["One", "Two"]);
        request.response.send(Some(1)).unwrap();
        assert_eq!(selection.await.unwrap().unwrap(), Some(1));
    }

    #[tokio::test]
    async fn multi_selection_round_trips_the_frontend_action() {
        let (ui, _confirmations, _selections, mut multi_selections, _surfaces) =
            PluginUiService::channel();
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
