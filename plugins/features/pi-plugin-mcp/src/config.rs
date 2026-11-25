//! MCP document parsing and connection-time transport resolution.
//!
//! Parsing validates static transport fields without reading environment variables,
//! opening files or connecting. Callers own file discovery, scope precedence, trust
//! and persistence; only selected entries are resolved immediately before connecting.

mod values;
pub(crate) use values::LocalValues;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde::de::{MapAccess, Visitor};

use serde_json::{Value, json};

use crate::policy::{McpExposure, McpServerPolicy};
use crate::{
    McpOAuthClientRegistration, McpOAuthConfig, McpProviderAuth, McpServerConfig, McpTransport,
};

/// Maximum supported MCP document size, also used to bound callers' disk reads.
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// A validated document entry whose environment references are still unresolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpConfigEntry {
    name: String,
    transport: Option<McpTransport>,
    enabled: bool,
    policy: McpServerPolicy,
    oauth: Option<McpOAuthConfig>,
    auth: Option<McpProviderAuth>,
    policy_override: Option<McpPolicyOverride>,
}

/// Unresolved transport/auth data selected after the caller's policy and trust checks.
/// Safe to retain across generation preparation: no generation-owned policy or code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct McpConnectionDefinition {
    pub(crate) config: McpServerConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct McpPolicyOverride {
    unexpected_fields: bool,
    enabled: Option<bool>,
    exposure: Option<McpExposure>,
    tool_exposure: Option<Vec<(String, McpExposure)>>,
}

/// Paths selected by the caller after resolving configuration scope and trust.
#[derive(Clone, Copy, Debug)]
pub struct McpConfigContext<'a> {
    /// Configuration provenance. Kept for source compatibility; paths now use `default_cwd`.
    pub config_dir: &'a Path,
    /// Session working directory; also the base for explicitly configured relative stdio paths.
    pub default_cwd: &'a Path,
}

impl McpConfigEntry {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn transport(&self) -> Option<&McpTransport> {
        self.transport.as_ref()
    }

    pub(crate) fn policy(&self) -> &McpServerPolicy {
        &self.policy
    }

    pub(crate) fn is_override(&self) -> bool {
        self.policy_override.is_some()
    }

    pub(crate) fn has_provider_auth(&self) -> bool {
        self.auth.is_some()
    }

    pub(crate) fn auth_provider(&self) -> Option<&str> {
        self.auth.as_ref().map(|auth| auth.provider.as_str())
    }

    pub(crate) fn supports_oauth(&self) -> bool {
        self.auth.is_none()
            && matches!(&self.transport, Some(McpTransport::Http { headers, .. })
            if !headers.keys().any(|key| key.eq_ignore_ascii_case("authorization")))
    }

    pub(crate) fn apply_override(self, base: &Self) -> Result<Self, String> {
        let patch = self.policy_override.ok_or("Expected MCP policy override")?;
        if patch.unexpected_fields {
            return Err(format!(
                "MCP server {}: a project policy override can only set enabled, exposure and toolExposure",
                self.name
            ));
        }
        if base.transport.is_none() {
            return Err(format!(
                "MCP server {}: override needs a global transport",
                self.name
            ));
        }
        let mut merged = base.clone();
        if let Some(enabled) = patch.enabled {
            merged.enabled = enabled;
        }
        if let Some(exposure) = patch.exposure {
            merged.policy.exposure = exposure;
        }
        if let Some(tool_exposure) = patch.tool_exposure {
            merged.policy.tool_exposure = tool_exposure;
        }
        Ok(merged)
    }

    /// Expands environment references and stdio paths without starting a transport.
    /// Callers select enabled entries; a disabled entry may have no transport.
    pub fn resolve(self, context: McpConfigContext<'_>) -> Result<McpServerConfig, String> {
        if self.policy != McpServerPolicy::direct() {
            return Err(format!(
                "MCP server {}: resolve returns transport only; use McpLibrary::prepare to preserve MCP policy",
                self.name
            ));
        }
        self.resolve_transport(context)
    }

    /// Extracts transport/auth data without expanding values or executing helpers.
    /// The caller remains responsible for checking enabled state and preserving policy.
    pub(crate) fn into_connection_definition(self) -> Result<McpConnectionDefinition, String> {
        Ok(McpConnectionDefinition {
            config: McpServerConfig {
                name: self.name,
                transport: self
                    .transport
                    .ok_or("Enabled MCP server needs a transport")?,
                oauth: self.oauth,
                auth: self.auth,
            },
        })
    }

