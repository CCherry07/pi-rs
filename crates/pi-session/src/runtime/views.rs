//! Read-only queries over a live session, without exposing its mutable owners.

use std::path::Path;
use std::sync::Arc;

use pi_core::{ModelId, ModelSpec, PluginId, ProviderId, ToolSpec, WorkspaceSnapshot};
use pi_plugin::{CommandSpec, PluginDiagnostic, ProviderStatus};
use pi_runtime::{PiRuntime, ResourceDiagnostic};

use crate::{
    BranchQuery, EntryQuery, JsonlSessionMetadata, LaneRecord, SessionContext, SessionDocument,
    SessionError, SessionHeader, SessionRecord, SessionStats, SessionStore,
};

/// Generation and catalog queries. This view cannot configure or reload the session.
/// Read the current tool selection from `session.snapshot().agent.active_tools`.
///
/// ```compile_fail
/// fn configure(session: &pi_session::AgentSession) {
///     session.catalog().set_active_tools(["read"]);
/// }
/// ```
/// ```compile_fail
/// async fn reload(session: &pi_session::AgentSession) {
///     session.catalog().reload().await;
/// }
/// ```
pub struct SessionCatalog<'a> {
    pub(crate) runtime: &'a PiRuntime,
}

impl<'a> SessionCatalog<'a> {
    pub fn workspace(&self) -> &'a WorkspaceSnapshot {
        self.runtime.workspace()
    }

    pub fn cwd(&self) -> &'a Path {
        self.runtime.cwd()
    }

    pub fn generation(&self) -> u64 {
        self.runtime.generation()
    }

    pub fn execution_origin(&self) -> pi_plugin::SessionExecutionOrigin {
        self.runtime.execution_origin()
    }

    pub fn models(&self) -> Vec<ModelSpec> {
        self.runtime.models()
    }

    pub fn available_models(&self) -> Vec<ModelSpec> {
        self.runtime.available_models()
    }

    pub fn model(&self, provider: &ProviderId, model: &ModelId) -> Option<ModelSpec> {
        self.runtime.model(provider, model)
    }

    pub fn resolve_model_reference(
        &self,
        provider: &ProviderId,
        reference: &str,
    ) -> Option<ModelSpec> {
        self.runtime.resolve_model_reference(provider, reference)
    }

    pub fn resolve_available_model_reference(
        &self,
        provider: &ProviderId,
        reference: &str,
    ) -> Option<ModelSpec> {
        self.runtime
            .resolve_available_model_reference(provider, reference)
    }

    pub fn provider_statuses(&self) -> Vec<ProviderStatus> {
        self.runtime.provider_statuses()
    }

    pub fn provider_name(&self, provider: &ProviderId) -> Option<String> {
        self.runtime.provider_name(provider)
    }

    pub fn provider_is_available(&self, provider: &ProviderId) -> bool {
        self.runtime.provider_is_available(provider)
    }

    pub fn has_provider(&self, provider: &ProviderId) -> bool {
        self.runtime.has_provider(provider)
    }

    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.runtime.tool_specs()
    }

    pub fn command_specs(&self) -> Vec<CommandSpec> {
        self.runtime.command_specs()
    }

    pub fn plugin_order(&self) -> Vec<PluginId> {
        self.runtime.plugin_order()
    }

    pub fn provider_plugin_order(&self) -> Vec<PluginId> {
        self.runtime.provider_plugin_order()
    }

    pub fn plugin_diagnostics(&self) -> Vec<PluginDiagnostic> {
        self.runtime.plugin_diagnostics()
    }

    pub fn resource_diagnostics(&self) -> Vec<ResourceDiagnostic> {
        self.runtime.resource_diagnostics()
    }

    pub fn prompt_options(&self) -> Option<serde_json::Value> {
        self.runtime.prompt_options()
    }

    pub fn resource_options(&self) -> Option<serde_json::Value> {
        self.runtime.resource_options()
    }
}

/// Read-only journal queries. Returned documents and records are snapshots, not write handles.
///
/// ```compile_fail
/// fn append(session: &pi_session::AgentSession, message: pi_core::Message) {
///     session.history().append_message(message);
/// }
/// ```
/// ```compile_fail
/// fn raw_store(session: &pi_session::AgentSession) -> &pi_session::SessionStore {
///     session.store()
/// }
/// ```
/// ```compile_fail
/// fn raw_runtime(session: &pi_session::AgentSession) -> &pi_runtime::PiRuntime {
///     session.runtime()
/// }
/// ```
pub struct SessionHistory<'a> {
    pub(crate) store: &'a SessionStore,
}

impl<'a> SessionHistory<'a> {
    pub fn main_branch_snapshot(&self) -> Result<Vec<crate::SessionMutation>, SessionError> {
        self.store.main_branch_snapshot().map_err(Into::into)
    }

    pub fn id(&self) -> &'a str {
        self.store.id()
    }

    pub fn path(&self) -> &'a Path {
        self.store.path()
    }

    pub fn header(&self) -> SessionHeader {
        self.store.header()
    }

    pub fn metadata(&self) -> Result<JsonlSessionMetadata, SessionError> {
        self.store.metadata().map_err(Into::into)
    }

    pub fn is_materialized(&self) -> bool {
        self.store.is_materialized()
    }

    pub fn load(&self) -> Result<SessionDocument, SessionError> {
        self.store.load().map_err(Into::into)
    }

    pub fn shared_document(&self) -> Result<Arc<SessionDocument>, SessionError> {
        self.store.shared_document().map_err(Into::into)
    }

    pub fn leaf_id(&self) -> Option<String> {
        self.store.leaf_id()
    }

    pub fn get_entry(&self, id: &str) -> Option<SessionRecord> {
        self.store.get_entry(id)
    }

    pub fn branch_entries(&self) -> Result<Vec<SessionRecord>, SessionError> {
        self.store.branch_entries().map_err(Into::into)
    }

    pub fn find_entries(&self, query: &EntryQuery) -> Result<Vec<SessionRecord>, SessionError> {
        self.store.find_entries(query).map_err(Into::into)
    }

    pub fn find_entries_on_branch(
        &self,
        query: &BranchQuery,
    ) -> Result<Vec<SessionRecord>, SessionError> {
        self.store.find_entries_on_branch(query).map_err(Into::into)
    }

    pub fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<usize>,
    ) -> Result<Vec<LaneRecord>, SessionError> {
        self.store
            .find_open_operations(lane, limit)
            .map_err(Into::into)
    }

    pub fn context(&self) -> Result<SessionContext, SessionError> {
        self.store.context().map_err(Into::into)
    }

    pub fn name(&self) -> Option<String> {
        self.store.name()
    }

    pub fn stats(&self) -> SessionStats {
        self.store.stats()
    }
}
