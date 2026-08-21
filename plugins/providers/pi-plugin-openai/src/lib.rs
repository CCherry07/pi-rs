#![forbid(unsafe_code)]

mod config;
mod plugin;
mod provider;
mod request;
mod stream;

pub use config::{OpenAiCompatibleConfig, OpenAiConfig};
pub use plugin::{OpenAiCompatiblePlugin, OpenAiPlugin};
pub use provider::{OpenAiCompatibleProvider, OpenAiProvider};
