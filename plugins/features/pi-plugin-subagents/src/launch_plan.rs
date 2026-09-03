use std::collections::HashSet;

use pi_core::{IsolatedSessionOptions, ModelSelection, ModelSpec, ToolContext, ToolError};

use crate::profiles::SubagentProfile;

/// Resolved child runtime policy ready for the generic isolated-session seam.
pub(crate) struct SubagentLaunchPlan {
    options: IsolatedSessionOptions,
}

impl SubagentLaunchPlan {
    pub(crate) fn resolve(
        profile: &SubagentProfile,
        context: &ToolContext,
    ) -> Result<Self, ToolError> {
        let active_tools = resolve_tools(profile, context)?;
        let current_selection = if profile.model.is_some() {
            context.models.selection()?
        } else {
            None
        };
        let current_model = if profile.model.is_none() && profile.thinking_level.is_some() {
            context.models.current()?
        } else {
            None
        };
        let (model, selected_model) = resolve_model(
            profile,
            current_selection
                .as_ref()
                .map(|selection| &selection.provider),
            context,
        )?;
        validate_thinking(profile, selected_model.as_ref().or(current_model.as_ref()))?;
        Ok(Self {
            options: IsolatedSessionOptions {
                active_tools: Some(active_tools),
                model,
                thinking_level: profile.thinking_level,
            },
        })
    }

    pub(crate) fn into_options(self) -> IsolatedSessionOptions {
        self.options
    }
}

fn resolve_tools(
    profile: &SubagentProfile,
    context: &ToolContext,
) -> Result<Vec<String>, ToolError> {
    resolve_tool_names(profile, context.session.active_tools()?)
}

