use async_trait::async_trait;
use pi_session::PluginUiBridge;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub(crate) struct PluginConfirmationRequest {
    pub(crate) title: String,
    pub(crate) message: String,
    pub(crate) response: oneshot::Sender<bool>,
}

#[derive(Clone)]
pub(crate) struct PluginUiService {
    requests: mpsc::UnboundedSender<PluginConfirmationRequest>,
}

impl PluginUiService {
    pub(crate) fn channel() -> (Self, mpsc::UnboundedReceiver<PluginConfirmationRequest>) {
        let (requests, receiver) = mpsc::unbounded_channel();
        (Self { requests }, receiver)
    }
}

#[async_trait]
impl PluginUiBridge for PluginUiService {
    async fn confirm(&self, title: String, message: String) -> Result<bool, String> {
        let (response, receiver) = oneshot::channel();
        self.requests
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn confirmation_round_trips_the_frontend_decision() {
        let (ui, mut requests) = PluginUiService::channel();
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
}
