#![forbid(unsafe_code)]

//! First-party MCP integration with explicit-server and local-library entry points.
//!
//! [`McpToolSet`] connects caller-supplied configurations without reading local files
//! or registering management commands. [`McpLibrary`] manages scoped local configuration
//! and prepares a plugin with discovered tools plus `/mcp`, using the caller's trust decision.

mod auth;
mod client;
mod client_handler;
pub mod config;
mod directory;
mod library;
mod naming;
mod output_schema;
mod plugin;
mod policy;
mod resources;
mod result;

pub use auth::{
    McpClientAuth, McpOAuthCredentialStore, McpOAuthError, McpOAuthRedirect, McpOAuthSignIn,
    McpOAuthTokens, McpProviderTokenResolver,
};
pub use client::{
    McpError, McpOAuthClientRegistration, McpOAuthConfig, McpProviderAuth, McpServerConfig,
    McpToolDescriptor, McpToolSet, McpTransport, validate_configs,
};
pub use library::{McpDocument, McpLibrary, McpScope, McpServerRow};
