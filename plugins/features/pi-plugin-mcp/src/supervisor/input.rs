//! Transport-only attempt input. Local value resolution is inside the supervised lifetime.
use std::collections::BTreeMap;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::client::{McpConnection, McpConnectionOptions, connect_inventory};
use crate::config::{LocalValues, McpConfigContext, McpConnectionDefinition};
use crate::{McpClientAuth, McpError, McpServerConfig};
use std::sync::Arc;

/// Trust scope and effective auth source are opaque to the transport supervisor.
#[derive(Clone, PartialEq, Eq)]
struct ReuseKey {
    config: [u8; 32],
    options: McpConnectionOptions,
    isolation: [u8; 32],
}

#[derive(Clone)]
enum ConnectionSource {
    #[cfg(test)]
    Explicit(McpServerConfig),
    Local {
        definition: McpConnectionDefinition,
        values: LocalValues,
    },
}

#[derive(Clone)]
pub(crate) struct ConnectionInput {
    source: ConnectionSource,
    options: McpConnectionOptions,
    auth: McpClientAuth,
    key: ReuseKey,
}

impl ConnectionInput {
    // Transport-only callers still use their existing client API; this constructor exercises
    // the supervisor independently of local-library policy at its owning test seam.
    #[cfg(test)]
    pub(crate) fn new(
        mut config: McpServerConfig,
        options: McpConnectionOptions,
        auth: McpClientAuth,
        isolation: [u8; 32],
    ) -> Result<Self, McpError> {
        crate::validate_configs(std::slice::from_ref(&config))?;
        if let crate::McpTransport::Http { url, .. } = &mut config.transport {
            *url = reqwest::Url::parse(url)
                .map_err(|_| McpError::InvalidHttp {
                    server: config.name.clone(),
                    message: "invalid URL",
                })?
                .to_string();
        }
        let bytes = serde_json::to_vec(&config).map_err(|_| fingerprint_error(&config.name))?;
        Ok(Self {
            source: ConnectionSource::Explicit(config),
            key: ReuseKey {
                config: Sha256::digest(bytes).into(),
                options: options.clone(),
                isolation,
            },
            options,
            auth,
        })
    }

    pub(crate) fn local(
        definition: McpConnectionDefinition,
        values: LocalValues,
        options: McpConnectionOptions,
        auth: McpClientAuth,
        isolation: [u8; 32],
    ) -> Result<Self, McpError> {
        // No shell commands or secret-field expansion when constructing/reusing an attempt.
        // URL/path expansion and validation happen inside that attempt's single lifetime.
        let bytes = serde_json::to_vec(&(&definition.config, &values.cwd, values.allow_commands))
            .map_err(|_| fingerprint_error(&definition.config.name))?;
        let mut digest = Sha256::new();
        digest.update(bytes);
        // Include inherited environment as well as explicit references. Keep only a digest;
        // equality must not accidentally reuse a child launched with a different environment.
        for (key, value) in std::env::vars_os().collect::<BTreeMap<_, _>>() {
            for field in [key, value] {
                let bytes = field.as_encoded_bytes();
                digest.update((bytes.len() as u64).to_le_bytes());
                digest.update(bytes);
            }
        }
        Ok(Self {
            source: ConnectionSource::Local { definition, values },
            key: ReuseKey {
                config: digest.finalize().into(),
                options: options.clone(),
                isolation,
            },
            options,
            auth,
        })
    }

    fn config(&self) -> &McpServerConfig {
        match &self.source {
            #[cfg(test)]
            ConnectionSource::Explicit(config) => config,
            ConnectionSource::Local { definition, .. } => &definition.config,
        }
    }

    pub(super) fn equivalent(&self, other: &Self) -> bool {
        self.key == other.key && self.auth.same_source(&other.auth, self.config())
    }

    pub(super) fn name(&self) -> &str {
        &self.config().name
    }

    pub(super) fn identity(&self) -> McpServerConfig {
        // Reconciliation validates only global name/namespace invariants before doing any I/O.
        // Invalid per-server transport/value resolution must not discard healthy siblings.
        McpServerConfig::stdio(self.name(), "identity-only")
    }

    pub(super) async fn connect(
        self,
        signal: pi_core::AbortSignal,
    ) -> Result<(Arc<McpConnection>, Vec<rmcp::model::Tool>), McpError> {
        let name = self.name().to_owned();
        let timeout_name = name.clone();
        tokio::time::timeout(Duration::from_secs(30), async move {
            let config = match self.source {
                #[cfg(test)]
                ConnectionSource::Explicit(config) => config,
                ConnectionSource::Local { definition, values } => definition
                    .resolve_for_connect(
                        McpConfigContext {
                            config_dir: &values.cwd,
                            default_cwd: &values.cwd,
                        },
                        values.allow_commands,
                        signal,
                    )
                    .await
                    .map_err(|message| McpError::Start {
                        server: name,
                        message,
                    })?,
            };
            // Parsing validates placeholders, not the eventual URL/header values.
            // Validate the resolved transport before any authentication or network work.
            crate::validate_configs(std::slice::from_ref(&config))?;
            connect_inventory(config, self.options, self.auth).await
        })
        .await
        .map_err(|_| McpError::Timeout(timeout_name))?
    }
}

#[cfg(test)]
mod tests;

fn fingerprint_error(server: &str) -> McpError {
    McpError::Initialize {
        server: server.into(),
        message: "configuration fingerprint could not be created".into(),
    }
}
