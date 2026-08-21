use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::{
    AgentPlugin, Command, CoreError, ModelId, ModelRuntime, ModelSpec, PluginDriver, PluginId,
    Provider, ProviderId, ProviderPlugin, ProviderPluginDriver, Result, Tool,
};

#[derive(Default)]
pub struct RegistriesBuilder {
    tools: HashMap<String, (PluginId, Arc<dyn Tool>)>,
    commands: HashMap<String, (PluginId, Arc<dyn Command>)>,
    providers: HashMap<ProviderId, (PluginId, Arc<dyn Provider>)>,
    provider_overrides: HashSet<ProviderId>,
    models: HashMap<(ProviderId, ModelId), (PluginId, ModelSpec)>,
}

impl RegistriesBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_tool(&mut self, owner: PluginId, tool: Arc<dyn Tool>) -> Result<()> {
        let name = tool.spec().name;
        if self.tools.contains_key(&name) {
            return Err(CoreError::DuplicateTool(name));
        }
        self.tools.insert(name, (owner, tool));
        Ok(())
    }

    pub fn register_command(&mut self, owner: PluginId, command: Arc<dyn Command>) -> Result<()> {
        let name = command.spec().name;
        if self.commands.contains_key(&name) {
            return Err(CoreError::DuplicateCommand(name));
        }
        self.commands.insert(name, (owner, command));
        Ok(())
    }

    pub(crate) fn register_provider(
        &mut self,
        owner: PluginId,
        provider: Arc<dyn Provider>,
    ) -> Result<()> {
        let id = provider.id();
        if self.providers.contains_key(&id) {
            return Err(CoreError::DuplicateProvider(id.to_string()));
        }
        self.providers.insert(id, (owner, provider));
        Ok(())
    }

    pub(crate) fn provider(&self, id: &ProviderId) -> Option<Arc<dyn Provider>> {
        self.providers
            .get(id)
            .map(|(_, provider)| Arc::clone(provider))
    }

    pub(crate) fn register_provider_override(
        &mut self,
        owner: PluginId,
        provider: Arc<dyn Provider>,
    ) -> Result<()> {
        let id = provider.id();
        if !self.provider_overrides.insert(id.clone()) {
            return Err(CoreError::DuplicateProviderOverride(id.to_string()));
        }
        self.providers.insert(id, (owner, provider));
        Ok(())
    }

    pub(crate) fn register_model(&mut self, owner: PluginId, model: ModelSpec) -> Result<()> {
        if !self.providers.contains_key(&model.provider) {
            return Err(CoreError::ModelProviderNotFound {
                provider: model.provider.to_string(),
                model: model.id.to_string(),
            });
        }
        let key = (model.provider.clone(), model.id.clone());
        if self.models.contains_key(&key) {
            return Err(CoreError::DuplicateModel(format!(
                "{}/{}",
                model.provider, model.id
            )));
        }
        self.models.insert(key, (owner, model));
        Ok(())
    }

    pub fn register_plugins(
        mut self,
        plugins: Vec<Arc<dyn AgentPlugin>>,
    ) -> Result<(PluginDriver, FrozenRegistries)> {
        let driver = PluginDriver::new(plugins)?;
        driver.register_all(&mut self)?;
        Ok((driver, self.freeze()))
    }

    pub fn register_plugin_sets(
        mut self,
        plugins: Vec<Arc<dyn AgentPlugin>>,
        provider_plugins: Vec<Arc<dyn ProviderPlugin>>,
    ) -> Result<(PluginDriver, ProviderPluginDriver, FrozenRegistries)> {
        let plugin_driver = PluginDriver::new(plugins)?;
        plugin_driver.register_all(&mut self)?;
        let provider_plugin_driver = ProviderPluginDriver::new(provider_plugins)?;
        provider_plugin_driver.register_all(&mut self)?;
        Ok((plugin_driver, provider_plugin_driver, self.freeze()))
    }

    pub fn freeze(self) -> FrozenRegistries {
        FrozenRegistries {
            tools: self.tools,
            commands: self.commands,
            models: ModelRuntime::new(self.providers, self.models),
        }
    }
}

pub struct FrozenRegistries {
    tools: HashMap<String, (PluginId, Arc<dyn Tool>)>,
    commands: HashMap<String, (PluginId, Arc<dyn Command>)>,
    models: ModelRuntime,
}

impl FrozenRegistries {
    pub fn tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).map(|(_, tool)| Arc::clone(tool))
    }

    pub fn command(&self, name: &str) -> Option<Arc<dyn Command>> {
        self.commands
            .get(name)
            .map(|(_, command)| Arc::clone(command))
    }

    pub fn command_specs(&self) -> Vec<crate::CommandSpec> {
        let mut specs = self
            .commands
            .values()
            .map(|(_, command)| command.spec())
            .collect::<Vec<_>>();
        specs.sort_by(|left, right| left.name.cmp(&right.name));
        specs
    }

    pub fn provider(&self, id: &ProviderId) -> Option<Arc<dyn Provider>> {
        self.models.provider(id)
    }

    pub fn has_providers(&self) -> bool {
        self.models.has_providers()
    }

    pub fn model_runtime(&self) -> &ModelRuntime {
        &self.models
    }

    pub fn model(&self, provider: &ProviderId, id: &ModelId) -> Option<&ModelSpec> {
        self.models.model(provider, id)
    }

    pub fn model_specs(&self) -> Vec<ModelSpec> {
        self.models.models()
    }

    pub fn tool_specs(&self, active: &[String]) -> Result<Vec<crate::ToolSpec>> {
        active
            .iter()
            .map(|name| {
                self.tool(name)
                    .map(|tool| tool.spec())
                    .ok_or_else(|| CoreError::ToolNotFound(name.clone()))
            })
            .collect()
    }

    pub fn tool_owner(&self, name: &str) -> Option<&PluginId> {
        self.tools.get(name).map(|(owner, _)| owner)
    }

    pub fn command_owner(&self, name: &str) -> Option<&PluginId> {
        self.commands.get(name).map(|(owner, _)| owner)
    }

    pub fn provider_owner(&self, id: &ProviderId) -> Option<&PluginId> {
        self.models.provider_owner(id)
    }

    pub fn model_owner(&self, provider: &ProviderId, id: &ModelId) -> Option<&PluginId> {
        self.models.model_owner(provider, id)
    }
}