fn resolve_tool_names(
    profile: &SubagentProfile,
    parent_tools: Vec<String>,
) -> Result<Vec<String>, ToolError> {
    let ceiling = parent_tools.iter().cloned().collect::<HashSet<_>>();
    let mut selected = match &profile.tools {
        None => parent_tools,
        Some(requested) => requested.clone(),
    };
    let excluded = profile
        .excluded_tools
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    selected.retain(|tool| !excluded.contains(tool.as_str()));

    let unavailable = selected
        .iter()
        .filter(|tool| !ceiling.contains(tool.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unavailable.is_empty() {
        return Err(ToolError::Execution(format!(
            "subagent profile {:?} requests tools outside the calling session capability ceiling: {}",
            profile.name,
            unavailable.join(", ")
        )));
    }
    if !profile.allow_nested_subagents {
        if profile.tools.is_some() && selected.iter().any(|tool| tool == "subagent") {
            return Err(ToolError::Execution(format!(
                "subagent profile {:?} selects the subagent tool but does not authorize nested delegation",
                profile.name
            )));
        }
        selected.retain(|tool| tool != "subagent");
    }
    if (profile.inherit_skills || !profile.skills.is_empty())
        && !selected.iter().any(|tool| tool == "read")
    {
        if excluded.contains("read") {
            return Err(ToolError::Execution(format!(
                "subagent profile {:?} enables skills but excludes the read tool required to load them",
                profile.name
            )));
        }
        if !ceiling.contains("read") {
            return Err(ToolError::Execution(format!(
                "subagent profile {:?} enables skills and requires the read tool, but the calling session capability ceiling does not provide it",
                profile.name
            )));
        }
        selected.push("read".to_string());
    }
    Ok(selected)
}

fn resolve_model(
    profile: &SubagentProfile,
    preferred_provider: Option<&pi_core::ProviderId>,
    context: &ToolContext,
) -> Result<(Option<ModelSelection>, Option<ModelSpec>), ToolError> {
    let Some(reference) = profile.model.as_deref() else {
        return Ok((None, None));
    };
    let available = context.models.available()?;
    let model = resolve_model_reference(reference, preferred_provider, &available)
        .map_err(ToolError::Execution)?;
    Ok((
        Some(ModelSelection {
            provider: model.provider.clone(),
            model_id: model.id.clone(),
        }),
        Some(model),
    ))
}

fn resolve_model_reference(
    reference: &str,
    preferred_provider: Option<&pi_core::ProviderId>,
    available: &[ModelSpec],
) -> Result<ModelSpec, String> {
    if available.is_empty() {
        return Err(format!(
            "subagent model {reference:?} cannot be resolved because no models are available"
        ));
    }
    let reference = reference.trim();
    if let Some((provider, model_id)) = reference.split_once('/')
        && available
            .iter()
            .any(|model| model.provider.as_str().eq_ignore_ascii_case(provider))
    {
        return available
            .iter()
            .find(|model| {
                model.provider.as_str().eq_ignore_ascii_case(provider)
                    && model.id.as_str().eq_ignore_ascii_case(model_id)
            })
            .cloned()
            .ok_or_else(|| format!("subagent model {reference:?} is not available"));
    }

    let matches = available
        .iter()
        .filter(|model| {
            model.id.as_str().eq_ignore_ascii_case(reference)
                || model.name.eq_ignore_ascii_case(reference)
                || format!("{}/{}", model.provider, model.id).eq_ignore_ascii_case(reference)
        })
        .collect::<Vec<_>>();
    if let Some(preferred_provider) = preferred_provider {
        let preferred = matches
            .iter()
            .filter(|model| model.provider == *preferred_provider)
            .collect::<Vec<_>>();
        if let [model] = preferred.as_slice() {
            return Ok((**model).clone());
        }
    }
    match matches.as_slice() {
        [model] => Ok((*model).clone()),
        [] => Err(format!("subagent model {reference:?} is not available")),
        models => Err(format!(
            "subagent model {reference:?} is ambiguous; use provider/model ({})",
            models
                .iter()
                .map(|model| format!("{}/{}", model.provider, model.id))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn validate_thinking(
    profile: &SubagentProfile,
    model: Option<&ModelSpec>,
) -> Result<(), ToolError> {
    let (Some(level), Some(model)) = (profile.thinking_level, model) else {
        return Ok(());
    };
    if model.supports_thinking_level(level) {
        Ok(())
    } else {
        Err(ToolError::Execution(format!(
            "subagent profile {:?} requests thinking level {} unsupported by {}/{}",
            profile.name,
            level.as_str(),
            model.provider,
            model.id
        )))
    }
}

#[cfg(test)]
mod tests {
    use pi_core::{ModelId, ProviderId, ThinkingLevel};

    use super::*;
    use crate::profiles::builtin_profile;

    fn model(provider: &str, id: &str) -> ModelSpec {
        ModelSpec::new(provider, id, id, "test")
    }

    #[test]
    fn bare_model_prefers_the_current_provider_then_requires_uniqueness() {
        let models = [model("one", "shared"), model("two", "shared")];
        let selected =
            resolve_model_reference("shared", Some(&ProviderId::new("two")), &models).unwrap();
        assert_eq!(selected.provider, ProviderId::new("two"));
        assert!(resolve_model_reference("shared", None, &models).is_err());
        assert_eq!(
            resolve_model_reference("one/shared", None, &models)
                .unwrap()
                .id,
            ModelId::new("shared")
        );
    }

    #[test]
    fn unknown_qualified_model_never_switches_provider() {
        let models = [model("one", "known"), model("two", "other")];
        assert!(resolve_model_reference("one/other", None, &models).is_err());
    }

    #[test]
    fn incompatible_thinking_fails_instead_of_being_silently_clamped() {
        let mut profile = builtin_profile("scout");
        profile.thinking_level = Some(ThinkingLevel::High);
        assert!(validate_thinking(&profile, Some(&model("one", "plain"))).is_err());

        let mut reasoning = model("one", "reasoning");
        reasoning.reasoning = true;
        profile.thinking_level = Some(ThinkingLevel::Max);
        assert!(validate_thinking(&profile, Some(&reasoning)).is_err());
        reasoning
            .thinking_level_map
            .insert("max".to_string(), Some("max".to_string()));
        assert!(validate_thinking(&profile, Some(&reasoning)).is_ok());
    }

    #[test]
    fn excluded_tools_narrow_inherited_and_explicit_tool_sets() {
        let mut inherited = builtin_profile("scout");
        inherited.excluded_tools = vec!["bash".into(), "missing".into()];
        assert_eq!(
            resolve_tool_names(
                &inherited,
                ["read", "bash", "grep", "subagent"]
                    .map(str::to_string)
                    .to_vec(),
            )
            .unwrap(),
            ["read", "grep", "subagent"].map(str::to_string).to_vec()
        );

        let mut explicit = builtin_profile("scout");
        explicit.tools = Some(["read", "bash"].map(str::to_string).to_vec());
        explicit.excluded_tools = vec!["bash".into()];
        assert_eq!(
            resolve_tool_names(
                &explicit,
                ["read", "bash", "grep"].map(str::to_string).to_vec(),
            )
            .unwrap(),
            vec!["read".to_string()]
        );
    }

    #[test]
    fn explicitly_selected_skills_receive_read_without_widening_the_parent_ceiling() {
        let mut profile = builtin_profile("scout");
        profile.tools = Some(vec!["grep".into()]);
        profile.skills = vec!["review-checklist".into()];
        assert_eq!(
            resolve_tool_names(&profile, ["read", "grep"].map(str::to_string).to_vec()).unwrap(),
            ["grep", "read"].map(str::to_string).to_vec()
        );

        let error = resolve_tool_names(&profile, vec!["grep".into()]).unwrap_err();
        assert!(error.to_string().contains("requires the read tool"));

        profile.excluded_tools = vec!["read".into()];
        let error = resolve_tool_names(&profile, ["read", "grep"].map(str::to_string).to_vec())
            .unwrap_err();
        assert!(error.to_string().contains("excludes the read tool"));

        let mut inherited = builtin_profile("scout");
        inherited.tools = Some(vec!["grep".into()]);
        inherited.inherit_skills = true;
        assert_eq!(
            resolve_tool_names(&inherited, ["read", "grep"].map(str::to_string).to_vec()).unwrap(),
            ["grep", "read"].map(str::to_string).to_vec()
        );
    }
}
