use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pi_core::{
    ContentBlock, Message, ModelId, ModelSelection, PluginId, ProviderId, ToolCall, ToolCallId,
    ToolExecutionMode, ToolResult, ToolSpec, WorkspaceRoot, WorkspaceRootId, WorkspaceSpec,
};
use pi_plugin::{Plugin, RegisterContext, Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_sdk::{AgentHost, AgentSessionOptions};
use pi_session::AutoRetrySettings;
use pi_test_support::{ScriptedProvider, ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};

const PROMPT: &str = "Answer order questions using lookup_order. Use only returned order facts.";

#[derive(Clone, Debug, PartialEq, Eq)]
struct LookupObservation {
    session_id: String,
    workspace: WorkspaceSpec,
    generation: usize,
}

#[derive(Clone, Default)]
struct Fixture {
    fail: Arc<AtomicBool>,
    instances: Arc<AtomicUsize>,
    providers: Arc<Mutex<Vec<Arc<ScriptedProvider>>>>,
    lookups: Arc<Mutex<Vec<LookupObservation>>>,
}

impl Fixture {
    fn host(&self) -> AgentHost {
        let orders = self.clone();
        let providers = Arc::clone(&self.providers);
        AgentHost::builder(ModelSelection::new("scripted", "test"), PROMPT)
            .try_plugin_factory(move || {
                if orders.fail.load(Ordering::SeqCst) {
                    return Err("order service configuration is invalid");
                }
                Ok(OrdersPlugin {
                    generation: orders.instances.fetch_add(1, Ordering::SeqCst) + 1,
                    lookups: Arc::clone(&orders.lookups),
                })
            })
            .provider_plugin_factory(move || {
                let plugin = ScriptedProviderPlugin::scripted([
                    ScriptedTurn::ToolCalls(vec![ToolCall::new(
                        "order-call",
                        "lookup_order",
                        json!({"orderId": "ORD-42"}),
                    )]),
                    ScriptedTurn::Text("Order ORD-42 has shipped.".into()),
                    ScriptedTurn::Text("The existing conversation is still usable.".into()),
                ]);
                providers.lock().unwrap().push(plugin.provider());
                plugin
            })
            .session_options(AgentSessionOptions::default().retry(AutoRetrySettings {
                enabled: false,
                ..AutoRetrySettings::default()
            }))
            .build()
    }

    fn provider(&self, index: usize) -> Arc<ScriptedProvider> {
        Arc::clone(&self.providers.lock().unwrap()[index])
    }
}

struct OrdersPlugin {
    generation: usize,
    lookups: Arc<Mutex<Vec<LookupObservation>>>,
}

#[pi_plugin::plugin]
impl Plugin for OrdersPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("orders")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(OrderLookup {
            generation: self.generation,
            lookups: Arc::clone(&self.lookups),
        }))
    }
}

struct OrderLookup {
    generation: usize,
    lookups: Arc<Mutex<Vec<LookupObservation>>>,
}

#[async_trait]
impl Tool for OrderLookup {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "lookup_order".into(),
            label: "Look up order".into(),
            description: "Returns an order's fulfillment status.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"orderId": {"type": "string"}},
                "required": ["orderId"],
                "additionalProperties": false
            }),
            execution_mode: ToolExecutionMode::Parallel,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        _call: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        // Real managed-session capabilities must be bound before a domain tool executes.
        let snapshot = context.session.snapshot()?;
        assert_eq!(
            context.session.workspace()?.spec(),
            context.workspace().spec()
        );
        assert_eq!(context.session.cwd()?, context.cwd());
        self.lookups.lock().unwrap().push(LookupObservation {
            session_id: snapshot.id().into(),
            workspace: context.workspace().spec().clone(),
            generation: self.generation,
        });
        Ok(ToolResult::text(
            json!({"orderId": input["orderId"], "status": "shipped"}).to_string(),
        ))
    }
}

fn text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect()
}

fn workspace(root: &std::path::Path) -> WorkspaceSpec {
    WorkspaceSpec::new(
        vec![
            WorkspaceRoot::external("orders", "Orders", root.join("orders")),
            WorkspaceRoot::external("catalog", "Catalog", root.join("catalog")),
        ],
        WorkspaceRootId::new("orders"),
        root.join("orders"),
    )
    .unwrap()
}

