use pi_core::{BeforeAgentStartEvent, ContentBlock, Message};
use pi_plugin_skills::{
    SkillCatalog, SkillPromptProjection, SkillPromptProjector, SkillPromptSelection,
};

use crate::runtime::{SubagentRuntime, run_marker};

/// Projects the normal generation-local skill catalog for one delegated run.
///
/// The Adapter reads only feature-owned run metadata. Skill discovery,
/// collision handling, private-path loading, and prompt formatting remain in
/// `pi-plugin-skills`.
#[derive(Clone)]
pub struct SubagentSkillPromptProjector {
    runtime: SubagentRuntime,
}

impl SubagentSkillPromptProjector {
    pub fn new(runtime: SubagentRuntime) -> Self {
        Self { runtime }
    }
}

impl SkillPromptProjector for SubagentSkillPromptProjector {
    fn project(
        &self,
        event: &BeforeAgentStartEvent,
        catalog: &SkillCatalog,
    ) -> SkillPromptProjection {
        let Some(run_id) = marker_from_messages(&event.input_messages) else {
            return catalog.all_for_prompt();
        };
        let Some(profile) = self.runtime.profile_for_run(run_id) else {
            // A marker denotes a narrow child. If its feature-owned launch
            // record is unavailable, fail closed instead of exposing the full
            // parent skill catalog.
            return SkillPromptProjection::default();
        };
        let additional_paths = if profile.skills.is_empty() {
            Vec::new()
        } else {
            profile.skill_paths.clone()
        };
        let projection = catalog.project(&SkillPromptSelection {
            inherit: profile.inherit_skills,
            names: profile.skills,
            additional_paths,
        });
        self.runtime
            .record_warnings(run_id, projection.warnings().to_vec());
        projection
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
