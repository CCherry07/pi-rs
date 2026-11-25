//! Opaque credential revisions for configured, enabled HTTP identities.
//! Hosts choose when to replace generations; MCP never mutates live registries.
use crate::{McpOAuthCredentialStore, McpServerConfig, McpTransport};
use std::collections::BTreeSet;

#[derive(Clone)]
pub struct McpCredentialWatch {
    store: McpOAuthCredentialStore,
    oauth: Vec<(String, String)>,
    providers: BTreeSet<String>,
}
impl McpCredentialWatch {
    pub(crate) fn new(store: McpOAuthCredentialStore, configs: Vec<McpServerConfig>) -> Self {
        let mut watch = Self {
            store,
            oauth: Vec::new(),
            providers: BTreeSet::new(),
        };
        for config in configs {
            let McpTransport::Http { url, headers, .. } = config.transport else {
                continue;
            };
            if headers
                .keys()
                .any(|key| key.eq_ignore_ascii_case("authorization"))
            {
                continue;
            }
            if let Some(auth) = config.auth {
                watch.providers.insert(auth.provider);
            } else {
                watch.oauth.push((config.name, url));
            }
        }
        watch
    }
    pub fn is_empty(&self) -> bool {
        self.oauth.is_empty() && self.providers.is_empty()
    }
    pub fn provider_names(&self) -> &BTreeSet<String> {
        &self.providers
    }
    /// Excludes pending login state, client-only registrations and other servers.
    /// No credential bytes are exposed to the caller or its diagnostics.
    pub fn oauth_revision(&self) -> Result<[u8; 32], String> {
        if self.oauth.is_empty() {
            return Ok([0; 32]);
        }
        self.store.credentials_revision(&self.oauth)
    }
}
