//! Host-owned authorization interaction. MCP validates/scopes the target and owns tokens;
//! browser processes, callback listeners and terminal input belong to the embedding frontend.
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::AbortSignal;

use crate::{
    McpOAuthConfig, McpOAuthCredentialStore, McpOAuthError, McpOAuthRedirect, McpOAuthSignIn,
};

pub struct McpOAuthLoginRequest {
    pub(crate) name: String,
    pub(crate) server_url: String,
    pub(crate) settings: McpOAuthConfig,
    pub(crate) store: McpOAuthCredentialStore,
    pub(crate) challenge: Option<String>,
}

impl McpOAuthLoginRequest {
    pub fn server_name(&self) -> &str {
        &self.name
    }
    pub fn callback_url(&self) -> Option<&str> {
        self.settings.callback_url.as_deref()
    }
    pub fn callback_port(&self) -> Option<u16> {
        self.settings.callback_port
    }

    /// The host supplies the actual bound redirect, including the listening port.
    /// The resulting redirect may have a CIMD-specific path; validate callbacks
    /// against that returned redirect, never just the originally requested path.
    pub async fn start(
        &self,
        redirect_url: &str,
        signal: AbortSignal,
    ) -> Result<(McpOAuthSignIn, McpOAuthRedirect), McpOAuthError> {
        McpOAuthSignIn::start(
            &self.name,
            &self.server_url,
            &self.settings,
            self.store.clone(),
            redirect_url,
            self.challenge.as_deref(),
            signal,
        )
        .await
    }
}

#[async_trait]
pub trait McpOAuthPrompt: Send + Sync {
    fn show_authorization_url(&self, url: &str) -> Result<(), String>;
    fn notify(&self, message: &str) -> Result<(), String>;
    /// `None` means the person cancelled. A noninteractive presenter should wait
    /// for cancellation instead of returning immediately and racing the callback.
    async fn redirect_url(&self, signal: AbortSignal) -> Result<Option<String>, String>;
}

#[async_trait]
pub trait McpOAuthHost: Send + Sync {
    async fn sign_in(
        &self,
        request: McpOAuthLoginRequest,
        prompt: Arc<dyn McpOAuthPrompt>,
        signal: AbortSignal,
    ) -> Result<(), McpOAuthError>;
}

/// Safe default for embeddings without a browser/listener adapter. The host can
/// still answer a semantic pasted-redirect prompt; no OS application is launched.
pub(crate) struct ManualOAuthHost;

#[async_trait]
impl McpOAuthHost for ManualOAuthHost {
    async fn sign_in(
        &self,
        request: McpOAuthLoginRequest,
        prompt: Arc<dyn McpOAuthPrompt>,
        signal: AbortSignal,
    ) -> Result<(), McpOAuthError> {
        let redirect_url = request
            .callback_url()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "http://127.0.0.1:{}/callback",
                    request.callback_port().unwrap_or(53682)
                )
            });
        let (mut flow, redirect) = request.start(&redirect_url, signal.clone()).await?;
        prompt
            .show_authorization_url(&redirect.authorization_url)
            .map_err(McpOAuthError::Failed)?;
        prompt.notify("Open the URL manually. No callback listener is installed; paste the complete redirected address even if the browser shows a connection error.").map_err(McpOAuthError::Failed)?;
        let response = tokio::select! {
            biased;
            () = signal.wait() => return Err(McpOAuthError::Cancelled),
            response = prompt.redirect_url(signal.clone()) => response.map_err(McpOAuthError::Failed)?,
        }.ok_or(McpOAuthError::Cancelled)?;
        flow.complete_redirect_url(response.trim(), signal).await
    }
}
