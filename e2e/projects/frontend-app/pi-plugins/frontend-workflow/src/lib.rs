use std::sync::Arc;

use pi_plugin_sdk::agent::prelude::*;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Options {
    command: String,
    guidance: String,
    checks: Vec<String>,
    review_paths: Vec<String>,
    ignore_paths: Vec<String>,
}

struct FrontendWorkflowPlugin {
    options: Options,
}

impl NativePluginFactory for FrontendWorkflowPlugin {
    type Options = Options;

    fn load(_context: &PluginLoadContext, options: Self::Options) -> PluginLoadResult<Self> {
        if !valid_command_name(&options.command) {
            return Err(PluginLoadError::initialization(
                "options.command must contain only lowercase ASCII letters, digits, and '-'",
            ));
        }
        if options.checks.is_empty() || options.checks.iter().any(|check| check.trim().is_empty()) {
            return Err(PluginLoadError::initialization(
                "options.checks must contain at least one non-empty command",
            ));
        }
        if options.review_paths.is_empty()
            || options
                .review_paths
                .iter()
                .chain(&options.ignore_paths)
                .any(|path| path.trim().is_empty())
        {
            return Err(PluginLoadError::initialization(
                "options.review_paths must be non-empty and configured paths cannot be blank",
            ));
        }
        Ok(Self { options })
    }
}

#[pi_plugin_sdk::agent(factory)]
impl AgentPlugin for FrontendWorkflowPlugin {
    fn register(&self, context: &mut RegisterContext<'_>) -> Result<()> {
        context.register_command(Arc::new(FrontendCheckCommand {
            name: self.options.command.clone(),
            checks: self.options.checks.clone(),
            review_paths: self.options.review_paths.clone(),
            ignore_paths: self.options.ignore_paths.clone(),
        }))
    }

    async fn before_agent_start(
        &self,
        context: AgentPluginContext,
        event: BeforeAgentStartEvent,
    ) -> std::result::Result<BeforeAgentStartPatch, PluginError> {
        let _mode = context.ui.mode()?;
        let _trusted = context.session.is_project_trusted()?;
        let _session_id = context.session.id()?;
        let _model = context.models.current()?;
        let checks = bullet_list(&self.options.checks);
        Ok(BeforeAgentStartPatch {
            system_prompt: Some(format!(
                "{}\n\nFrontend native plugin:\n{}\nRequired checks:\n{}",
                event.system_prompt, self.options.guidance, checks
            )),
            messages: Vec::new(),
        })
    }
}

struct FrontendCheckCommand {
    name: String,
    checks: Vec<String>,
    review_paths: Vec<String>,
    ignore_paths: Vec<String>,
}

#[async_trait]
impl Command for FrontendCheckCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: self.name.clone(),
            description: "Verify this frontend before considering the task complete".to_string(),
            argument_hint: Some("[focus]".to_string()),
        }
    }

    async fn execute(
        &self,
        _context: CommandContext,
        arguments: String,
    ) -> std::result::Result<CommandOutcome, CommandError> {
        Ok(CommandOutcome::TransformInput(review_request(
            arguments.trim(),
            &self.checks,
            &self.review_paths,
            &self.ignore_paths,
        )))
    }
}

fn valid_command_name(command: &str) -> bool {
    !command.is_empty()
        && command
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn bullet_list(items: &[String]) -> String {
    items
        .iter()
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn review_request(
    focus: &str,
    checks: &[String],
    review_paths: &[String],
    ignore_paths: &[String],
) -> String {
    let focus = if focus.is_empty() {
        String::new()
    } else {
        format!("\nPay particular attention to: {focus}.")
    };
    let ignored = if ignore_paths.is_empty() {
        String::new()
    } else {
        format!(
            "\nDo not inspect or review these paths:\n{}",
            bullet_list(ignore_paths)
        )
    };
    format!(
        "Review the frontend for correctness, accessibility, and regressions.{focus}\
         \nOnly inspect and review these paths:\n{}\
         {ignored}\nRun these checks and report every failure clearly:\n{}",
        bullet_list(review_paths),
        bullet_list(checks)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_validation_matches_the_registered_slash_command_contract() {
        assert!(valid_command_name("frontend-check"));
        assert!(!valid_command_name("Frontend Check"));
        assert!(!valid_command_name(""));
    }

    #[test]
    fn verification_prompt_preserves_check_order() {
        assert_eq!(
            bullet_list(&["npm run lint".to_string(), "npm run build".to_string()]),
            "- npm run lint\n- npm run build"
        );
    }

    #[test]
    fn review_request_scopes_the_agent_away_from_plugin_sources() {
        let request = review_request(
            "accessibility",
            &["npm run lint".to_string()],
            &["src/".to_string(), "package.json".to_string()],
            &[".pi/".to_string()],
        );

        assert!(request.contains("Pay particular attention to: accessibility"));
        assert!(request.contains("Only inspect and review these paths:\n- src/\n- package.json"));
        assert!(request.contains("Do not inspect or review these paths:\n- .pi/"));
    }
}