    pub(crate) fn resolve_transport(
        self,
        context: McpConfigContext<'_>,
    ) -> Result<McpServerConfig, String> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        self.resolve_with(context, home.as_deref(), |key| {
            std::env::var(key)
                .map_err(|_| format!("MCP environment variable {key} is not set or is not UTF-8"))
        })
    }

    /// Only identities are needed for credential watches/logout. Never resolve secret fields.
    pub(crate) fn resolve_identity(self) -> Result<McpServerConfig, String> {
        let mut config = self.into_connection_definition()?.config;
        if let McpTransport::Http { url, .. } = &mut config.transport {
            *url = expand_with(url, |key| {
                std::env::var(key)
                    .map_err(|_| "MCP URL environment reference is unavailable".into())
            })?;
        }
        Ok(config)
    }

    pub(crate) async fn resolve_for_connect(
        self,
        context: McpConfigContext<'_>,
        allow_commands: bool,
        signal: pi_core::AbortSignal,
    ) -> Result<McpServerConfig, String> {
        if !self.enabled {
            return Err("Enable the MCP server before connecting or signing in".into());
        }
        self.into_connection_definition()?
            .resolve_for_connect(context, allow_commands, signal)
            .await
    }

    fn resolve_with(
        self,
        context: McpConfigContext<'_>,
        home: Option<&Path>,
        lookup: impl Fn(&str) -> Result<String, String>,
    ) -> Result<McpServerConfig, String> {
        self.into_connection_definition()?
            .resolve_with(context, home, lookup)
    }
}

impl McpConnectionDefinition {
    /// Resolves at connection time, within the caller's bounded/cancellable attempt.
    /// OAuth secrets remain late-bound by the auth owner, not transport setup.
    pub(crate) async fn resolve_for_connect(
        self,
        context: McpConfigContext<'_>,
        allow_commands: bool,
        signal: pi_core::AbortSignal,
    ) -> Result<McpServerConfig, String> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let lookup = |key: &str| {
            std::env::var(key)
                .map_err(|_| "MCP transport environment reference is unavailable".into())
        };
        // Resolve only transport fields here; values must be resolved exactly once below.
        let mut config =
            self.resolve_fields_with(context, home.as_deref(), &lookup, |value| Ok(value.into()))?;
        let fields = match &mut config.transport {
            McpTransport::Stdio { env, .. } => env,
            McpTransport::Http { headers, .. } => headers,
        };
        for value in fields.values_mut() {
            *value =
                values::resolve(value, context.default_cwd, allow_commands, signal.clone()).await?;
        }
        // OAuth settings remain unresolved: a usable token must not need its client secret.
        // Local auth resolves it only for an actual sign-in or refresh operation.
        Ok(config)
    }

    fn resolve_with(
        self,
        context: McpConfigContext<'_>,
        home: Option<&Path>,
        lookup: impl Fn(&str) -> Result<String, String>,
    ) -> Result<McpServerConfig, String> {
        self.resolve_fields_with(context, home, &lookup, |value| {
            values::expand(value, &lookup)
        })
    }

    fn resolve_fields_with(
        self,
        context: McpConfigContext<'_>,
        home: Option<&Path>,
        lookup: &impl Fn(&str) -> Result<String, String>,
        value: impl Fn(&str) -> Result<String, String>,
    ) -> Result<McpServerConfig, String> {
        let McpServerConfig {
            name,
            mut transport,
            mut oauth,
            auth,
        } = self.config;
        let use_oauth = auth.is_none()
            && matches!(&transport, McpTransport::Http { headers, .. } if !headers.keys().any(|name| name.eq_ignore_ascii_case("authorization")));
        if use_oauth
            && let Some(secret) = oauth
                .as_mut()
                .and_then(|oauth| oauth.client_secret.as_mut())
        {
            *secret = value(secret)?;
        }
        match &mut transport {
            McpTransport::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                *command = expand_tilde_path(&expand_with(command, lookup)?, home)
                    .to_string_lossy()
                    .into_owned();
                for arg in args {
                    *arg = expand_tilde_path(&expand_with(arg, lookup)?, home)
                        .to_string_lossy()
                        .into_owned();
                }
                for field in env.values_mut() {
                    *field = value(field)?;
                }
                *cwd = Some(match cwd.take() {
                    Some(path) => {
                        let path =
                            expand_tilde_path(&expand_with(&path.to_string_lossy(), lookup)?, home);
                        if path.is_absolute() {
                            path
                        } else {
                            context.default_cwd.join(path)
                        }
                    }
                    None => context.default_cwd.to_path_buf(),
                });
            }
            McpTransport::Http { url, headers } => {
                *url = expand_with(url, lookup)?;
                for field in headers.values_mut() {
                    *field = value(field)?;
                }
            }
        }
        Ok(McpServerConfig {
            name,
            transport,
            oauth,
            auth,
        })
    }
}

/// Parses an `mcpServers` document and unresolved policy-only overrides.
/// The version-1 marker and the empty `mcpServers` object are optional.
/// Scope owners must resolve project overrides against global definitions before connecting.
/// Use [`parse_document`] for documents containing `autoEnableCodemode`; this
/// entry-only interface rejects document policy rather than silently discarding it.
pub fn parse_config(content: &str) -> Result<Vec<McpConfigEntry>, String> {
    let document = parse_document(content)?;
    if document.auto_enable_codemode.is_some() {
        return Err("mcp.json: parse_config returns entries only; use parse_document or McpLibrary to preserve autoEnableCodemode".into());
    }
    Ok(document.servers)
}

/// Validated document policy and unresolved server entries. Project scope merging
/// belongs to the host or McpLibrary; an omitted flag inherits the lower scope.
#[derive(Debug)]
pub struct ParsedMcpConfig {
    pub servers: Vec<McpConfigEntry>,
    pub auto_enable_codemode: Option<bool>,
}

