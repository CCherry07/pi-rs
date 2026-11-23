//! Cancellation before a provider yields a stream, through the public Agent boundary.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pi_agent::{Agent, AgentLoopStop, AgentOptions, AgentRuntime, StreamFn};
use pi_core::{AgentEvent, Message, ModelSpec, PluginId, StopReason, ThinkingLevel, Usage};
use pi_plugin::{ProviderPlugin, ProviderRegisterContext, RegistriesBuilder};
use pi_test_support::ScriptedProviderPlugin;
use tokio::sync::Notify;

struct Catalog;

#[pi_plugin::provider_plugin]
impl ProviderPlugin for Catalog {
    fn id(&self) -> PluginId { PluginId::new("cancellation-catalog") }

    fn register(&self, context: &mut ProviderRegisterContext<'_>) -> pi_plugin::Result<()> {
        let mut model = ModelSpec::new("scripted", "test", "Test", "openai-completions");
        model.reasoning = true;
        context.register_model(model)
    }
}

#[tokio::test]
async fn abort_while_provider_is_pending_preserves_empty_content_and_model_metadata() {
    let entered = Arc::new(Notify::new());
    let options = AgentOptions {
        thinking_level: ThinkingLevel::High,
        stream_fn: Some(StreamFn::new({
            let entered = entered.clone();
            move |_, _, _| {
                let entered = entered.clone();
                async move {
                    entered.notify_one();
                    std::future::pending().await
                }
            }
        })),
        ..AgentOptions::default()
    };
    let provider_plugins: Vec<Arc<dyn ProviderPlugin>> = vec![Arc::new(ScriptedProviderPlugin::scripted([])), Arc::new(Catalog)];
    let (plugins, provider_plugins, registries) = RegistriesBuilder::new().register_plugin_sets(Vec::new(), provider_plugins).unwrap();
    let runtime = Arc::new(AgentRuntime::new(1, "", Arc::new(registries), Arc::new(plugins), Arc::new(provider_plugins)));
    let agent = Agent::with_runtime(options, runtime);
    let events = Arc::new(Mutex::new(Vec::new()));
    agent.subscribe(Arc::new({
        let events = events.clone();
        move |event, _| {
            let events = events.clone();
            async move { events.lock().unwrap().push(event); Ok(()) }
        }
    }));
    let running = agent.clone();
    let task = tokio::spawn(async move { running.prompt("cancel before response headers").await.unwrap() });
    tokio::time::timeout(Duration::from_secs(2), entered.notified()).await.unwrap();
    agent.abort();
    let outcome = tokio::time::timeout(Duration::from_secs(2), task).await.unwrap().unwrap();
    assert_eq!(outcome.stop, AgentLoopStop::Aborted);
    let Message::Assistant(message) = outcome.new_messages.last().unwrap() else { panic!("missing aborted reply") };
    assert!(message.content.is_empty(), "no provider content was observed: {:?}", message.content);
    assert_eq!(message.api, "openai-completions");
    assert_eq!(message.provider.as_str(), "scripted");
    assert_eq!(message.model.as_str(), "test");
    assert_eq!(message.stop_reason, StopReason::Aborted);
    assert_eq!(message.error_message.as_deref(), Some("Request aborted"));
    assert_eq!(message.usage, Usage::default());
    assert_eq!(message.thinking_level, Some(ThinkingLevel::High));
    assert!(message.duration_ms.is_some());
    let events = events.lock().unwrap();
    assert!(!events.iter().any(|event| matches!(event, AgentEvent::MessageUpdate { .. })));
    assert!(matches!(&events[events.len()-4..], [
        AgentEvent::MessageStart { message: Message::Assistant(start) },
        AgentEvent::MessageEnd { message: Message::Assistant(end) },
        AgentEvent::TurnEnd { message: turn, tool_results },
        AgentEvent::AgentEnd { messages },
    ] if start == message && end == message && turn == message && tool_results.is_empty() && messages == &outcome.new_messages));
}
