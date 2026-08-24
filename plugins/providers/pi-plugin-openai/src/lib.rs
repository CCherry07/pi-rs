#![forbid(unsafe_code)]

mod codex;
mod config;
mod oauth;
mod plugin;
mod provider;
mod request;
pub mod responses;
mod stream;

pub use codex::{CodexCredentials, OpenAiCodexProvider};
pub use config::{OpenAiCompatibleConfig, OpenAiConfig};
pub use oauth::{
    DeviceAuthorization as OpenAiDeviceAuthorization, OAuthCredential as OpenAiOAuthCredential,
    poll_device_authorization as poll_openai_device_authorization, refresh as refresh_openai_oauth,
    start_device_authorization as start_openai_device_authorization,
};
pub use plugin::{
    OpenAiCodexCatalogPlugin, OpenAiCodexPlugin, OpenAiCompatiblePlugin, OpenAiPlugin,
    openai_codex_models,
};
pub use provider::{OpenAiCompatibleProvider, OpenAiProvider};
pub use request::OpenAiCompletionsCompat;
pub use responses::{
    OPENAI_RESPONSES_API, OpenAiResponsesCompat, OpenAiResponsesCompatibleProvider,
};
