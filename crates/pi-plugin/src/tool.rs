use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{
    AbortSignal, ContextParts, ModelsContext, PluginContextHandle, RunId, SessionContext,
    ToolCallId, ToolResult, ToolSpec, ToolUpdate, UiContext,
};

/// Whether a tool is declared by default or loaded through discovery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolExposure {
    #[default]
    Direct,
    Deferred,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolNamespace {
    pub name: String,
    pub description: Option<String>,
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolOrigin {
    #[default]
    Local,
    Mcp,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolDiscovery {
    pub origin: ToolOrigin,
    pub exposure: ToolExposure,
    pub namespace: Option<ToolNamespace>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    pub spec: ToolSpec,
    pub discovery: ToolDiscovery,
}

/// Original metadata for one request. Description rewrites never feed back into this view.
#[derive(Debug, Clone, Default)]
pub struct ToolLoadout {
    pub declared: Vec<ToolInfo>,
    pub callable: Vec<ToolInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolLoadoutChanges {
    pub descriptions: std::collections::BTreeMap<String, String>,
    pub hidden_declarations: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PreparedToolLoadout {
    /// Complete executable definitions after description preparation. Transcript
    /// declarations retain these even when a request projection hides a tool.
    pub declarations: Vec<ToolSpec>,
    pub tools: Vec<ToolSpec>,
    pub hidden_tools: Vec<String>,
}

/// A nested call does not create a model-visible tool-result message. The
/// caller decides what to expose from the final, hook-processed result.
#[derive(Debug, Clone)]
pub struct NestedToolResult {
    pub call_id: ToolCallId,
    pub result: ToolResult,
    /// Explicit structured output supplied by the tool, never inferred from
    /// its private presentation details.
    pub structured_content: Option<Value>,
}

/// Host-injected capability tied to one invocation and its immutable scope.
#[async_trait]
pub trait NestedToolExecutor: Send + Sync {
    /// Accounts for work outside nested tool calls, so parent cancellation can
    /// preserve usage even when it replaces the outer tool result.
    fn record_usage(&self, _usage: &crate::Usage) {}

    async fn execute(
        &self,
        name: &str,
        arguments: Value,
        signal: AbortSignal,
    ) -> Result<NestedToolResult, ToolError>;
}

#[derive(Clone)]
pub struct ToolUpdateSink {
    sender: mpsc::UnboundedSender<ToolUpdate>,
}

impl ToolUpdateSink {
    pub fn channel() -> (Self, mpsc::UnboundedReceiver<ToolUpdate>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self { sender }, receiver)
    }

    pub fn send(&self, update: ToolUpdate) -> bool {
        self.sender.send(update).is_ok()
    }
}

#[derive(Clone)]
pub struct ToolContext {
    workspace: crate::WorkspaceSnapshot,
    abort_signal: AbortSignal,
    pub session: SessionContext,
    pub models: ModelsContext,
    pub ui: UiContext,
    run_id: Option<RunId>,
    discoverable_tools: Vec<ToolInfo>,
    nested_tools: Arc<Vec<ToolInfo>>,
    nested_executor: Option<Arc<dyn NestedToolExecutor>>,
}

impl ToolContext {
    /// Constructs a context for running a tool outside a Pi session.
    ///
    /// Session, model, and presentation capabilities are intentionally
    /// unavailable on standalone contexts.
    pub fn standalone(cwd: PathBuf, abort_signal: AbortSignal) -> Self {
        Self::with_plugin_context(cwd, abort_signal, ContextParts::unavailable())
    }

    #[doc(hidden)]
    pub fn with_plugin_context(
        cwd: PathBuf,
        abort_signal: AbortSignal,
        context: ContextParts,
    ) -> Self {
        Self {
            workspace: context.workspace_or_cwd(&cwd),
            abort_signal,
            session: context.session,
            models: context.models,
            ui: context.ui,
            run_id: None,
            discoverable_tools: Vec::new(),
            nested_tools: Arc::default(),
            nested_executor: None,
        }
    }

    pub fn workspace(&self) -> &crate::WorkspaceSnapshot {
        &self.workspace
    }

    pub fn cwd(&self) -> &std::path::Path {
        self.workspace.cwd()
    }

    pub fn signal(&self) -> &AbortSignal {
        &self.abort_signal
    }

    /// The same execution identity observed by agent plugin hooks. Standalone
    /// tool calls have no Agent run; plugin-owned state is never stored here.
    pub fn run_id(&self) -> Option<&RunId> {
        self.run_id.as_ref()
    }

    /// Deferred tools available to this invocation and not yet declared.
    /// Standalone contexts have an empty catalog.
    pub fn discoverable_tools(&self) -> &[ToolInfo] {
        &self.discoverable_tools
    }

    /// Active direct tools and visible deferred tools within this invocation's
    /// execution scope. Model-only tools are excluded.
    pub fn nested_tools(&self) -> &[ToolInfo] {
        &self.nested_tools
    }

    /// Record supplemental provider work. Nested tool usage is recorded by the
    /// executor already and must not be passed again. Normal tool results still
    /// carry their total usage; this ledger is used only on parent interruption.
    /// Standalone contexts have no parent ledger.
    pub fn record_usage(&self, usage: &crate::Usage) {
        if let Some(executor) = &self.nested_executor {
            executor.record_usage(usage);
        }
    }

    /// Runs through the host's preparation, validation and tool hooks. Tool
    /// failures are returned as `result.is_error`; no transcript is appended.
    /// The supplied signal can cancel this call without cancelling its parent.
    /// The Agent host tracks started calls through cancellation and gives their
    /// result hooks up to one second to settle before aborting cleanup.
    /// The current Agent host supports one level: contexts of nested tools do
    /// not receive another executor. Standalone contexts have no executor.
    pub async fn execute_tool(
        &self,
        name: &str,
        arguments: Value,
        signal: AbortSignal,
    ) -> Result<NestedToolResult, ToolError> {
        if self.abort_signal.is_aborted() || signal.is_aborted() {
            return Err(ToolError::Aborted);
        }
        let executor = self.nested_executor.as_ref().ok_or_else(|| {
            ToolError::Execution("nested tool execution is unavailable in this context".into())
        })?;
        executor.execute(name, arguments, signal).await
    }

    #[doc(hidden)]
    pub fn with_nested_executor(
        mut self,
        tools: impl Into<Arc<Vec<ToolInfo>>>,
        executor: Arc<dyn NestedToolExecutor>,
    ) -> Self {
        self.nested_tools = tools.into();
        self.nested_executor = Some(executor);
        self
    }

    #[doc(hidden)]
    pub fn with_discoverable_tools(mut self, tools: Vec<ToolInfo>) -> Self {
        self.discoverable_tools = tools;
        self
    }

    #[doc(hidden)]
    pub fn with_run_id(mut self, run_id: RunId) -> Self {
        self.run_id = Some(run_id);
        self
    }

    #[doc(hidden)]
    pub fn plugin_context_handle(&self) -> PluginContextHandle {
        self.session.handle_for_adapter()
    }
}

/// Keeps the advertised schema intact while guarding preparation AND execution.
pub(crate) struct ScopedTool {
    pub inner: Arc<dyn Tool>,
    pub allowed: bool,
    pub origin: Arc<str>,
}

impl ScopedTool {
    fn ensure_allowed(&self) -> Result<(), ToolError> {
        if !self.allowed {
            return Err(ToolError::Execution(format!(
                "{} denied tool {}. Do not retry this tool in this run.",
                self.origin,
                self.inner.spec().name
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for ScopedTool {
    fn spec(&self) -> ToolSpec {
        self.inner.spec()
    }

    fn model_only(&self) -> bool {
        self.inner.model_only()
    }

    fn reaches_tools(&self) -> bool {
        self.allowed && self.inner.reaches_tools()
    }

    fn structured_output(&self, result: &ToolResult) -> Option<Value> {
        self.inner.structured_output(result)
    }

    fn prepare_loadout(&self, loadout: &ToolLoadout) -> ToolLoadoutChanges {
        self.inner.prepare_loadout(loadout)
    }

    async fn prepare_arguments(
        &self,
        context: &ToolContext,
        input: Value,
    ) -> Result<Value, ToolError> {
        self.ensure_allowed()?;
        self.inner.prepare_arguments(context, input).await
    }

    fn validate_arguments(&self, input: &Value) -> Result<(), ToolError> {
        self.inner.validate_arguments(input)
    }

    async fn execute(
        &self,
        context: ToolContext,
        id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        self.ensure_allowed()?;
        self.inner.execute(context, id, input, updates).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool execution aborted")]
    Aborted,
    #[error("invalid tool arguments: {0}")]
    InvalidArguments(String),
    #[error("tool failed: {0}")]
    Execution(String),
    #[error(transparent)]
    Context(#[from] crate::PluginContextError),
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    /// Tools that control model-visible execution are not callable by other
    /// tools. This is enforced before preparation, including guessed names.
    fn model_only(&self) -> bool {
        false
    }

    /// Whether this tool can execute or activate other registered tools. Hosts
    /// use this marker when making discovery entry points available.
    fn reaches_tools(&self) -> bool {
        false
    }

    /// Opt in to structured nested results. Ordinary tools expose text; their
    /// internal `details` remain presentation data rather than a result schema.
    fn structured_output(&self, result: &ToolResult) -> Option<Value> {
        result.structured_content.clone()
    }

    fn prepare_loadout(&self, _loadout: &ToolLoadout) -> ToolLoadoutChanges {
        ToolLoadoutChanges::default()
    }

    async fn prepare_arguments(
        &self,
        _context: &ToolContext,
        input: Value,
    ) -> Result<Value, ToolError> {
        Ok(input)
    }

    fn validate_arguments(&self, input: &Value) -> Result<(), ToolError> {
        if input.is_object() {
            Ok(())
        } else {
            Err(ToolError::InvalidArguments(
                "arguments must be a JSON object".to_string(),
            ))
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        tool_call_id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ReachingTool;
    #[async_trait]
    impl Tool for ReachingTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                output_schema: None,
                constrained_sampling: None,
                name: "entry".into(),
                label: "Entry".into(),
                description: "Discovery entry point".into(),
                parameters: serde_json::json!({"type":"object"}),
                execution_mode: crate::ToolExecutionMode::Sequential,
                prompt_snippet: None,
                prompt_guidelines: vec![],
            }
        }
        fn reaches_tools(&self) -> bool {
            true
        }
        async fn execute(
            &self,
            _context: ToolContext,
            _id: ToolCallId,
            _input: Value,
            _updates: ToolUpdateSink,
        ) -> Result<ToolResult, ToolError> {
            unreachable!("capability queries must not execute tools")
        }
    }

    #[test]
    fn denied_execution_scope_cannot_advertise_reaching_capability() {
        let tool: Arc<dyn Tool> = Arc::new(ReachingTool);
        let allowed: Arc<dyn Tool> = Arc::new(ScopedTool {
            inner: tool.clone(),
            allowed: true,
            origin: "parent".into(),
        });
        assert!(allowed.reaches_tools());
        let denied: Arc<dyn Tool> = Arc::new(ScopedTool {
            inner: tool,
            allowed: false,
            origin: "parent".into(),
        });
        assert!(!denied.reaches_tools());
        let wrapped = ScopedTool {
            inner: denied,
            allowed: true,
            origin: "child".into(),
        };
        assert!(!wrapped.reaches_tools());
    }
}
