//! Generation-local MCP policy, separate from transport credentials and configuration storage.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How a configured MCP server exposes its tools to the model.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpExposure {
    Direct,
    Codemode,
    Deferred,
    Hidden,
}

impl McpExposure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Codemode => "codemode",
            Self::Deferred => "deferred",
            Self::Hidden => "hidden",
        }
    }
}

impl FromStr for McpExposure {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "direct" => Ok(Self::Direct),
            "codemode" | "codemode-deferred" => Ok(Self::Codemode),
            "deferred" => Ok(Self::Deferred),
            "hidden" => Ok(Self::Hidden),
            _ => Err("exposure must be direct, codemode, deferred or hidden".into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct McpServerPolicy {
    pub exposure: McpExposure,
    /// Declaration order matters: after exact names, Pi uses the first matching wildcard.
    pub tool_exposure: Vec<(String, McpExposure)>,
    pub description: Option<String>,
    pub timeout: Duration,
    pub root: Option<PathBuf>,
    /// Local-library-only diagnostics; never inferred by transport-only clients.
    pub log_path: Option<PathBuf>,
}

impl Default for McpServerPolicy {
    fn default() -> Self {
        Self {
            exposure: McpExposure::Codemode,
            tool_exposure: Vec::new(),
            description: None,
            timeout: Duration::from_secs(60),
            root: None,
            log_path: None,
        }
    }
}

impl McpServerPolicy {
    /// Explicit transport-only clients request tools without a script-entry plugin.
    pub fn direct() -> Self {
        Self {
            exposure: McpExposure::Direct,
            ..Self::default()
        }
    }

    pub fn uses_codemode(&self) -> bool {
        self.exposure == McpExposure::Codemode
            || self
                .tool_exposure
                .iter()
                .any(|(_, exposure)| *exposure == McpExposure::Codemode)
    }

    pub fn uses_deferred(&self) -> bool {
        self.exposure == McpExposure::Deferred
            || self
                .tool_exposure
                .iter()
                .any(|(_, exposure)| *exposure == McpExposure::Deferred)
    }

    pub fn exposure_for(&self, name: &str) -> McpExposure {
        self.tool_exposure
            .iter()
            .find(|(pattern, _)| pattern == name)
            .or_else(|| {
                self.tool_exposure
                    .iter()
                    .find(|(pattern, _)| pattern.contains('*') && matches_pattern(pattern, name))
            })
            .map_or(self.exposure, |(_, exposure)| *exposure)
    }
}

/// Only `*` is special; all other characters match literally, as in Pi's tool patterns.
fn matches_pattern(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let mut parts = parts.peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return rest.ends_with(part);
        }
        let Some(position) = rest.find(part) else {
            return false;
        };
        rest = &rest[position + part.len()..];
    }
    rest.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_names_precede_ordered_wildcards_and_patterns_are_literal() {
        let policy = McpServerPolicy {
            exposure: McpExposure::Hidden,
            tool_exposure: vec![
                ("read*".into(), McpExposure::Hidden),
                ("*".into(), McpExposure::Direct),
                ("read.allowed".into(), McpExposure::Direct),
            ],
            ..Default::default()
        };
        assert_eq!(policy.exposure_for("read.allowed"), McpExposure::Direct);
        assert_eq!(policy.exposure_for("read.secret"), McpExposure::Hidden);
        assert_eq!(policy.exposure_for("write"), McpExposure::Direct);
        assert!(matches_pattern("a.*.z", "a.b.c.z"));
        assert!(!matches_pattern("a.*.z", "axb.c.z"));
        assert!(!matches_pattern("abc*bc", "abc"));
        assert!(matches_pattern("**", "anything"));
    }
}
