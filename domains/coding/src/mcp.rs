//! Coding's credential adapter for MCP. Configuration/trust and transport policy stay in MCP.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pi_plugin_mcp::{McpClientAuth, McpOAuthCredentialStore, McpProviderTokenResolver};

use crate::credentials::{CredentialPrecedence, StoredCredentialSource};

/// Supplies the same durable MCP store and request-time provider credentials to
/// both Coding generations and session-free shell management commands.
///
/// Like the current Pi MCP provider-token reader, this consults the named
/// provider's credentials, not the selected model's explicit CLI API key.
pub fn mcp_client_auth(agent_dir: &Path) -> McpClientAuth {
    McpClientAuth::default()
        .with_oauth_store(McpOAuthCredentialStore::new(
            agent_dir.join("mcp-auth.json"),
        ))
        .with_provider_tokens(Arc::new(CodingProviderTokens(agent_dir.to_owned())))
}

/// A generation-local, weak live selection reader for MCP startup activation. Binding
/// happens before session hooks, so an earlier hook's explicit deselection wins. Retained
/// executable plugins can retain this reader, but never the session or its freshness lease.
#[derive(Clone, Default)]
pub(crate) struct McpToolSelectionBinding {
    session: Arc<Mutex<std::sync::Weak<pi_session::AgentSession>>>,
}

impl McpToolSelectionBinding {
    pub(crate) fn bind(&self, session: &Arc<pi_session::AgentSession>) {
        *self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::downgrade(session);
    }

    pub(crate) fn reader(&self) -> Arc<dyn Fn() -> Result<Vec<String>, String> + Send + Sync> {
        let binding = self.clone();
        Arc::new(move || {
            let session = binding
                .session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .upgrade()
                .ok_or("MCP tool selection is not bound to a live session")?;
            session
                .desired_active_tools()
                .map_err(|error| error.to_string())
        })
    }
}

/// The key identifies a live runtime, never a persisted session path. Two managers may open
/// the same path through one factory; neither may borrow the other's transports or credentials.
#[derive(Clone, Default)]
pub(crate) struct McpSessionCache {
    entries: Arc<
        Mutex<
            HashMap<pi_session::LiveRuntimeScopeId, Vec<pi_plugin_mcp::WeakMcpGenerationReadiness>>,
        >,
    >,
}

impl McpSessionCache {
    pub(crate) fn owner(
        &self,
        scope: pi_session::LiveRuntimeScopeId,
    ) -> pi_plugin_mcp::McpSupervisor {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|_, generations| {
            generations.retain(|generation| !generation.is_retired());
            !generations.is_empty()
        });
        entries
            .get(&scope)
            .and_then(|generations| {
                generations
                    .iter()
                    .find_map(|generation| generation.active_supervisor())
            })
            .unwrap_or_default()
    }

    pub(crate) fn stage(
        &self,
        scope: pi_session::LiveRuntimeScopeId,
        generation: pi_plugin_mcp::WeakMcpGenerationReadiness,
    ) {
        // Do not replace the current weak lease with an uncommitted candidate. If preparation
        // fails later, the prior published owner is still available for the next reload.
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(scope)
            .or_default()
            .push(generation);
    }
}

#[derive(PartialEq, Eq)]
struct CredentialRevision {
    oauth: [u8; 32],
    providers: BTreeMap<String, [u8; 32]>,
}

/// Credential baselines are captured before preparing any transports. The sole strong MCP
/// generation lease is moved into the existing non-executable freshness object, not a plugin.
pub(crate) struct McpCredentialSnapshot {
    agent_dir: PathBuf,
    watch: pi_plugin_mcp::McpCredentialWatch,
    baseline: CredentialRevision,
}

impl McpCredentialSnapshot {
    pub(crate) fn capture(agent_dir: &Path, watch: pi_plugin_mcp::McpCredentialWatch) -> Self {
        let baseline = credential_revision(agent_dir, &watch);
        Self {
            agent_dir: agent_dir.to_owned(),
            watch,
            baseline,
        }
    }

    pub(crate) fn provider_revisions(&self) -> BTreeMap<String, [u8; 32]> {
        self.baseline.providers.clone()
    }

    pub(crate) fn with_readiness(
        self,
        readiness: pi_plugin_mcp::McpGenerationReadiness,
    ) -> Arc<dyn pi_session::GenerationFreshness> {
        Arc::new(move || {
            Ok(readiness.changed()
                || credential_revision(&self.agent_dir, &self.watch) != self.baseline)
        })
    }
}

fn credential_revision(
    agent_dir: &Path,
    watch: &pi_plugin_mcp::McpCredentialWatch,
) -> CredentialRevision {
    CredentialRevision {
        oauth: opaque_revision(watch.oauth_revision()),
        providers: watch
            .provider_names()
            .iter()
            .map(|provider| {
                (
                    provider.clone(),
                    opaque_revision(crate::read_stored_credential(agent_dir, provider)),
                )
            })
            .collect(),
    }
}

