//! Shared built-in declaration contracts from legacy builtin-tool-strict-mode.
//! Test the native Tool/registry/provider boundary rather than JS wrapper identity.
use std::{process::Command, sync::Arc};

use async_trait::async_trait;
use pi_agent::{Agent, AgentOptions, AgentRuntime};
use pi_core::{
    AbortHandle, ConstrainedSampling, PluginId, ToolCallId, ToolDeclaration, ToolResult,
    ToolSchemaStrict, ToolSpec,
};
use pi_plugin::{
    Plugin, RegisterContext, RegistriesBuilder, Tool, ToolContext, ToolError, ToolUpdateSink,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};

fn strict_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(pi_plugin_read::ReadTool),
        Arc::new(pi_plugin_bash::BashTool),
        Arc::new(pi_plugin_powershell::PowerShellTool),
        Arc::new(pi_plugin_edit::EditTool),
        Arc::new(pi_plugin_write::WriteTool),
    ]
}

#[test]
fn builtin_strict_sampling_is_independent_of_the_experimental_environment() {
    const CHILD: &str = "PI_TOOL_PARITY_STRICT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        for experimental in [None, Some("0"), Some("1")] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .arg("--exact")
                .arg("builtin_strict_sampling_is_independent_of_the_experimental_environment")
                .env(CHILD, "1")
                .env_remove("PI_EXPERIMENTAL");
            if let Some(value) = experimental {
                command.env("PI_EXPERIMENTAL", value);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "PI_EXPERIMENTAL={experimental:?}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    }
    for tool in strict_tools() {
        let spec = tool.spec();
        assert_eq!(
            spec.constrained_sampling,
            Some(ConstrainedSampling::JsonSchema {
                strict: ToolSchemaStrict::Prefer,
            }),
            "{}",
            spec.name
        );
        assert_eq!(
            ToolDeclaration::from(&spec).constrained_sampling,
            spec.constrained_sampling
        );
    }
    for spec in [
        pi_plugin_grep::GrepTool.spec(),
        pi_plugin_find::FindTool.spec(),
        pi_plugin_ls::LsTool.spec(),
    ] {
        assert_eq!(spec.constrained_sampling, None, "{}", spec.name);
    }
    assert_eq!(
        pi_plugin_read::ReadTool.spec().parameters["required"],
        json!(["path"])
    );
    assert_eq!(
        pi_plugin_bash::BashTool.spec().parameters["required"],
        json!(["command"])
    );
}

struct NonStrictTool(Arc<dyn Tool>);

#[async_trait]
impl Tool for NonStrictTool {
    fn spec(&self) -> ToolSpec {
        let mut spec = self.0.spec();
        spec.constrained_sampling = Some(ConstrainedSampling::Disabled);
        spec
    }

    async fn prepare_arguments(
        &self,
        context: &ToolContext,
        args: Value,
    ) -> Result<Value, ToolError> {
        self.0.prepare_arguments(context, args).await
    }

    fn validate_arguments(&self, args: &Value) -> Result<(), ToolError> {
        self.0.validate_arguments(args)
    }

    async fn execute(
        &self,
        context: ToolContext,
        id: ToolCallId,
        args: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        self.0.execute(context, id, args, updates).await
    }
}

struct NonStrictPlugin;

#[pi_plugin::plugin]
impl Plugin for NonStrictPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("non-strict-builtins")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        for tool in strict_tools() {
            context.register_tool(Arc::new(NonStrictTool(tool)))?;
        }
        Ok(())
    }
}

#[tokio::test]
async fn explicit_sampling_opt_out_preserves_tool_contract_and_execution() {
    for original in strict_tools() {
        let original_spec = original.spec();
        let tool = NonStrictTool(original.clone());
        let mut expected = original_spec.clone();
        expected.constrained_sampling = Some(ConstrainedSampling::Disabled);
        assert_eq!(tool.spec(), expected);
        assert_eq!(original.spec(), original_spec);
        assert_eq!(
            serde_json::to_value(ToolDeclaration::from(&tool.spec())).unwrap()["constrainedSampling"],
            false
        );
    }
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("sample.txt"), "still works").unwrap();
    let (_, signal) = AbortHandle::new();
    let context = ToolContext::standalone(directory.path().into(), signal);
    let edit = NonStrictTool(Arc::new(pi_plugin_edit::EditTool));
    let arguments = edit
        .prepare_arguments(
            &context,
            json!({"path":"sample.txt", "oldText":"still", "newText":"always"}),
        )
        .await
        .unwrap();
    assert_eq!(
        arguments["edits"],
        json!([{"oldText":"still", "newText":"always"}])
    );
    edit.validate_arguments(&arguments).unwrap();
    let (updates, _) = ToolUpdateSink::channel();
    edit.execute(context.clone(), ToolCallId::new("edit"), arguments, updates)
        .await
        .unwrap();
    let (updates, _) = ToolUpdateSink::channel();
    let result = NonStrictTool(Arc::new(pi_plugin_read::ReadTool))
        .execute(
            context,
            ToolCallId::new("read"),
            json!({"path":"sample.txt"}),
            updates,
        )
        .await
        .unwrap();
    assert_eq!(result.content, ToolResult::text("always works").content);
}

#[tokio::test]
async fn registered_sampling_overrides_survive_active_loadout_and_provider_projection() {
    for active in [
        vec![],
        vec!["read"],
        vec!["read", "bash", "powershell", "edit", "write"],
    ] {
        let provider = Arc::new(ScriptedProviderPlugin::scripted([ScriptedTurn::Text(
            "ok".into(),
        )]));
        let requests = provider.provider();
        let (plugins, providers, registries) = RegistriesBuilder::new()
            .register_plugin_sets(vec![Arc::new(NonStrictPlugin)], vec![provider])
            .unwrap();
        let runtime = Arc::new(AgentRuntime::new(
            1,
            "unchanged system prompt",
            Arc::new(registries),
            Arc::new(plugins),
            Arc::new(providers),
        ));
        let agent = Agent::with_runtime(
            AgentOptions {
                active_tools: active.iter().map(|name| (*name).into()).collect(),
                ..Default::default()
            },
            runtime,
        );
        agent.prompt("declare tools").await.unwrap();
        let request = &requests.requests()[0];
        assert_eq!(request.system_prompt, "unchanged system prompt");
        assert_eq!(
            request
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            active
        );
        assert!(
            request
                .tools
                .iter()
                .all(|tool| tool.constrained_sampling == Some(ConstrainedSampling::Disabled))
        );
    }
}
