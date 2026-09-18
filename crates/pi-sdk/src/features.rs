/// Runtime selection of first-party product features.
///
/// All features are enabled by default to preserve the standard Pi product.
/// Disabling a feature omits its built-in plugins and lifecycle work; it does not
/// remove compiled dependencies or restrict explicitly supplied plugins/tools.
/// The selection is captured by the host and reused across session replacements
/// and reloads. Feature-specific settings still apply when a feature is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Features {
    pub memory: bool,
    pub subagents: bool,
    pub schedule: bool,
    pub skills: bool,
    pub prompt_templates: bool,
    pub session_transfer: bool,
}

impl Features {
    /// Enable all first-party product features.
    pub const fn all() -> Self {
        Self {
            memory: true,
            subagents: true,
            schedule: true,
            skills: true,
            prompt_templates: true,
            session_transfer: true,
        }
    }

    /// Disable optional first-party features, retaining providers, core tools,
    /// session management and independently configured native/JS/MCP plugins.
    pub const fn none() -> Self {
        Self {
            memory: false,
            subagents: false,
            schedule: false,
            skills: false,
            prompt_templates: false,
            session_transfer: false,
        }
    }
}

impl Default for Features {
    fn default() -> Self {
        Self::all()
    }
}
