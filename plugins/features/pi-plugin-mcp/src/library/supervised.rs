//! Concurrent transport preparation with bounded direct-tool startup and frozen publication.
use super::*;
use crate::client::McpConnectionOptions;
use crate::supervisor::{ConnectionInput, ConnectionReadiness, McpSupervisor};
use std::time::Duration;

/// Immutable readiness at generation preparation, not a mutable executable registry.
#[derive(Clone, Debug)]
pub enum McpServerReadiness {
    Pending,
    Ready,
    /// The connection and last valid tools remain usable after discovery refresh failed.
    ReadyWithRefreshError(Arc<crate::McpError>),
    NeedsAuth(crate::McpAuthorizationRequired),
    Failed(Arc<crate::McpError>),
    Disconnected,
    Disabled,
}

impl McpServerReadiness {
    pub fn state(&self) -> McpServerState {
        match self {
            Self::Pending => McpServerState::Pending,
            Self::Ready | Self::ReadyWithRefreshError(_) => McpServerState::Connected,
            Self::NeedsAuth(_) => McpServerState::NeedsAuth,
            Self::Failed(_) => McpServerState::Failed,
            Self::Disconnected => McpServerState::Disconnected,
            Self::Disabled => McpServerState::Disabled,
        }
    }

    pub fn diagnostic(&self) -> Option<String> {
        match self {
            Self::NeedsAuth(required) => Some(required.to_string()),
            Self::Failed(error) | Self::ReadyWithRefreshError(error) => Some(error.to_string()),
            Self::Disconnected => Some("Connection closed".into()),
            _ => None,
        }
    }
}

impl McpLibrary {
    /// Supervised freshness does not let one unresolved URL hide healthy siblings.
    /// Failed identities are still diagnosed by `prepare_supervised_generation`.
    pub fn supervised_credential_watch(&self) -> Result<crate::McpCredentialWatch, String> {
        let configs = self
            .entries()?
            .into_iter()
            .filter(|entry| entry.config.enabled())
            .filter_map(|entry| self.resolve_identity(entry).ok())
            .collect();
        Ok(crate::McpCredentialWatch::new(
            self.client_auth.oauth_store.clone(),
            configs,
        ))
    }

    /// Opts into session-supervised transport reuse. Every eligible start is concurrent;
    /// only configured direct exposure waits, within one ten-second total budget.
    /// The returned plugin is frozen; the host owns publication and idle refreshes.
    /// `provider_revisions` is keyed by provider ID, not MCP server name. Missing IDs
    /// use a zero revision. Static Authorization headers ignore this map entirely.
    /// Move the returned readiness lease into the host's non-executable freshness source;
    /// do not discard it while retaining the plugin's executable views.
    pub async fn prepare_supervised_generation(
        &self,
        supervisor: McpSupervisor,
        provider_revisions: BTreeMap<String, [u8; 32]>,
    ) -> Result<PreparedMcpGeneration, String> {
        self.prepare_supervised_with_budget(supervisor, provider_revisions, Duration::from_secs(10))
            .await
    }

