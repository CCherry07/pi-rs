//! Deterministic domain composition: a real order tool driven by a scripted local provider.

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    ContentBlock, Message, ModelSelection, PluginId, ToolCall, ToolCallId, ToolExecutionMode,
    ToolResult, ToolSpec,
};
use pi_plugin::{Plugin, RegisterContext, Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_sdk::AgentHost;
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};
use serde_json::{Value, json};

struct OrdersPlugin;

#[pi_plugin::plugin]
impl Plugin for OrdersPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("orders")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(LookupOrder))
    }
}

struct LookupOrder;

#[async_trait]
impl Tool for LookupOrder {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "lookup_order".into(),
            label: "Look up order".into(),
            description: "Finds the fulfillment status of an order by its identifier.".into(),
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
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let order_id = input["orderId"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments("orderId is required".into()))?;
        // This in-memory record stands in for the domain's order service.
        // The service owns authorization and business rules when connected to real data.
        if order_id != "ORD-42" {
            return Ok(ToolResult::text(json!({"found": false}).to_string()));
        }
        Ok(ToolResult::text(
            json!({
                "orderId": order_id,
                "status": "shipped",
                "trackingNumber": "DEMO-0042",
                "sessionId": context.session.id()?
            })
            .to_string(),
        ))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let host = AgentHost::builder(
        ModelSelection::new("scripted", "test"),
        "You help customers track orders. Look up order facts before answering.",
    )
    .plugin_factory(|| OrdersPlugin)
    .provider_plugin_factory(|| {
        ScriptedProviderPlugin::scripted([
            ScriptedTurn::ToolCalls(vec![ToolCall::new(
                "lookup-42",
                "lookup_order",
                json!({"orderId": "ORD-42"}),
            )]),
            ScriptedTurn::Text("Order ORD-42 has shipped. Tracking number: DEMO-0042.".into()),
        ])
    })
    .build();
    let storage = tempfile::tempdir()?;
    let session = host
        .sessions()
        .create_session(storage.path(), storage.path().join("order.jsonl"))
        .await?;
    session.current().submit("Where is order ORD-42?").await?;
    for message in &session.current().runtime().agent().state().messages {
        if let Message::Assistant(assistant) = message {
            for block in &assistant.content {
                if let ContentBlock::Text(text) = block {
                    println!("{}", text.text);
                }
            }
        }
    }
    host.sessions().shutdown().await?;
    Ok(())
}
