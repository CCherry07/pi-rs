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
    pub fn update_setting(&self, setting: crate::SessionSetting) -> Result<(), SessionError> {
        self.ensure_open()?;
        if let Some(writer) = &self.settings_writer {
            writer.write(setting).map_err(SessionError::Runtime)?;
        }
        match setting {
            crate::SessionSetting::SteeringMode(mode) => self.set_steering_mode(mode),
            crate::SessionSetting::FollowUpMode(mode) => self.set_follow_up_mode(mode),
            crate::SessionSetting::AutoCompaction(enabled) => {
                self.set_auto_compaction_enabled(enabled)
            }
            crate::SessionSetting::AutoRetry(enabled) => self.set_auto_retry_enabled(enabled),
        }
        Ok(())
    }
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

    pub(super) fn resolved_compaction_settings_for(
        &self,
        provider: &ProviderId,
        model: &ModelId,
    ) -> Result<CompactionSettings, SessionError> {
        let base = self.compaction_settings();
        self.compaction_settings_resolver
            .as_ref()
            .map_or(Ok(base), |resolver| resolver.resolve(base, provider, model))
    }

    pub(super) fn retry_settings(&self) -> AutoRetrySettings {
        *self
            .retry_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The current branch's durable desired selection, including exact names
    /// retained while temporarily absent. This does not declare or register tools.
    /// An explicit empty selection stays empty; only a missing selection falls
    /// back to the generation's effective runtime defaults. Reading is passive.
    pub fn desired_active_tools(&self) -> Result<Vec<String>, SessionError> {
        Ok(self
            .store
            .selected_active_tools()?
            .unwrap_or_else(|| self.runtime.active_tools()))
    }

    /// Pi plugin setters ignore unrelated unknown names, unlike the strict Rust
    /// setter. Exact policy-allowed absent names still carry explicit intent.
    pub(crate) fn set_active_tools_from_plugin(
        &self,
        tools: Vec<String>,
    ) -> Result<(), SessionError> {
        self.set_active_tools(self.activity.tool_selection.plugin_request(tools))
    }

    pub fn set_active_tools(
        &self,
        tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<(), SessionError> {
        let _operation = self.try_begin_operation()?;
        let desired: Vec<String> = tools.into_iter().map(Into::into).collect();
        let prepared = self
            .runtime
            .prepare_configuration(RuntimeConfigurationPatch {
                active_tools: Some(self.activity.tool_selection.explicit_effective(&desired)?),
                ..RuntimeConfigurationPatch::default()
            })?;
        // Validate executable names and render before committing either view.
        // Unlike passive capture, an explicit selection replaces all intent.
        let entry = SessionEntry::ActiveToolsChange(ActiveToolsEntry {
            active_tool_names: self.activity.tool_selection.reconcile(desired),
        });
        self.commit_configuration(prepared, vec![entry], None)
    }

    pub fn set_model(&self, provider: ProviderId, model_id: ModelId) -> Result<(), SessionError> {
        self.set_model_with_thinking(provider, model_id, None)
    }

    pub fn scoped_models(&self) -> Vec<pi_plugin::ScopedModel> {
        self.scoped_models.clone()
    }

    /// Cycles the product's scoped catalog, or all available models when no scope is configured.
    pub fn cycle_model(&self) -> Result<Option<crate::ModelCycleResult>, SessionError> {
        let available = self.runtime.available_models();
        let is_scoped = !self.scoped_models.is_empty();
        let choices: Vec<_> = if is_scoped {
            self.scoped_models
                .iter()
                .filter(|scoped| {
                    available.iter().any(|model| {
                        model.provider == scoped.model.provider && model.id == scoped.model.id
                    })
                })
                .cloned()
                .collect()
        } else {
            available
                .into_iter()
                .map(|model| pi_plugin::ScopedModel {
                    model,
                    thinking_level: None,
                })
                .collect()
        };
        let state = self.runtime.agent().state();
        let current = choices.iter().position(|choice| {
            choice.model.provider == state.provider_id && choice.model.id == state.model_id
        });
        if choices.is_empty() || (choices.len() == 1 && current.is_some()) {
            return Ok(None);
        }
        let next = &choices[current.map_or(0, |index| (index + 1) % choices.len())];
        self.set_model_with_thinking(
            next.model.provider.clone(),
            next.model.id.clone(),
            next.thinking_level,
        )?;
        Ok(Some(crate::ModelCycleResult {
            model: next.model.clone(),
            thinking_level: self.runtime.agent().thinking_level(),
            is_scoped,
        }))
    }

    fn set_model_with_thinking(
        &self,
        provider: ProviderId,
        model_id: ModelId,
        explicit: Option<ThinkingLevel>,
    ) -> Result<(), SessionError> {
        self.ensure_open()?;
        let _operation = if self.activity.accepts_live_messages() {
            None
        } else {
            Some(self.try_begin_operation()?)
        };
        let previous_thinking = self.runtime.agent().thinking_level();
        let requested_thinking = explicit
            .or_else(|| {
                self.model_switch
                    .model_thinking_levels
                    .get(&format!("{provider}/{model_id}"))
                    .copied()
            })
            .or(self.model_switch.default_thinking_level)
            .unwrap_or(previous_thinking);
        let thinking_level =
            clamp_thinking_for_model(&self.runtime, &provider, &model_id, requested_thinking);
        let prepared = self.runtime.prepare_model_configuration(
            Some(pi_core::ModelSelection {
                provider: provider.clone(),
                model_id: model_id.clone(),
            }),
            Some(thinking_level),
        )?;
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
        self.commit_configuration(prepared, entries, thinking_changed)?;
        if thinking_changed.is_some() {
            self.emit_thinking_level_select(thinking_level, previous_thinking);
        }
        Ok(())
    }

    pub fn set_thinking_level(&self, thinking_level: ThinkingLevel) -> Result<(), SessionError> {
        self.ensure_open()?;
        let _operation = if self.activity.accepts_live_messages() {
            None
        } else {
            Some(self.try_begin_operation()?)
        };
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
            .prepare_model_configuration(None, Some(thinking_level))?;
        let entry = SessionEntry::ThinkingLevelChange(ThinkingLevelEntry {
            thinking_level: thinking_level.as_str().to_string(),
        });
        self.commit_configuration(prepared, vec![entry], Some(thinking_level))?;
        self.emit_thinking_level_select(thinking_level, state.thinking_level);
        Ok(())
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

    /// Updates the product name immediately and dispatches its observer without delaying the caller.
    pub fn set_session_name(&self, name: Option<String>) -> Result<(), SessionError> {
        self.ensure_open()?;
        let _operation = if self.activity.accepts_live_messages() {
            None
        } else {
            Some(self.try_begin_operation()?)
        };
        let normalized = self.set_name_locked(name)?;
        self.events.publish_session_info(normalized.clone());
        let driver = std::sync::Arc::clone(&self.plugin_driver);
        let context = self.session_dispatch_context().clone();
        self.executor.spawn(async move {
            driver
                .session_info_changed(&context, &SessionInfoChangedEvent { name: normalized })
                .await;
        });
        Ok(())
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
        let entry = self.store.set_name_with_entry(normalized.clone())?;
        self.events
            .publish_entry(entry, crate::EntryAppendOrigin::Configuration);
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