    async fn prepare_supervised_with_budget(
        &self,
        supervisor: McpSupervisor,
        provider_revisions: BTreeMap<String, [u8; 32]>,
        budget: Duration,
    ) -> Result<PreparedMcpGeneration, String> {
        let candidate = supervisor.begin_candidate();
        let (entries, auto_enable_codemode) = self.configuration_replacing(None)?;
        // Check all identities, including entries whose later resolution will fail.
        crate::validate_configs(
            &entries
                .iter()
                .filter(|entry| entry.config.enabled())
                .map(|entry| McpServerConfig::stdio(entry.config.name(), "identity-only"))
                .collect::<Vec<_>>(),
        )
        .map_err(|error| error.to_string())?;
        let rows = entries.iter().map(Entry::row).collect();
        let mut policies = BTreeMap::new();
        let mut inputs = Vec::new();
        let mut server_readiness = BTreeMap::new();
        let mut direct = Vec::new();
        for entry in entries {
            let name = entry.config.name().to_owned();
            if !entry.config.enabled() {
                server_readiness.insert(name, McpServerReadiness::Disabled);
                continue;
            }
            let mut policy = entry.config.policy().clone();
            policy.root = Some(self.cwd.as_ref().unwrap_or(&self.agent_dir).clone());
            policy.log_path = Some(self.agent_dir.join("mcp.log"));
            if policy.exposure == McpExposure::Direct
                || policy
                    .tool_exposure
                    .iter()
                    .any(|(_, exposure)| *exposure == McpExposure::Direct)
            {
                direct.push(name.clone());
            }
            match self.supervised_input(entry, &policy, &provider_revisions) {
                Ok(input) => inputs.push(input),
                Err(error) => {
                    server_readiness
                        .insert(name.clone(), McpServerReadiness::Failed(Arc::new(error)));
                }
            }
            policies.insert(name, policy);
        }
        supervisor
            .reconcile(candidate, inputs)
            .map_err(|error| error.to_string())?;
        // No await at all for indirect-only configuration. Transports continue independently.
        let snapshot = if direct.is_empty() {
            supervisor.snapshot()
        } else {
            supervisor.wait_for(&direct, budget).await
        };
        supervisor
            .ensure_current(candidate)
            .map_err(|error| error.to_string())?;
        let watch = crate::McpGenerationReadiness::new(supervisor, &snapshot);
        let mut connections = Vec::new();
        let mut instructions = BTreeMap::new();
        let mut authorization_required = Vec::new();
        for (name, readiness) in snapshot.servers {
            let state = match readiness {
                ConnectionReadiness::Ready(inventory) => {
                    let policy = policies.get(&name).ok_or("MCP candidate was superseded")?;
                    instructions.insert(
                        name.clone(),
                        inventory
                            .connection
                            .metadata()
                            .and_then(|info| info.instructions.clone()),
                    );
                    connections.push((
                        inventory.connection.clone(),
                        inventory.tools.clone(),
                        policy.clone(),
                    ));
                    match &inventory.refresh_error {
                        Some(error) => McpServerReadiness::ReadyWithRefreshError(error.clone()),
                        None => McpServerReadiness::Ready,
                    }
                }
                ConnectionReadiness::Connecting => McpServerReadiness::Pending,
                ConnectionReadiness::Disconnected(_) | ConnectionReadiness::Retired => {
                    McpServerReadiness::Disconnected
                }
                ConnectionReadiness::Failed(error) => match error.as_ref() {
                    crate::McpError::AuthorizationRequired(required) => {
                        authorization_required.push(required.clone());
                        McpServerReadiness::NeedsAuth(required.clone())
                    }
                    _ => McpServerReadiness::Failed(error),
                },
            };
            server_readiness.insert(name, state);
        }
        // Projection failures are fatal but never close transports retained by the supervisor.
        let pool = McpToolSet::from_connections(connections, authorization_required.clone())
            .map_err(|error| error.to_string())?;
        let mut library = self.clone();
        library.generation = Some(watch.downgrade());
        library.server_readiness = server_readiness.clone();
        library.server_directory =
            render_supervised_directory(&policies, &server_readiness, &instructions);
        library.pending_direct = direct
            .into_iter()
            .filter(|name| {
                matches!(
                    server_readiness.get(name),
                    Some(McpServerReadiness::Pending)
                )
            })
            .collect();
        let selection =
            library.prepare_tool_selection(&policies, &server_readiness, &pool.tool_names());
        // Filter only final qualified identities: filtering raw inventory first
        // would rename collision peers and could activate a different tool.
        let pool = pool.without_tools(&selection.quarantined);
        let needs_codemode = policies.values().any(McpServerPolicy::uses_codemode);
        let needs_tool_search = policies.values().any(McpServerPolicy::uses_deferred);
        Ok(PreparedMcpGeneration {
            temporarily_unavailable_tools: selection.allowed,
            unresolved_tool_selection: selection.unresolved,
            readiness_watch: Some(watch),
            server_readiness,
            authorization_required,
            plugin: Arc::new(McpProductPlugin {
                library,
                rows,
                pool,
                auto_enable_codemode,
                needs_codemode,
                needs_tool_search,
            }),
        })
    }

