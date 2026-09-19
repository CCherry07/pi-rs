#![forbid(unsafe_code)]

mod catalog;
mod collaboration;
mod config;
mod desktop;
mod fork_context;
mod launch_context;
mod launch_plan;
mod profiles;
mod runtime;
#[cfg(test)]
mod session;
mod skills;
mod tool;

use std::sync::Arc;

use pi_core::{ContentBlock, Message, PluginId};
use pi_plugin::{
    AgentPluginContext, BeforeAgentStartEvent, BeforeAgentStartPatch, Plugin, PluginError,
    RegisterContext,
};

use crate::catalog::SubagentCatalog;
use crate::config::load_max_depth;
use crate::profiles::specialized_system_prompt;
use crate::runtime::run_marker;
use crate::tool::{AgentTool, AgentToolKind};

pub use crate::catalog::{SubagentCatalogError, SubagentLoaderOptions};
pub use crate::runtime::SubagentRuntime;
pub use crate::skills::SubagentSkillPromptProjector;
use pi_plugin::{SessionPluginContext, SessionShutdownEvent};

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

#[pi_plugin::plugin]
impl Plugin for SubagentsPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("subagents")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
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
        event: pi_plugin::ContextEvent,
    ) -> Result<pi_plugin::ContextPatch, PluginError> {
        let session_id = context.session.id()?;
        let messages = if let Some((run_id, _)) = self.runtime.assignment_for_session(&session_id) {
            fork_context::project_inherited_messages(event.messages, &run_id)
        } else {
            event.messages
        };
        Ok(pi_plugin::ContextPatch {
            messages: Some(
                self.runtime
                    .project_collaboration_context(&session_id, messages),
            ),
        })
    }

    async fn session_start(
        &self,
        context: &SessionPluginContext,
        _event: &pi_session::SessionStartEvent,
    ) -> Result<(), PluginError> {
        self.runtime
            .bind_session(context.identity().id.clone(), context.session.clone());
        Ok(())
    }

    async fn session_shutdown(
        &self,
        context: &SessionPluginContext,
        _event: &SessionShutdownEvent,
    ) -> Result<(), PluginError> {
        self.runtime.suspend_desktop(&context.identity().id);
        self.runtime.close_owner(&context.identity().id);
        self.runtime.drain_monitors(&context.identity().id).await;
        self.runtime.forget_session(&context.identity().id);
        Ok(())
    }
}

const PARENT_GUIDANCE: &str = "Agent collaboration: spawn_agent submits one child asynchronously and returns a stable exact agent id; work beyond the root concurrency limit waits in FIFO order. Use fork_turns none, all, or a positive integer string when the child needs fresh, complete, or bounded recent conversation context. By default each bounded task report automatically rejoins your context; set detached only when no automatic report is wanted. Children share the current working directory and filesystem, so assign disjoint write ownership when launching parallel editors. Launch independent children in the same response for parallel work. Child model and thinking are defined by the selected agent profile; an omitted profile setting inherits the current parent selection. Use wait_agent mode:any as a race, or mode:all as a barrier when your next decision depends on completion; timeout only yields a snapshot and never cancels work. Re-plan after every result or message. Use send_message for information, followup_task to reuse a child's session, interrupt_agent to stop only its active turn, and list_agents for the current tree. Conditions and loops remain your decisions; there is no workflow or graph language.";

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
