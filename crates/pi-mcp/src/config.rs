//! MCP document parsing and connection-time transport resolution.
//!
//! Parsing validates static transport fields without reading environment variables,
//! opening files or connecting. Callers own file discovery, scope precedence, trust
//! and persistence; only selected entries are resolved immediately before connecting.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::{McpServerConfig, McpTransport};

/// Maximum supported MCP document size, also used to bound callers' disk reads.
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// A validated document entry whose environment references are still unresolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpConfigEntry {
    name: String,
    transport: Option<McpTransport>,
    enabled: bool,
}

/// Paths selected by the caller after resolving configuration scope and trust.
#[derive(Clone, Copy, Debug)]
pub struct McpConfigContext<'a> {
    /// Base for an explicitly configured relative stdio working directory.
    pub config_dir: &'a Path,
    /// Working directory used when the stdio entry does not specify one.
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

    /// Expands environment references and stdio paths without starting a transport.
    /// Callers select enabled entries; a disabled entry may have no transport.
    pub fn resolve(self, context: McpConfigContext<'_>) -> Result<McpServerConfig, String> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        self.resolve_with(context, home.as_deref(), |key| {
            std::env::var(key)
                .map_err(|_| format!("MCP environment variable {key} is not set or is not UTF-8"))
        })
    }

    fn resolve_with(
        self,
        context: McpConfigContext<'_>,
        home: Option<&Path>,
        lookup: impl Fn(&str) -> Result<String, String>,
    ) -> Result<McpServerConfig, String> {
        let mut transport = self
            .transport
            .ok_or("Enabled MCP server needs a transport")?;
        match &mut transport {
            McpTransport::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                *command = expand_tilde_path(&expand_with(command, &lookup)?, home)
                    .to_string_lossy()
                    .into_owned();
                for arg in args {
                    *arg = expand_with(arg, &lookup)?;
                }
                for value in env.values_mut() {
                    *value = expand_with(value, &lookup)?;
                }
                *cwd = Some(match cwd.take() {
                    Some(path) => {
                        let path = expand_tilde_path(
                            &expand_with(&path.to_string_lossy(), &lookup)?,
                            home,
                        );
                        if path.is_absolute() {
                            path
                        } else {
                            context.config_dir.join(path)
                        }
                    }
                    None => context.default_cwd.to_path_buf(),
                });
            }
            McpTransport::Http { url, headers } => {
                *url = expand_with(url, &lookup)?;
                for value in headers.values_mut() {
                    *value = expand_with(value, &lookup)?;
                }
            }
        }
        Ok(McpServerConfig {
            name: self.name,
            transport,
        })
    }
}

/// Parses an `mcpServers` document (the version-1 marker is optional) and validates static fields.
pub fn parse_config(content: &str) -> Result<Vec<McpConfigEntry>, String> {
    if content.len() as u64 > MAX_CONFIG_BYTES {
        return Err("mcp.json exceeds 1 MiB".into());
    }
    let value: Value = serde_json::from_str(content).map_err(|e| {
        format!(
            "Invalid mcp.json syntax at line {}, column {}",
            e.line(),
            e.column()
        )
    })?;
    let object = value.as_object().ok_or("mcp.json must be an object")?;
    if object.get("version").is_some_and(|v| v != 1) {
        return Err("Unsupported mcp.json version (expected 1)".into());
    }
    let servers = object
        .get("mcpServers")
        .and_then(Value::as_object)
        .ok_or("mcp.json needs an mcpServers object")?;
    let mut entries = Vec::new();
    for (name, value) in servers {
        if name.trim().is_empty() || name != name.trim() {
            return Err(
                "MCP server names must be non-empty, without surrounding whitespace".into(),
            );
        }
        let mut config = value
            .as_object()
            .cloned()
            .ok_or_else(|| format!("MCP server {name} must be an object"))?;
        let enabled = match config.remove("enabled") {
            None => true,
            Some(Value::Bool(value)) => value,
            _ => return Err(format!("MCP server {name}: enabled must be boolean")),
        };
        // Accept the common mcpServers format without a redundant explicit type.
        if !config.contains_key("type") {
            if config.contains_key("command") {
                config.insert("type".into(), json!("stdio"));
            } else if config.contains_key("url") {
                config.insert("type".into(), json!("http"));
            }
        }
        let transport = if !enabled && !config.contains_key("type") {
            None
        } else {
            if config.get("type") == Some(&json!("sse")) {
                return Err(format!(
                    "MCP server {name}: legacy SSE is not supported; use a Streamable HTTP endpoint"
                ));
            }
            let transport: McpTransport = serde_json::from_value(Value::Object(config)).map_err(|_| format!("MCP server {name}: invalid transport fields (stdio command/args/env/cwd or http url/headers)"))?;
            // Validate static fields without resolving environment references or executing anything.
            match &transport {
                McpTransport::Stdio { command, .. } if command.trim().is_empty() => {
                    return Err(format!("MCP server {name}: command is empty"));
                }
                McpTransport::Http { url, headers } => {
                    let placeholder =
                        |value: &str| expand_with(value, |_| Ok("placeholder".into()));
                    let checked_url = placeholder(url)?;
                    let checked_url = if url.starts_with('$') {
                        "https://mcp.invalid".into()
                    } else {
                        checked_url
                    };
                    let config = McpServerConfig::http(name, checked_url).headers(
                        headers
                            .iter()
                            .map(|(k, v)| Ok((k.clone(), placeholder(v)?)))
                            .collect::<Result<_, String>>()?,
                    );
                    crate::validate_configs(&[config]).map_err(|e| e.to_string())?;
                }
                _ => {}
            }
            Some(transport)
        };
        entries.push(McpConfigEntry {
            name: name.clone(),
            transport,
            enabled,
        });
    }
    Ok(entries)
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
                config_dir: root.join("pi-mcp-configuration"),
                default_cwd: root.join("pi-mcp-session"),
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
            disabled.resolve(paths.context()).unwrap_err(),
            "Enabled MCP server needs a transport"
        );
        // Disabling an entry does not skip validation of an explicit transport.
        let disabled = entry(json!({"enabled": false, "command": "server"}));
        assert!(!disabled.enabled());
        assert!(disabled.transport().is_some());
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
            (r#"{"version":1}"#, "mcp.json needs an mcpServers object"),
            (
                r#"{"mcpServers":{" server ":{}}}"#,
                "MCP server names must be non-empty",
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
        assert_eq!(args, ["value", "~/literal", "$(do-not-execute)"]);
        assert_eq!(env["TOKEN"], "value");
        assert_eq!(cwd.unwrap(), paths.config_dir.join("relative"));

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
