//! Coding's credential adapter for MCP. Configuration/trust and transport policy stay in MCP.

use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// Observe external sign-in, replacement and sign-out for this generation's
/// configured identities. Capture before connecting, not after a pending probe.
pub(crate) fn generation_freshness(
    agent_dir: &Path,
    watch: pi_plugin_mcp::McpCredentialWatch,
) -> Result<Option<Arc<dyn pi_session::GenerationFreshness>>, pi_session::SessionError> {
    if watch.is_empty() {
        return Ok(None);
    }
    let agent_dir = agent_dir.to_owned();
    let revision = move || -> Result<([u8; 32], [u8; 32]), pi_session::SessionError> {
        use sha2::{Digest, Sha256};
        let snapshot = || -> Result<_, String> {
            let oauth_revision = watch.oauth_revision()?;
            let credentials = watch
                .provider_names()
                .iter()
                .map(|provider| {
                    crate::read_stored_credential(&agent_dir, provider)
                        .map(|credential| (provider, credential))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let bytes =
                serde_json::to_vec(&credentials).map_err(|_| "credential revision failed")?;
            Ok((oauth_revision, Sha256::digest(bytes).into()))
        };
        snapshot().map_err(|_| {
            pi_session::SessionError::Runtime("MCP credential state could not be read".into())
        })
    };
    let baseline = revision()?;
    Ok(Some(Arc::new(move || Ok(revision()? != baseline))))
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