    fn supervised_input(
        &self,
        entry: Entry,
        policy: &McpServerPolicy,
        provider_revisions: &BTreeMap<String, [u8; 32]>,
    ) -> Result<ConnectionInput, crate::McpError> {
        let name = entry.config.name().to_owned();
        let failed = |message| crate::McpError::Start {
            server: name.clone(),
            message,
        };
        let source = self.path(entry.scope).map_err(&failed)?;
        let auth_revision = match entry.config.transport() {
            Some(McpTransport::Http { headers, .. })
                if !headers
                    .keys()
                    .any(|key| key.eq_ignore_ascii_case("authorization")) =>
            {
                if let Some(provider) = entry.config.auth_provider() {
                    provider_revisions.get(provider).copied().unwrap_or([0; 32])
                } else {
                    let identity = self.resolve_identity(entry.clone()).map_err(&failed)?;
                    crate::McpCredentialWatch::new(
                        self.client_auth.oauth_store.clone(),
                        vec![identity],
                    )
                    .oauth_revision()
                    .map_err(&failed)?
                }
            }
            _ => [0; 32],
        };
        let isolation = serde_json::to_vec(&(self.trusted, &policy.root, source, auth_revision))
            .map_err(|_| failed("connection isolation fingerprint could not be created".into()))?;
        ConnectionInput::local(
            entry.config.into_connection_definition().map_err(failed)?,
            self.local_values(),
            McpConnectionOptions::from(policy),
            self.connection_auth(),
            Sha256::digest(isolation).into(),
        )
    }

    pub(crate) fn active_supervisor(&self) -> Option<McpSupervisor> {
        self.generation.as_ref()?.active_supervisor()
    }

    pub(crate) fn reconnect_active(&self, name: &str) -> Option<Result<(), String>> {
        let generation = self.generation.as_ref()?;
        Some(match generation.active_supervisor() {
            Some(supervisor) => supervisor
                .reconnect(name)
                .map_err(|error| error.to_string()),
            None => Err("MCP generation is not active; reload before reconnecting".into()),
        })
    }

    pub(super) fn invalidate_active(&self, name: &str) {
        if let Some(supervisor) = self.active_supervisor() {
            supervisor.invalidate(name);
        }
    }

    pub(crate) fn publish_generation(&self) {
        if let Some(generation) = &self.generation {
            generation.publish();
        }
    }

    pub(crate) async fn shutdown_generation(&self, reloading: bool) -> Result<(), crate::McpError> {
        if let Some(generation) = &self.generation {
            generation.shutdown(reloading).await?;
        }
        Ok(())
    }

    pub(crate) fn supervised_directory(&self) -> Option<&str> {
        self.server_directory.as_deref()
    }

    pub(crate) fn pending_direct_servers(&self) -> &[String] {
        &self.pending_direct
    }

    pub(crate) fn snapshot_readiness(&self, name: &str) -> Option<&McpServerReadiness> {
        self.server_readiness.get(name)
    }
}

fn render_supervised_directory(
    policies: &BTreeMap<String, McpServerPolicy>,
    readiness: &BTreeMap<String, McpServerReadiness>,
    instructions: &BTreeMap<String, Option<String>>,
) -> Option<String> {
    let summaries = policies.iter()
        .filter(|(_, policy)| policy.uses_codemode() || policy.uses_deferred())
        .map(|(name, policy)| {
            let description = policy.description.as_deref().map(str::trim).filter(|value| !value.is_empty())
                .or_else(|| instructions.get(name).and_then(|value| value.as_deref())).unwrap_or_default()
                .lines().next().unwrap_or_default().trim();
            // Status first: a long configured description must not hide readiness. Raw errors
            // and auth challenges never enter the model-facing directory.
            let status = match readiness.get(name) {
                Some(McpServerReadiness::Ready) => String::new(),
                Some(McpServerReadiness::ReadyWithRefreshError(_)) => "Tool refresh failed; last discovered tools remain available. Inspect /mcp status or retry /mcp reconnect. ".into(),
                Some(McpServerReadiness::NeedsAuth(required)) => format!("Needs sign-in; {}. ", required.guidance()),
                Some(McpServerReadiness::Failed(_)) => "Connection failed; inspect /mcp status and retry /mcp reconnect. ".into(),
                Some(McpServerReadiness::Pending) => "Pending; tools unavailable in this generation. Recheck after connection completes and the next idle refresh. ".into(),
                _ => "Disconnected; reconnect before using these tools. ".into(),
            };
            (name.as_str(), format!("{status}{description}"), if policy.uses_codemode() { "codemode" } else { "tool_search" })
        }).collect::<Vec<_>>();
    crate::directory::render_servers_section(
        summaries
            .iter()
            .map(|(name, summary, reach)| (*name, Some(summary.as_str()), None, *reach)),
    )
}

#[cfg(test)]
mod tests;
