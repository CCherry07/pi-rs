#![forbid(unsafe_code)]

//! First-party MCP integration with explicit-server and local-library entry points.
//!
//! [`McpToolSet`] connects caller-supplied configurations without reading local files
//! or registering management commands. [`McpLibrary`] manages scoped local configuration
//! and prepares a plugin with discovered tools plus `/mcp`, using the caller's trust decision.

mod client;
pub mod config;
mod library;
mod plugin;

pub use client::{
    McpError, McpServerConfig, McpToolDescriptor, McpToolSet, McpTransport, validate_configs,
};
pub use library::{McpDocument, McpLibrary, McpScope, McpServerRow};
