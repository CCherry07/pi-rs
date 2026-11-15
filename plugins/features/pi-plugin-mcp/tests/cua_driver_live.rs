//! Opt-in verification against an installed official Cua Driver. No desktop input.

use pi_core::{AbortHandle, ModelId, ProviderId, RunId, ToolExecutionMode};
use pi_plugin::{BeforeAgentStartEvent, RegistriesBuilder, ToolContext, ToolUpdateSink};
use pi_plugin_mcp::{McpServerConfig, McpToolSet};
use serde_json::json;

#[tokio::test]
#[ignore = "requires an installed cua-driver and its local daemon"]
async fn cua_driver_mcp_discovers_guidance_and_executes_read_only_tools() {
    let binary = std::env::var("PI_CUA_DRIVER_BIN").unwrap_or_else(|_| "cua-driver".into());
    let pool = McpToolSet::connect(vec![
        McpServerConfig::stdio("cua-driver", binary).args(["mcp"]),
    ])
    .await
    .unwrap();
    let (driver, registry) = RegistriesBuilder::new()
        .register_plugins(vec![pool.plugin()])
        .unwrap();
    let observe = registry.tool("mcp__cua_driver__get_window_state").unwrap();
    let act = registry.tool("mcp__cua_driver__run_steps").unwrap();
    assert_eq!(observe.spec().execution_mode, ToolExecutionMode::Parallel);
    assert_eq!(act.spec().execution_mode, ToolExecutionMode::Sequential);

    let cwd = std::env::current_dir().unwrap();
    let (_abort, signal) = AbortHandle::new();
    let patch = driver
        .before_agent_start(
            &RunId::next(),
            &cwd,
            &signal,
            BeforeAgentStartEvent {
                system_prompt: "base".into(),
                input_messages: vec![],
                active_tools: pool.tools().into_iter().map(|tool| tool.name).collect(),
                provider_id: ProviderId::new("scripted"),
                model_id: ModelId::new("test"),
            },
        )
        .await
        .unwrap();
    let prompt = patch.system_prompt.unwrap();
    assert!(prompt.contains("mcp__cua_driver__"));
    assert!(prompt.contains("run_steps"));

    for (name, arguments) in [
        ("check_permissions", json!({"prompt": false})),
        ("get_guide", json!({"topic": "batching"})),
    ] {
        let tool = registry.tool(&format!("mcp__cua_driver__{name}")).unwrap();
        let (updates, _receiver) = ToolUpdateSink::channel();
        let result = tool
            .execute(
                ToolContext::standalone(cwd.clone(), signal.clone()),
                name.into(),
                arguments,
                updates,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{name}: {:?}", result.content);
        assert!(!result.content.is_empty());
    }
    println!(
        "Official Cua Driver: {} tools; guidance and read-only calls verified",
        pool.tools().len()
    );
    pool.shutdown().await.unwrap();
}
