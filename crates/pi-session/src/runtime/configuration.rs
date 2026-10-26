use super::AgentSession;
use super::activity::ActivityPhase;
use super::options::AutoRetrySettings;
use crate::{
    ActiveToolsEntry, CompactionSettings, ModelChangeEntry, SessionEntry, SessionError,
    SessionInfoChangedEvent, ThinkingLevelEntry,
};
use pi_agent::QueueMode;
use pi_core::{ModelId, ProviderId, ThinkingLevel};
use pi_runtime::{PiRuntime, PreparedRuntimeConfiguration, RuntimeConfigurationPatch};

impl AgentSession {
    pub fn auto_compaction_enabled(&self) -> bool {
        self.compaction_settings().enabled
    }

    pub fn set_auto_compaction_enabled(&self, enabled: bool) {
        self.compaction_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .enabled = enabled;
    }

    pub fn set_auto_retry_enabled(&self, enabled: bool) {
        self.retry_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .enabled = enabled;
    }

    pub fn steering_mode(&self) -> QueueMode {
        self.runtime.agent().steering_mode()
    }

    pub fn set_steering_mode(&self, mode: QueueMode) {
        self.runtime.agent().set_steering_mode(mode);
    }

    pub fn follow_up_mode(&self) -> QueueMode {
        self.runtime.agent().follow_up_mode()
    }

    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        self.runtime.agent().set_follow_up_mode(mode);
    }

    pub(super) fn compaction_settings(&self) -> CompactionSettings {
        *self
            .compaction_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn retry_settings(&self) -> AutoRetrySettings {
        *self
            .retry_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn set_active_tools(
        &self,
        tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<(), SessionError> {
        let _operation = self.try_begin_operation()?;
        let prepared = self
            .runtime
            .prepare_configuration(RuntimeConfigurationPatch {
                active_tools: Some(tools.into_iter().map(Into::into).collect()),
                ..RuntimeConfigurationPatch::default()
            })?;
        let entry = SessionEntry::ActiveToolsChange(ActiveToolsEntry {
            active_tool_names: prepared.configuration().active_tools.clone(),
        });
        self.commit_configuration(prepared, vec![entry], None)
    }

    pub fn set_model(&self, provider: ProviderId, model_id: ModelId) -> Result<(), SessionError> {
        let _operation = self.try_begin_operation()?;
        let previous_thinking = self.runtime.agent().thinking_level();
        let thinking_level =
            clamp_thinking_for_model(&self.runtime, &provider, &model_id, previous_thinking);
        let prepared = self
            .runtime
            .prepare_configuration(RuntimeConfigurationPatch {
                model: Some(pi_core::ModelSelection {
                    provider: provider.clone(),
                    model_id: model_id.clone(),
                }),
                thinking_level: Some(thinking_level),
                ..RuntimeConfigurationPatch::default()
            })?;
        let thinking_changed = (thinking_level != previous_thinking).then_some(thinking_level);
        let mut entries = vec![SessionEntry::ModelChange(ModelChangeEntry {
            provider,
            model_id,
        })];
        if thinking_changed.is_some() {
            entries.push(SessionEntry::ThinkingLevelChange(ThinkingLevelEntry {
                thinking_level: thinking_level.as_str().to_string(),
            }));
        }
        self.commit_configuration(prepared, entries, thinking_changed)
    }

    pub fn set_thinking_level(&self, thinking_level: ThinkingLevel) -> Result<(), SessionError> {
        let _operation = self.try_begin_operation()?;
        let state = self.runtime.agent().state();
        let thinking_level = clamp_thinking_for_model(
            &self.runtime,
            &state.provider_id,
            &state.model_id,
            thinking_level,
        );
        if thinking_level == state.thinking_level {
            return Ok(());
        }
        let prepared = self
            .runtime
            .prepare_configuration(RuntimeConfigurationPatch {
                thinking_level: Some(thinking_level),
                ..RuntimeConfigurationPatch::default()
            })?;
        let entry = SessionEntry::ThinkingLevelChange(ThinkingLevelEntry {
            thinking_level: thinking_level.as_str().to_string(),
        });
        self.commit_configuration(prepared, vec![entry], Some(thinking_level))
    }

    pub(super) fn commit_configuration(
        &self,
        prepared: PreparedRuntimeConfiguration<'_>,
        entries: Vec<SessionEntry>,
        thinking_level: Option<ThinkingLevel>,
    ) -> Result<(), SessionError> {
        let records = self.store.append_batch_records(entries)?;
        prepared.commit();
        self.events
            .publish_configuration(records, self.runtime.agent().state(), thinking_level);
        Ok(())
    }

    pub async fn set_name(&self, name: Option<String>) -> Result<(), SessionError> {
        let _operation = self.activity.begin(ActivityPhase::Updating).await;
        self.ensure_open()?;
        let normalized = self.set_name_locked(name)?;
        self.plugin_driver()
            .session_info_changed(
                self.session_dispatch_context(),
                &SessionInfoChangedEvent {
                    name: normalized.clone(),
                },
            )
            .await;
        self.events.publish_session_info(normalized);
        Ok(())
    }

    /// Applies the synchronous plugin adapter's name change. The adapter
    /// dispatches the async session-plugin hook after publishing the metadata.
    pub(crate) fn set_name_immediate(
        &self,
        name: Option<String>,
    ) -> Result<Option<String>, SessionError> {
        let _operation = self.try_begin_operation()?;
        let normalized = self.set_name_locked(name)?;
        self.events.publish_session_info(normalized.clone());
        Ok(normalized)
    }

    pub(super) fn set_name_locked(
        &self,
        name: Option<String>,
    ) -> Result<Option<String>, SessionError> {
        let normalized = name.and_then(|name| {
            let mut sanitized = String::with_capacity(name.len());
            let mut newline_run = false;
            for character in name.chars() {
                if matches!(character, '\r' | '\n') {
                    if !newline_run {
                        sanitized.push(' ');
                    }
                    newline_run = true;
                } else {
                    sanitized.push(character);
                    newline_run = false;
                }
            }
            let name = sanitized.trim().to_string();
            (!name.is_empty()).then_some(name)
        });
        self.store.set_name(normalized.clone())?;
        Ok(normalized)
    }
}

pub(super) fn clamp_thinking_for_model(
    runtime: &PiRuntime,
    provider: &ProviderId,
    model_id: &ModelId,
    requested: ThinkingLevel,
) -> ThinkingLevel {
    runtime
        .model(provider, model_id)
        .map_or(requested, |model| model.clamp_thinking_level(requested))
}