fn opaque_revision<T: serde::Serialize, E>(value: Result<T, E>) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    // An unreadable store is a distinct revision, not an admission failure for healthy MCP
    // siblings. Its actual auth error is reported by the affected server's connection attempt.
    let bytes = match value {
        Ok(value) => serde_json::to_vec(&(true, value)),
        Err(_) => serde_json::to_vec(&(false, ())),
    }
    .unwrap_or_else(|_| b"unavailable credential revision".to_vec());
    Sha256::digest(bytes).into()
}

struct CodingProviderTokens(PathBuf);

#[async_trait::async_trait]
impl McpProviderTokenResolver for CodingProviderTokens {
    async fn token(&self, provider: &str) -> Result<Option<String>, String> {
        let source = StoredCredentialSource::builtin_with_precedence(
            &self.0,
            provider,
            None,
            CredentialPrecedence::StoredFirst,
        );
        let (_, signal) = pi_core::AbortHandle::new();
        source
            .resolve(&signal)
            .await
            .map(|credential| {
                credential
                    .map(|credential| credential.secret)
                    .filter(|secret| !secret.trim().is_empty())
            })
            // Provider errors may contain endpoints or credential-store context.
            // Only the stable category crosses the MCP presentation boundary.
            .map_err(|_| "Provider credentials could not be resolved; use pi auth login".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StoredCredential, modify_credentials};
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn provider_tokens_reread_external_login_and_logout_without_a_generation() {
        let directory = tempfile::tempdir().unwrap();
        let resolver = CodingProviderTokens(directory.path().into());
        assert_eq!(resolver.token("fixture-provider").await.unwrap(), None);
        for key in ["first-fixture-key", "replacement-fixture-key"] {
            modify_credentials(directory.path(), |credentials| {
                credentials.insert(
                    "fixture-provider".into(),
                    StoredCredential::ApiKey {
                        key: Some(key.into()),
                        env: BTreeMap::new(),
                        extra: BTreeMap::new(),
                    },
                );
            })
            .unwrap();
            assert_eq!(
                resolver.token("fixture-provider").await.unwrap().as_deref(),
                Some(key)
            );
        }
        modify_credentials(directory.path(), |credentials| credentials.clear()).unwrap();
        assert_eq!(resolver.token("fixture-provider").await.unwrap(), None);
    }

    #[test]
    fn credential_revisions_ignore_unrelated_names_and_distinguish_unreadable_stores() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("mcp.json"), serde_json::json!({
            "mcpServers":{"server":{"url":"https://example.invalid/mcp", "auth":{"provider":"watched"}}}
        }).to_string()).unwrap();
        let watch = pi_plugin_mcp::McpLibrary::new(directory.path(), None, false)
            .supervised_credential_watch()
            .unwrap();
        let initial =
            McpCredentialSnapshot::capture(directory.path(), watch.clone()).provider_revisions();
        let key = |value: &str| StoredCredential::ApiKey {
            key: Some(value.into()),
            env: BTreeMap::new(),
            extra: BTreeMap::new(),
        };
        modify_credentials(directory.path(), |credentials| {
            credentials.insert("unrelated".into(), key("unrelated-fixture"));
        })
        .unwrap();
        assert!(credential_revision(directory.path(), &watch).providers == initial);
        modify_credentials(directory.path(), |credentials| {
            credentials.insert("watched".into(), key("watched-fixture"));
        })
        .unwrap();
        let signed_in = credential_revision(directory.path(), &watch).providers;
        assert!(signed_in != initial);
        std::fs::write(directory.path().join("auth.json"), "not JSON").unwrap();
        let unavailable =
            McpCredentialSnapshot::capture(directory.path(), watch.clone()).provider_revisions();
        assert!(unavailable != initial && unavailable != signed_in);
        std::fs::write(directory.path().join("auth.json"), "{}").unwrap();
        assert!(credential_revision(directory.path(), &watch).providers == initial);
        assert!(!directory.path().join("mcp-auth.json").exists());
    }

    #[tokio::test]
    async fn expired_oauth_uses_shared_refresh_policy_and_redacts_failure() {
        let directory = tempfile::tempdir().unwrap();
        modify_credentials(directory.path(), |credentials| {
            credentials.insert(
                "fixture-provider".into(),
                StoredCredential::Oauth {
                    access: "must-not-return-expired-token".into(),
                    refresh: "must-not-log-refresh-token".into(),
                    expires: 0.0,
                    extra: BTreeMap::new(),
                },
            );
        })
        .unwrap();
        let resolver = CodingProviderTokens(directory.path().into());
        assert_eq!(
            resolver.token("fixture-provider").await.unwrap_err(),
            "Provider credentials could not be resolved; use pi auth login"
        );
    }
}