#[tokio::test]
async fn order_tool_continues_and_resumes_without_coding_resources() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(directory.path());
    std::fs::create_dir_all(workspace.cwd().join(".pi/skills/leak")).unwrap();
    std::fs::write(workspace.cwd().join("AGENTS.md"), "CODING_CONTEXT_LEAK").unwrap();
    for name in ["settings.json", "models.json", "mcp.json", "plugins.json"] {
        std::fs::write(workspace.cwd().join(".pi").join(name), "invalid JSON").unwrap();
    }
    std::fs::write(
        workspace.cwd().join(".pi/skills/leak/SKILL.md"),
        "---\nname: leak\ndescription: Coding skill\n---\nCODING_SKILL_LEAK",
    )
    .unwrap();
    let fixture = Fixture::default();
    let host = fixture.host();
    let path = directory.path().join("sessions/order.jsonl");
    let session = host
        .sessions()
        .create_session_with_workspace(workspace.clone(), &path, None, None)
        .await
        .unwrap();
    let id = session.current().log().id().to_owned();
    assert!(!path.exists());
    assert!(
        session
            .current()
            .execute_shell("printf unexpected", Default::default())
            .await
            .unwrap_err()
            .to_string()
            .contains("shell execution is not configured")
    );
    assert!(!path.exists());
    session.current().submit("Where is ORD-42?").await.unwrap();
    assert!(path.exists());
    let requests = fixture.provider(0).requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.system_prompt, PROMPT);
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, "lookup_order");
    }
    assert!(requests[1].messages.iter().any(|message| {
        matches!(message, Message::ToolResult(result) if !result.is_error && text(&result.content).contains("shipped"))
    }));
    assert!(
        session
            .current()
            .runtime()
            .resource_diagnostics()
            .is_empty()
    );
    assert!(session.current().runtime().command_specs().is_empty());
    assert_eq!(fixture.lookups.lock().unwrap()[0].session_id, id);
    assert_eq!(fixture.lookups.lock().unwrap()[0].workspace, workspace);
    host.sessions().shutdown().await.unwrap();

    let resumed_host = fixture.host();
    let resumed = resumed_host.sessions().open_session(&path).await.unwrap();
    assert_eq!(resumed.current().log().id(), id);
    assert_eq!(resumed.current().runtime().workspace().spec(), &workspace);
    resumed
        .current()
        .submit("Check the order again.")
        .await
        .unwrap();
    let requests = fixture.provider(1).requests();
    assert!(requests[0].messages.iter().any(|message| {
        matches!(message, Message::Assistant(assistant) if text(&assistant.content).contains("has shipped"))
    }));
    assert_eq!(fixture.lookups.lock().unwrap()[1].session_id, id);
    resumed_host.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn reload_rebuilds_plugins_and_retains_model_workspace_and_conversation() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = workspace(directory.path());
    let fixture = Fixture::default();
    let host = fixture.host();
    let session = host
        .sessions()
        .create_session_with_workspace(
            workspace.clone(),
            directory.path().join("session.jsonl"),
            None,
            None,
        )
        .await
        .unwrap();
    session.current().submit("Where is ORD-42?").await.unwrap();
    session
        .current()
        .set_model(ProviderId::new("scripted"), ModelId::new("custom-model"))
        .unwrap();
    let previous = session.current();
    session.reload().await.unwrap();
    assert!(!Arc::ptr_eq(&previous, &session.current()));
    assert_eq!(fixture.instances.load(Ordering::SeqCst), 2);
    assert_eq!(session.current().log().id(), previous.log().id());
    assert_eq!(session.current().runtime().workspace().spec(), &workspace);
    assert_eq!(
        session
            .current()
            .runtime()
            .agent()
            .state()
            .model_id
            .as_str(),
        "custom-model"
    );
    session.current().submit("Look it up again.").await.unwrap();
    assert_eq!(
        fixture.provider(1).requests()[0].model.as_str(),
        "custom-model"
    );
    assert_eq!(fixture.lookups.lock().unwrap()[1].generation, 2);
    assert_eq!(fixture.lookups.lock().unwrap()[1].workspace, workspace);

    fixture.fail.store(true, Ordering::SeqCst);
    let previous = session.current();
    let saved = std::fs::read(previous.log().path()).unwrap();
    assert!(
        session
            .reload()
            .await
            .unwrap_err()
            .to_string()
            .contains("order service configuration")
    );
    assert!(Arc::ptr_eq(&previous, &session.current()));
    assert_eq!(std::fs::read(previous.log().path()).unwrap(), saved);
    session.current().submit("Continue.").await.unwrap();
    assert_eq!(fixture.provider(1).requests().len(), 3);
    host.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn unsaved_reload_stays_in_memory_and_sessions_have_distinct_capabilities() {
    let directory = tempfile::tempdir().unwrap();
    let fixture = Fixture::default();
    let host = fixture.host();
    let path = directory.path().join("first.jsonl");
    let first = host
        .sessions()
        .create_session(directory.path(), &path)
        .await
        .unwrap();
    let id = first.current().log().id().to_owned();
    first.reload().await.unwrap();
    assert!(!path.exists());
    assert_eq!(first.current().log().id(), id);
    let second = host
        .sessions()
        .create_session(
            directory.path().join("second"),
            directory.path().join("second.jsonl"),
        )
        .await
        .unwrap();
    first.current().submit("Check my order.").await.unwrap();
    second.current().submit("Check my order.").await.unwrap();
    let lookups = fixture.lookups.lock().unwrap().clone();
    assert_eq!(lookups[0].session_id, id);
    assert_eq!(lookups[1].session_id, second.current().log().id());
    assert_ne!(lookups[0].session_id, lookups[1].session_id);
    assert_ne!(lookups[0].workspace, lookups[1].workspace);
    host.sessions().shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_provider_fails_before_creating_a_resume_entry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("never-created.jsonl");
    let host =
        AgentHost::builder(ModelSelection::new("missing", "model"), "Explicit prompt").build();
    let result = host
        .sessions()
        .create_session(directory.path(), &path)
        .await;
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("provider missing is not registered")
    );
    assert!(!path.exists());
    assert!(host.sessions().sessions().is_empty());
}

#[tokio::test]
async fn explicit_tool_selection_and_later_changes_survive_reload() {
    let directory = tempfile::tempdir().unwrap();
    let host = AgentHost::builder(ModelSelection::new("scripted", "test"), PROMPT)
        .plugin_factory(|| OrdersPlugin {
            generation: 1,
            lookups: Arc::default(),
        })
        .provider_plugin_factory(|| ScriptedProviderPlugin::scripted([]))
        .active_tools(Vec::new())
        .build();
    let session = host
        .sessions()
        .create_session(directory.path(), directory.path().join("empty.jsonl"))
        .await
        .unwrap();
    assert!(session.current().runtime().active_tools().is_empty());
    session.reload().await.unwrap();
    assert!(session.current().runtime().active_tools().is_empty());
    session
        .current()
        .set_active_tools(["lookup_order"])
        .unwrap();
    session.reload().await.unwrap();
    assert_eq!(session.current().runtime().active_tools(), ["lookup_order"]);
    host.sessions().shutdown().await.unwrap();
}
