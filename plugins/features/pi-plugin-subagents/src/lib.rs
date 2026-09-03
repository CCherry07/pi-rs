#![forbid(unsafe_code)]

mod catalog;
mod config;
mod launch_plan;
mod profiles;
mod runtime;
mod session;
mod skills;
mod tool;

use std::sync::Arc;

use pi_core::{
    AgentPlugin, AgentPluginContext, BeforeAgentStartEvent, BeforeAgentStartPatch, ContentBlock,
    Message, PluginError, PluginId, RegisterContext,
};

use crate::catalog::SubagentCatalog;
use crate::config::load_max_depth;
use crate::profiles::specialized_system_prompt;
use crate::runtime::run_marker;
use crate::tool::SubagentTool;

pub use crate::catalog::{SubagentCatalogError, SubagentLoaderOptions};
pub use crate::runtime::SubagentRuntime;
pub use crate::session::SubagentsSessionPlugin;
pub use crate::skills::SubagentSkillPromptProjector;

/// First-party delegation policy layered over the product's generic isolated
/// session capability.
///
/// The shared runtime owns recursive lineage and budgets. Each runtime
/// generation gets a fresh plugin instance backed by that same state.
pub struct SubagentsPlugin {
    runtime: SubagentRuntime,
    catalog: SubagentCatalog,
    max_depth: usize,
}

impl SubagentsPlugin {
    pub fn new(runtime: SubagentRuntime) -> Self {
        let max_depth = runtime.default_max_depth();
        Self {
            runtime,
            catalog: SubagentCatalog::builtins(),
            max_depth,
        }
    }

    pub fn load(
        runtime: SubagentRuntime,
        options: SubagentLoaderOptions,
    ) -> Result<Self, SubagentCatalogError> {
        let max_depth = load_max_depth(&options)?;
        Ok(Self {
            runtime,
            catalog: SubagentCatalog::load(&options)?,
            max_depth,
        })
    }
}

impl Default for SubagentsPlugin {
    fn default() -> Self {
        Self::new(SubagentRuntime::default())
    }
}

#[pi_core::agent_plugin]
impl AgentPlugin for SubagentsPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("subagents")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        context.register_tool(Arc::new(SubagentTool::new(
            self.runtime.clone(),
            self.catalog.clone(),
            self.max_depth,
        )))
    }

    async fn before_agent_start(
        &self,
        context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        let Some(run_id) = marker_from_messages(&event.input_messages) else {
            return Ok(BeforeAgentStartPatch::default());
        };
        let session_id = context.session.id()?;
        let assignment = self
            .runtime
            .bind_child(run_id, &session_id)
            .map_err(|error| PluginError::Hook {
                plugin_id: PluginId::new("subagents"),
                hook: "before_agent_start",
                message: error.to_string(),
            })?;
        Ok(BeforeAgentStartPatch {
            system_prompt: Some(specialized_system_prompt(
                &event.system_prompt,
                &assignment.profile,
                assignment.depth,
                assignment.max_depth,
            )),
            messages: Vec::new(),
        })
    }
}

fn marker_from_messages(messages: &[Message]) -> Option<&str> {
    messages.iter().find_map(|message| {
        let Message::User(message) = message else {
            return None;
        };
        message.content.iter().find_map(|content| {
            let ContentBlock::Text(text) = content else {
                return None;
            };
            run_marker(&text.text)
        })
    })
}

#[cfg(test)]
mod tests {
    use pi_core::{TextContent, UserMessage};

    use super::*;

    #[test]
    fn marker_discovery_ignores_ordinary_parent_messages() {
        let messages = [Message::User(UserMessage {
            content: vec![ContentBlock::Text(TextContent::new("ordinary task"))],
            timestamp_ms: 0,
        })];
        assert_eq!(marker_from_messages(&messages), None);
    }
}