/// Parses both document policy and server entries without reading files or environment.
pub fn parse_document(content: &str) -> Result<ParsedMcpConfig, String> {
    if content.len() as u64 > MAX_CONFIG_BYTES {
        return Err("mcp.json exceeds 1 MiB".into());
    }
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let syntax_error = |e: serde_json::Error| {
        format!(
            "Invalid mcp.json syntax at line {}, column {}",
            e.line(),
            e.column()
        )
    };
    let value: Value = serde_json::from_str(content).map_err(syntax_error)?;
    let object = value.as_object().ok_or("mcp.json must be an object")?;
    if object.get("version").is_some_and(|v| v != 1) {
        return Err("Unsupported mcp.json version (expected 1)".into());
    }
    let auto_enable_codemode = object
        .get("autoEnableCodemode")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| "mcp.json: autoEnableCodemode must be a boolean".to_string())
        })
        .transpose()?;
    let Some(servers) = object.get("mcpServers") else {
        return Ok(ParsedMcpConfig {
            servers: Vec::new(),
            auto_enable_codemode,
        });
    };
    let servers = servers
        .as_object()
        .ok_or("mcp.json needs an mcpServers object")?;
    let mut namespaces = BTreeMap::new();
    for (name, value) in servers {
        validate_server_name(name)?;
        let namespace = name.replace('-', "_");
        if let Some(previous) = namespaces.insert(namespace, name) {
            return Err(format!(
                "MCP server {name}: namespace conflicts with {previous}"
            ));
        }
        let config = value
            .as_object()
            .ok_or_else(|| format!("MCP server {name} must be an object"))?;
        if config
            .get("toolExposure")
            .is_some_and(|value| !value.is_object())
        {
            return Err(format!(
                "MCP server {name}: toolExposure must map tool names to exposures"
            ));
        }
    }
    // Value's maps need not preserve order. Read just this ordered policy field separately,
    // without changing serde_json features for every package in the workspace.
    let mut order: ToolExposureOrderDocument =
        serde_json::from_str(content).map_err(syntax_error)?;
    let servers = servers
        .iter()
        .map(|(name, value)| {
            let ordered = order
                .servers
                .remove(name)
                .and_then(|entry| entry.tool_exposure)
                .unwrap_or_default();
            parse_entry(
                name,
                value.as_object().expect("validated server object").clone(),
                ordered.0,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ParsedMcpConfig {
        servers,
        auto_enable_codemode,
    })
}

pub(crate) fn validate_server_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        return Err("MCP server names must use only letters, digits, '_' and '-'".into());
    }
    Ok(())
}

