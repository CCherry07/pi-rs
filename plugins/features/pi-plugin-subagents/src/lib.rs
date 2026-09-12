#![forbid(unsafe_code)]

mod catalog;
mod config;
mod desktop;
mod fork_context;
mod launch_context;
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
use crate::tool::{AgentTool, AgentToolKind};

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
        for kind in [
            AgentToolKind::Spawn,
            AgentToolKind::SendMessage,
            AgentToolKind::FollowUp,
            AgentToolKind::Wait,
            AgentToolKind::Interrupt,
            AgentToolKind::List,
        ] {
            context.register_tool(Arc::new(AgentTool::new(
                self.runtime.clone(),
                self.catalog.clone(),
                self.max_depth,
                kind,
            )))?;
        }
        desktop::register_commands(context, &self.runtime)?;
        Ok(())
    }

    async fn before_agent_start(
        &self,
        context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> Result<BeforeAgentStartPatch, PluginError> {
        let session_id = context.session.id()?;
        let active_tools = context.session.active_tools()?;
        self.runtime
            .bind_session(session_id.clone(), context.session.clone());
        let assignment =
            if let Some((agent_id, profile)) = self.runtime.assignment_for_session(&session_id) {
                Some((agent_id, profile))
            } else if let Some(run_id) = marker_from_messages(&event.input_messages) {
                let profile = self
                    .runtime
                    .bind_child(run_id, &session_id)
                    .map_err(|error| PluginError::Hook {
                        plugin_id: PluginId::new("subagents"),
                        hook: "before_agent_start",
                        message: error.to_string(),
                    })?;
                Some((run_id.to_string(), profile))
            } else {
                if !active_tools.iter().any(|tool| tool == "spawn_agent") {
                    return Ok(BeforeAgentStartPatch::default());
                }
                return Ok(BeforeAgentStartPatch {
                    system_prompt: Some(format!("{}\n\n{}", event.system_prompt, PARENT_GUIDANCE)),
                    messages: Vec::new(),
                });
            };
        let (agent_id, profile) = assignment.expect("assigned child");
        let mut prompt = specialized_system_prompt(&event.system_prompt, &profile);
        prompt.push_str("\n\n");
        prompt.push_str(&format!(
            "Collaboration identity: your exact agent id is {agent_id}. Use send_message with target \"parent\" to report information without ending your task. Use wait_agent only when your task genuinely depends on another agent or a parent message."
        ));
        if profile.allow_nested_subagents && active_tools.iter().any(|tool| tool == "spawn_agent") {
            prompt.push_str("\n\n");
            prompt.push_str(PARENT_GUIDANCE);
        }
        Ok(BeforeAgentStartPatch {
            system_prompt: Some(prompt),
            messages: Vec::new(),
        })
    }

    async fn context(
        &self,
        context: AgentPluginContext,
        event: pi_core::ContextEvent,
    ) -> Result<pi_core::ContextPatch, PluginError> {
        let Some((run_id, _)) = self.runtime.assignment_for_session(&context.session.id()?) else {
            return Ok(pi_core::ContextPatch::default());
        };
        Ok(pi_core::ContextPatch {
            messages: Some(fork_context::project_inherited_messages(
                event.messages,
                &run_id,
            )),
        })
    }
}

const PARENT_GUIDANCE: &str = "Agent collaboration: spawn_agent always starts one child asynchronously and returns a stable exact agent id. Launch independent children in the same response for parallel work. Use wait_agent mode:any as a race, or mode:all as a barrier; timeout only yields a snapshot and never cancels work. Re-plan after every result or message. Use send_message for information, followup_task to reuse a child's session, interrupt_agent to stop only its active turn, and list_agents for the current tree. Conditions and loops remain your decisions; there is no workflow or graph language.";

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