fn parse_entry(
    name: &str,
    mut config: serde_json::Map<String, Value>,
    ordered_exposures: Vec<(String, Value)>,
) -> Result<McpConfigEntry, String> {
    let oauth_value = config.remove("oauth");
    let auth_value = config.remove("auth");
    let has_transport = ["command", "url", "type"]
        .iter()
        .any(|field| config.contains_key(*field));
    let unexpected_override_fields = !has_transport
        && config
            .keys()
            .any(|key| !matches!(key.as_str(), "enabled" | "exposure" | "toolExposure"));
    let enabled = match config.remove("enabled") {
        None => None,
        Some(Value::Bool(value)) => Some(value),
        _ => return Err(format!("MCP server {name}: enabled must be boolean")),
    };
    let exposure = config
        .remove("exposure")
        .map(|value| parse_exposure(name, "exposure", &value))
        .transpose()?;
    let tool_exposure = config
        .remove("toolExposure")
        .map(|_| {
            ordered_exposures
                .into_iter()
                .map(|(pattern, value)| {
                    Ok((pattern, parse_exposure(name, "toolExposure", &value)?))
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()?;
    let description = match config.remove("description") {
        None => None,
        Some(Value::String(value)) => Some(value),
        _ => return Err(format!("MCP server {name}: description must be a string")),
    };
    let timeout = config
        .remove("timeout")
        .map(|value| {
            value
                .as_f64()
                .filter(|seconds| *seconds > 0.0)
                .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
                .filter(|timeout| !timeout.is_zero())
                .filter(|timeout| std::time::Instant::now().checked_add(*timeout).is_some())
                .ok_or_else(|| {
                    format!("MCP server {name}: timeout must be a positive number of seconds")
                })
        })
        .transpose()?
        .unwrap_or(Duration::from_secs(60));
    let policy = McpServerPolicy {
        exposure: exposure.unwrap_or(McpExposure::Codemode),
        tool_exposure: tool_exposure.clone().unwrap_or_default(),
        description,
        timeout,
        root: None,
        log_path: None,
    };
    let policy_override = (!has_transport).then_some(McpPolicyOverride {
        unexpected_fields: unexpected_override_fields,
        enabled,
        exposure,
        tool_exposure,
    });
    let mut oauth = None;
    let mut auth = None;
    let transport = if !has_transport {
        if oauth_value.is_some() || auth_value.is_some() {
            return Err(format!(
                "MCP server {name}: project policy overrides cannot set authentication"
            ));
        }
        None
    } else {
        if config.get("type") == Some(&json!("streamable-http")) {
            config.insert("type".into(), json!("http"));
        }
        if !config.contains_key("type") {
            if config.get("url").is_some_and(Value::is_string) {
                config.insert("type".into(), json!("http"));
            } else if config.get("command").is_some_and(Value::is_string) {
                config.insert("type".into(), json!("stdio"));
            }
        }
        if config.get("type") == Some(&json!("sse")) {
            return Err(format!(
                "MCP server {name}: legacy SSE is not supported; use a Streamable HTTP endpoint"
            ));
        }
        let transport: McpTransport = serde_json::from_value(Value::Object(config)).map_err(|_| format!("MCP server {name}: invalid transport fields (stdio command/args/env/cwd or http url/headers)"))?;
        match &transport {
            McpTransport::Stdio { command, .. } if command.trim().is_empty() => {
                return Err(format!("MCP server {name}: command is empty"));
            }
            McpTransport::Http { url, headers } => {
                oauth = oauth_value
                    .map(|value| parse_oauth(name, value))
                    .transpose()?;
                auth = auth_value
                    .map(|value| parse_provider_auth(name, value))
                    .transpose()?;
                let placeholder = |value: &str| {
                    if value.starts_with('!') {
                        Ok("placeholder".into())
                    } else {
                        values::expand(value, |_| Ok("placeholder".into()))
                    }
                };
                let checked_url = expand_with(url, |_| Ok("placeholder".into()))?;
                let checked_url = if url.starts_with('$') {
                    "https://mcp.invalid".into()
                } else {
                    checked_url
                };
                let mut checked_oauth = oauth.clone();
                if let Some(secret) = checked_oauth
                    .as_mut()
                    .and_then(|oauth| oauth.client_secret.as_mut())
                {
                    *secret = placeholder(secret)?;
                }
                let config = McpServerConfig {
                    name: name.into(),
                    transport: McpTransport::Http {
                        url: checked_url,
                        headers: headers
                            .iter()
                            .map(|(key, value)| Ok((key.clone(), placeholder(value)?)))
                            .collect::<Result<_, String>>()?,
                    },
                    oauth: checked_oauth,
                    auth: auth.clone(),
                };
                crate::validate_configs(&[config]).map_err(|e| e.to_string())?;
            }
            McpTransport::Stdio { .. } => {
                if oauth_value.is_some() || auth_value.is_some() {
                    return Err(format!(
                        "MCP server {name}: oauth and auth are only valid for HTTP servers"
                    ));
                }
            }
        }
        Some(transport)
    };
    Ok(McpConfigEntry {
        name: name.into(),
        transport,
        enabled: enabled.unwrap_or(true),
        policy,
        oauth,
        auth,
        policy_override,
    })
}

fn parse_oauth(name: &str, value: Value) -> Result<McpOAuthConfig, String> {
    let oauth: McpOAuthConfig = serde_json::from_value(value)
        .map_err(|_| format!("MCP server {name}: oauth must use valid OAuth fields"))?;
    if oauth
        .client_id
        .as_ref()
        .is_some_and(|value| value.is_empty())
    {
        return Err(format!(
            "MCP server {name}: oauth.clientId must not be empty"
        ));
    }
    if oauth.callback_port == Some(0) {
        return Err(format!(
            "MCP server {name}: oauth.callbackPort must be a port number"
        ));
    }
    if let Some(callback) = &oauth.callback_url {
        let callback_url = url::Url::parse(callback).ok();
        let valid = callback_url.as_ref().is_some_and(|url| {
            url.scheme() == "http"
                && is_loopback_host(url.host_str())
                && url.query().is_none()
                && url.fragment().is_none()
        });
        if !valid {
            return Err(format!(
                "MCP server {name}: oauth.callbackUrl must be an http URI on localhost, 127.0.0.1, or [::1] without query or fragment"
            ));
        }
        if let (Some(configured), Some(port)) =
            (callback_url.and_then(|url| url.port()), oauth.callback_port)
            && configured != port
        {
            return Err(format!(
                "MCP server {name}: oauth.callbackUrl and oauth.callbackPort name different ports"
            ));
        }
    }
    if oauth
        .client_name
        .as_ref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(format!(
            "MCP server {name}: oauth.clientName must be a non-empty string"
        ));
    }
    if oauth.client_registration == Some(McpOAuthClientRegistration::Cimd) {
        if oauth.client_id.is_some() || oauth.client_name.is_some() {
            return Err(format!(
                "MCP server {name}: oauth.clientRegistration \"cimd\" cannot be combined with oauth.clientId or oauth.clientName"
            ));
        }
        if let Some(callback) = &oauth.callback_url {
            let callback = url::Url::parse(callback).expect("callback validated");
            if !matches!(callback.host_str(), Some("localhost" | "127.0.0.1"))
                || callback.path() != "/callback"
            {
                return Err(format!(
                    "MCP server {name}: oauth.clientRegistration \"cimd\" requires oauth.callbackUrl on localhost or 127.0.0.1 with path /callback"
                ));
            }
        }
    }
    if let Some(metadata) = &oauth.auth_server_metadata_url {
        let metadata = url::Url::parse(metadata).ok();
        let valid = metadata.as_ref().is_some_and(|url| {
            url.scheme() == "https" || (url.scheme() == "http" && is_loopback_host(url.host_str()))
        });
        if !valid {
            return Err(format!(
                "MCP server {name}: oauth.authServerMetadataUrl must be an https URL, or http on localhost, 127.0.0.1, or [::1]"
            ));
        }
    }
    Ok(oauth)
}

fn parse_provider_auth(name: &str, value: Value) -> Result<McpProviderAuth, String> {
    let auth: McpProviderAuth = serde_json::from_value(value)
        .map_err(|_| format!("MCP server {name}: auth.provider must be a provider name"))?;
    if auth.provider.trim().is_empty() {
        return Err(format!(
            "MCP server {name}: auth.provider must be a provider name"
        ));
    }
    Ok(auth)
}

fn is_loopback_host(host: Option<&str>) -> bool {
    matches!(host, Some("localhost" | "127.0.0.1" | "[::1]" | "::1"))
}

fn parse_exposure(name: &str, field: &str, value: &Value) -> Result<McpExposure, String> {
    match value.as_str() {
        Some("direct") => Ok(McpExposure::Direct),
        Some("deferred") => Ok(McpExposure::Deferred),
        Some("hidden") => Ok(McpExposure::Hidden),
        Some("codemode" | "codemode-deferred") => Ok(McpExposure::Codemode),
        _ => Err(format!(
            "MCP server {name}: {field} must be direct, codemode, deferred or hidden"
        )),
    }
}

#[derive(Default, Deserialize)]
struct ToolExposureOrderDocument {
    #[serde(default, rename = "mcpServers")]
    servers: BTreeMap<String, ToolExposureOrderEntry>,
}

#[derive(Deserialize)]
struct ToolExposureOrderEntry {
    #[serde(rename = "toolExposure")]
    tool_exposure: Option<OrderedExposures>,
}

#[derive(Default)]
struct OrderedExposures(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for OrderedExposures {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OrderedVisitor;
        impl<'de> Visitor<'de> for OrderedVisitor {
            type Value = OrderedExposures;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a tool exposure object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries: Vec<(String, Value)> = Vec::new();
                while let Some((name, value)) = map.next_entry::<String, Value>()? {
                    if let Some((_, current)) = entries.iter_mut().find(|(key, _)| *key == name) {
                        *current = value;
                    } else {
                        entries.push((name, value));
                    }
                }
                Ok(OrderedExposures(entries))
            }
        }
        deserializer.deserialize_map(OrderedVisitor)
    }
}

fn expand_with(
    value: &str,
    lookup: impl Fn(&str) -> Result<String, String>,
) -> Result<String, String> {
    let valid = |key: &str| {
        !key.is_empty()
            && key
                .bytes()
                .enumerate()
                .all(|(i, c)| c == b'_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
    };
    if let Some(key) = value.strip_prefix('$').filter(|key| valid(key)) {
        return lookup(key);
    }
    let mut result = String::new();
    let mut rest = value;
    while let Some((before, after)) = rest.split_once("${") {
        result.push_str(before);
        let (key, after) = after
            .split_once('}')
            .ok_or("Unclosed MCP environment reference")?;
        let key = key.strip_prefix("env:").unwrap_or(key);
        if !valid(key) {
            return Err("Invalid MCP environment variable name".into());
        }
        result.push_str(&lookup(key)?);
        rest = after;
    }
    result.push_str(rest);
    Ok(result)
}

fn expand_tilde_path(path: &str, home: Option<&Path>) -> PathBuf {
    if let Some(home) = home {
        if path == "~" {
            return home.to_path_buf();
        }
        if let Some(relative) = path.strip_prefix("~/") {
            return home.join(relative);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod value_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(config: Value) -> McpConfigEntry {
        parse_config(&json!({"mcpServers": {"server": config}}).to_string())
            .unwrap()
            .pop()
            .unwrap()
    }

    struct ConfigPaths {
        config_dir: PathBuf,
        default_cwd: PathBuf,
    }

    impl ConfigPaths {
        fn new() -> Self {
            let root = std::path::absolute(std::env::temp_dir()).unwrap();
            Self {
                config_dir: root.join("pi-plugin-mcp-configuration"),
                default_cwd: root.join("pi-plugin-mcp-session"),
            }
        }

        fn context(&self) -> McpConfigContext<'_> {
            McpConfigContext {
                config_dir: &self.config_dir,
                default_cwd: &self.default_cwd,
            }
        }
    }

    #[test]
    fn connection_definition_keeps_only_unresolved_transport_and_auth() {
        let selected = entry(json!({
            "url": "${MCP_DEFINITION_ENDPOINT}",
            "headers": {"X-Secret": "!printf $MCP_DEFINITION_SECRET"},
            "oauth": {"clientSecret": "${MCP_DEFINITION_SECRET}"},
            "auth": {"provider": "fixture-provider"}
        }));
        let mut other_policy = selected.clone();
        other_policy.enabled = false;
        other_policy.policy = McpServerPolicy {
            exposure: McpExposure::Hidden,
            tool_exposure: vec![("private".into(), McpExposure::Direct)],
            timeout: Duration::from_secs(1),
            description: Some("generation-local description".into()),
            root: Some(PathBuf::from("/different-root")),
            log_path: Some(PathBuf::from("/different-log")),
        };
        let definition = selected.into_connection_definition().unwrap();
        assert_eq!(
            definition,
            other_policy.into_connection_definition().unwrap()
        );
        assert_eq!(definition.config.name, "server");
        assert!(matches!(
            &definition.config.transport,
            McpTransport::Http { url, headers }
                if url == "${MCP_DEFINITION_ENDPOINT}"
                    && headers["X-Secret"] == "!printf $MCP_DEFINITION_SECRET"
        ));
        assert_eq!(
            definition.config.oauth.unwrap().client_secret.as_deref(),
            Some("${MCP_DEFINITION_SECRET}")
        );
        assert_eq!(definition.config.auth.unwrap().provider, "fixture-provider");
    }

    #[test]
    fn connection_definition_preserves_missing_transport_diagnostics() {
        for config in [json!({"enabled": false}), json!({"exposure": "hidden"})] {
            assert_eq!(
                entry(config).into_connection_definition().unwrap_err(),
                "Enabled MCP server needs a transport"
            );
        }
    }

    #[test]
    fn parse_config_infers_transports_and_keeps_disabled_entries() {
        let paths = ConfigPaths::new();
        let stdio = entry(json!({"command": "server", "future": {"ignored": true}}));
        assert_eq!(stdio.name(), "server");
        assert!(stdio.enabled());
        assert!(matches!(
            stdio.transport(),
            Some(McpTransport::Stdio { .. })
        ));
        assert!(matches!(
            entry(json!({"url": "https://example.com/mcp"})).transport(),
            Some(McpTransport::Http { .. })
        ));
        let disabled = entry(json!({"enabled": false}));
        assert!(!disabled.enabled());
        assert!(disabled.transport().is_none());
        assert_eq!(
            disabled.resolve_transport(paths.context()).unwrap_err(),
            "Enabled MCP server needs a transport"
        );
        // Disabling an entry does not skip validation of an explicit transport.
        let disabled = entry(json!({"enabled": false, "command": "server"}));
        assert!(!disabled.enabled());
        assert!(disabled.transport().is_some());
    }

    #[test]
    fn common_pi_documents_and_http_aliases_are_accepted() {
        for content in ["{}", "{\"version\":1}", "\u{feff}{\"mcpServers\":{}}"] {
            assert!(parse_config(content).unwrap().is_empty());
        }
        for config in [
            json!({"type": "streamable-http", "url": "https://example.com/mcp"}),
            json!({"command": "unused", "url": "https://example.com/mcp"}),
        ] {
            assert!(matches!(
                entry(config).transport(),
                Some(McpTransport::Http { .. })
            ));
        }
    }

    #[test]
    fn omitted_exposure_defaults_to_codemode_and_direct_remains_explicit() {
        let omitted = entry(json!({"command":"server"}));
        assert_eq!(
            omitted.policy().exposure_for("search"),
            McpExposure::Codemode
        );
        assert!(omitted.policy().uses_codemode());
        let direct = entry(json!({"command":"server", "exposure":"direct"}));
        assert_eq!(direct.policy().exposure_for("search"), McpExposure::Direct);
        assert!(!direct.policy().uses_codemode());
    }

    #[test]
    fn codemode_alias_and_overrides_preserve_script_exposure() {
        for exposure in ["codemode", "codemode-deferred"] {
            let config = entry(json!({
                "command": "server", "exposure": exposure,
                "toolExposure": {"read*": "direct", "private": "hidden"}
            }));
            assert_eq!(
                config.policy().exposure_for("search"),
                McpExposure::Codemode
            );
            assert_eq!(
                config.policy().exposure_for("read_file"),
                McpExposure::Direct
            );
            assert_eq!(config.policy().exposure_for("private"), McpExposure::Hidden);
            assert!(config.policy().uses_codemode());
            assert!(!config.policy().uses_deferred());
        }
        let config = entry(json!({"command":"server", "toolExposure":{"search":"codemode"}}));
        assert!(config.policy().uses_codemode());
        assert_eq!(
            config.policy().exposure_for("unmatched"),
            McpExposure::Codemode
        );
    }

    #[test]
    fn known_unsupported_policies_are_not_silently_promoted_to_direct() {
        for (field, value) in [
            ("oauth", json!({"clientSecret": "secret"})),
            ("auth", json!({"provider": "secret"})),
        ] {
            let mut config = json!({"command": "server"});
            config[field] = value;
            let error =
                parse_config(&json!({"mcpServers": {"server": config}}).to_string()).unwrap_err();
            assert!(error.contains(field), "{error}");
            assert!(!error.contains("secret"), "{error}");
        }
        for config in [
            json!({"command":"server","timeout":0}),
            json!({"command":"server","description":42}),
        ] {
            assert!(parse_config(&json!({"mcpServers":{"server":config}}).to_string()).is_err());
        }
    }

    #[test]
    fn exposure_preserves_declared_wildcard_order_and_exact_precedence() {
        let config = parse_config(r#"{"mcpServers":{"server":{"command":"server","exposure":"hidden","description":"Useful tools","timeout":0.25,"toolExposure":{"z*":"hidden","*":"direct","z-safe":"direct"}}}}"#).unwrap().pop().unwrap();
        assert_eq!(
            config.policy().exposure_for("z-secret"),
            McpExposure::Hidden
        );
        assert_eq!(config.policy().exposure_for("z-safe"), McpExposure::Direct);
        assert_eq!(config.policy().exposure_for("other"), McpExposure::Direct);
        assert_eq!(config.policy().timeout, Duration::from_millis(250));
        assert_eq!(config.policy().description.as_deref(), Some("Useful tools"));
    }

    #[test]
    fn deferred_policy_preserves_overrides_and_requests_discovery() {
        let config = entry(
            json!({"command":"server","exposure":"deferred", "toolExposure":{"read*":"direct", "read-secret":"hidden"}}),
        );
        assert_eq!(config.policy().exposure_for("write"), McpExposure::Deferred);
        assert_eq!(
            config.policy().exposure_for("read-file"),
            McpExposure::Direct
        );
        assert_eq!(
            config.policy().exposure_for("read-secret"),
            McpExposure::Hidden
        );
        assert!(config.policy().uses_deferred());
    }

    #[test]
    fn document_preserves_auto_enable_codemode_and_omission() {
        for content in ["{}", r#"{"mcpServers":{}}"#] {
            assert_eq!(parse_document(content).unwrap().auto_enable_codemode, None);
        }
        for enabled in [true, false] {
            for with_server in [true, false] {
                let mut document = json!({"autoEnableCodemode": enabled});
                if with_server {
                    document["mcpServers"] = json!({"docs":{"command":"server"}});
                }
                let content = document.to_string();
                let parsed = parse_document(&content).unwrap();
                assert_eq!(parsed.auto_enable_codemode, Some(enabled));
                assert_eq!(parsed.servers.len(), usize::from(with_server));
                let error = parse_config(&content).unwrap_err();
                assert!(error.contains("use parse_document or McpLibrary"));
            }
        }
    }

    #[test]
    fn document_rejects_invalid_auto_enable_codemode_without_echoing_values() {
        for value in [Value::Null, json!(0), json!("secret"), json!([]), json!({})] {
            let error =
                parse_document(&json!({"autoEnableCodemode":value}).to_string()).unwrap_err();
            assert_eq!(error, "mcp.json: autoEnableCodemode must be a boolean");
        }
    }

    #[test]
    fn transport_only_resolution_cannot_discard_policy() {
        let paths = ConfigPaths::new();
        for config in [
            json!({"command": "server"}),
            json!({"command": "server", "exposure": "codemode"}),
            json!({"command": "server", "exposure": "hidden"}),
            json!({"command": "server", "exposure": "deferred"}),
            json!({"command": "server", "toolExposure": {"private": "hidden"}}),
            json!({"command": "server", "timeout": 1}),
            json!({"command": "server", "description": "Useful tools"}),
        ] {
            let error = entry(config).resolve(paths.context()).unwrap_err();
            assert!(error.contains("transport only"), "{error}");
            assert!(error.contains("McpLibrary::prepare"), "{error}");
        }
        assert!(
            entry(json!({"command": "server", "exposure": "direct"}))
                .resolve(paths.context())
                .is_ok()
        );
    }

    #[test]
    fn namespace_collisions_and_invalid_server_names_are_rejected() {
        for name in ["space name", "a.b", "你好", "", " leading"] {
            assert!(
                parse_config(&json!({"mcpServers": {name: {"command": "server"}}}).to_string())
                    .is_err()
            );
        }
        assert!(
            parse_config(r#"{"mcpServers":{"a-b":{"command":"one"},"a_b":{"command":"two"}}}"#)
                .unwrap_err()
                .contains("namespace conflicts")
        );
    }

    #[test]
    fn parse_config_rejects_invalid_documents_without_echoing_values() {
        for (content, expected) in [
            ("{secret", "Invalid mcp.json syntax"),
            ("[]", "mcp.json must be an object"),
            (
                r#"{"version":2,"mcpServers":{}}"#,
                "Unsupported mcp.json version",
            ),
            (
                r#"{"mcpServers":null}"#,
                "mcp.json needs an mcpServers object",
            ),
            (
                r#"{"mcpServers":{" server ":{}}}"#,
                "MCP server names must use only letters",
            ),
            (
                r#"{"mcpServers":{"server":42}}"#,
                "MCP server server must be an object",
            ),
            (
                r#"{"mcpServers":{"server":{"enabled":"secret"}}}"#,
                "enabled must be boolean",
            ),
            (
                r#"{"mcpServers":{"server":{"command":" "}}}"#,
                "command is empty",
            ),
            (
                r#"{"mcpServers":{"server":{"command":"node","timeout":1e19}}}"#,
                "timeout must be a positive number of seconds",
            ),
            (
                r#"{"mcpServers":{"server":{"url":"https://example.com","headers":{"Authorization":false}}}}"#,
                "invalid transport fields",
            ),
        ] {
            let error = parse_config(content).unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert!(!error.contains("secret"), "{error}");
        }
        assert_eq!(
            parse_config(&" ".repeat(MAX_CONFIG_BYTES as usize + 1)).unwrap_err(),
            "mcp.json exceeds 1 MiB"
        );
    }

    #[test]
    fn parse_config_validates_static_http_fields_without_environment_access() {
        let remote = entry(json!({
            "url": "${MCP_ENDPOINT}",
            "headers": {"Authorization": "Bearer ${env:TOKEN}"}
        }));
        assert!(
            matches!(remote.transport(), Some(McpTransport::Http { url, .. }) if url == "${MCP_ENDPOINT}")
        );
        for config in [
            json!({"type": "sse", "url": "https://example.com/secret"}),
            json!({"url": "ftp://example.com/secret"}),
            json!({"url": "https://secret@example.com"}),
            json!({"url": "https://example.com/#secret"}),
            json!({"url": "https://example.com", "headers": {"Invalid Header": "secret"}}),
            json!({"url": "https://example.com", "headers": {"Authorization": "secret\n"}}),
            json!({"url": "https://example.com", "headers": {"Mcp-Session-Id": "secret"}}),
            json!({"enabled": false, "type": "http", "url": "secret"}),
        ] {
            let error =
                parse_config(&json!({"mcpServers": {"server": config}}).to_string()).unwrap_err();
            assert!(!error.contains("secret"), "{error}");
        }
    }

    #[test]
    fn resolve_expands_http_values_but_not_header_names() {
        let paths = ConfigPaths::new();
        let remote = entry(json!({
            "url": "$ENDPOINT",
            "headers": {"Authorization": "Bearer ${env:TOKEN}", "X-$TOKEN": "${TOKEN}"}
        }));
        let config = remote
            .resolve_with(paths.context(), None, |key| match key {
                "ENDPOINT" => Ok("https://example.com/mcp".into()),
                "TOKEN" => Ok("secret".into()),
                _ => panic!("unexpected lookup: {key}"),
            })
            .unwrap();
        let McpTransport::Http { url, headers } = config.transport else {
            panic!("expected HTTP")
        };
        assert_eq!(url, "https://example.com/mcp");
        assert_eq!(headers["Authorization"], "Bearer secret");
        assert_eq!(headers["X-$TOKEN"], "secret");
    }

    #[test]
    fn resolve_expands_stdio_values_and_uses_explicit_path_bases() {
        let paths = ConfigPaths::new();
        let stdio = entry(json!({
            "command": "${COMMAND}",
            "args": ["${TOKEN}", "~/literal", "$(do-not-execute)"],
            "env": {"TOKEN": "${env:TOKEN}"},
            "cwd": "${WORKDIR}"
        }));
        let home = paths.config_dir.join("home");
        let config = stdio
            .resolve_with(paths.context(), Some(&home), |key| match key {
                "COMMAND" => Ok("~/bin/server".into()),
                "TOKEN" => Ok("value".into()),
                "WORKDIR" => Ok("relative".into()),
                _ => panic!("unexpected lookup: {key}"),
            })
            .unwrap();
        let McpTransport::Stdio {
            command,
            args,
            env,
            cwd,
        } = config.transport
        else {
            panic!("expected stdio")
        };
        assert_eq!(command, home.join("bin/server").to_string_lossy());
        assert_eq!(
            args,
            [
                "value".to_string(),
                home.join("literal").to_string_lossy().into_owned(),
                "$(do-not-execute)".to_string()
            ]
        );
        assert_eq!(env["TOKEN"], "value");
        assert_eq!(cwd.unwrap(), paths.default_cwd.join("relative"));

        let absolute = paths.config_dir.join("absolute");
        for (cwd, expected) in [
            (None, paths.default_cwd.clone()),
            (Some(absolute.to_string_lossy().into_owned()), absolute),
            (Some("~/work".into()), home.join("work")),
            (Some("~".into()), home.clone()),
        ] {
            let mut fields = json!({"command": "server"});
            if let Some(cwd) = cwd {
                fields["cwd"] = json!(cwd);
            }
            let resolved = entry(fields)
                .resolve_with(paths.context(), Some(&home), |_| {
                    panic!("no environment lookup expected")
                })
                .unwrap();
            assert!(
                matches!(resolved.transport, McpTransport::Stdio { cwd: Some(cwd), .. } if cwd == expected)
            );
        }
        assert_eq!(expand_tilde_path("~/work", None), Path::new("~/work"));
        assert_eq!(
            expand_tilde_path("~other/work", Some(&home)),
            Path::new("~other/work")
        );
    }

    #[test]
    fn environment_expansion_is_deferred_and_never_runs_shell_commands() {
        let paths = ConfigPaths::new();
        let lookup = |key: &str| match key {
            "TOKEN" => Ok("secret".into()),
            _ => Err("missing".into()),
        };
        assert_eq!(
            expand_with("Bearer ${TOKEN}", lookup).unwrap(),
            "Bearer secret"
        );
        assert_eq!(expand_with("$TOKEN", lookup).unwrap(), "secret");
        assert_eq!(expand_with("${env:TOKEN}", lookup).unwrap(), "secret");
        assert_eq!(
            expand_with("$(do-not-execute)", lookup).unwrap(),
            "$(do-not-execute)"
        );
        assert_eq!(
            expand_with("prefix $TOKEN", lookup).unwrap(),
            "prefix $TOKEN"
        );
        assert!(expand_with("${MISSING}", lookup).is_err());
        assert!(expand_with("${UNCLOSED", lookup).is_err());
        assert!(expand_with("${1INVALID}", lookup).is_err());
        let unresolved = entry(json!({"command": "${MISSING}"}));
        assert_eq!(
            unresolved
                .resolve_with(paths.context(), None, lookup)
                .unwrap_err(),
            "missing"
        );
    }
}
