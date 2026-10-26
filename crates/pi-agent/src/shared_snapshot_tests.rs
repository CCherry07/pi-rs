use std::sync::{Arc, Mutex, OnceLock};

use pi_core::{
    AbortSignal, AgentEvent, ContentBlock, Message, ModelId, PluginId, ProviderId,
    ResponseMetadata, StopReason, StreamEvent, TextContent, Usage, UserMessage,
};
use pi_plugin::{
    AgentPluginContext, MessageEndEvent, MessageEndPatch, Plugin, PluginError, RegistriesBuilder,
};
use pi_test_support::{ScriptedProviderPlugin, ScriptedTurn};

use crate::{Agent, AgentOptions, AgentRestoreState, AgentRuntime, WeakAgent};

fn agent(turn: ScriptedTurn, plugin: Option<Arc<dyn Plugin>>) -> Agent {
    let (plugins, providers, registries) = RegistriesBuilder::new()
        .register_plugin_sets(
            plugin.into_iter().collect(),
            vec![Arc::new(ScriptedProviderPlugin::scripted([turn]))],
        )
        .unwrap();
    Agent::with_runtime(
        AgentOptions {
            messages: vec![Message::User(UserMessage::text("existing history", 0))],
            ..AgentOptions::default()
        },
        Arc::new(AgentRuntime::new(
            1,
            "",
            Arc::new(registries),
            Arc::new(plugins),
            Arc::new(providers),
        )),
    )
}

#[tokio::test]
async fn streamed_snapshots_share_history_until_a_message_commits() {
    let mut events = vec![
        StreamEvent::Start {
            metadata: ResponseMetadata::new(
                ProviderId::new("scripted"),
                ModelId::new("test"),
                "scripted",
                1,
            ),
        },
        StreamEvent::TextStart { content_index: 0 },
    ];
    events.extend((0..32).map(|_| StreamEvent::TextDelta {
        content_index: 0,
        delta: "chunk".into(),
    }));
    events.extend([
        StreamEvent::TextEnd {
            content_index: 0,
            text_signature: None,
        },
        StreamEvent::Done {
            reason: StopReason::Stop,
            usage: Usage::default(),
        },
    ]);
    let agent = agent(ScriptedTurn::Events(events), None);
    let original = agent.state();
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    agent.subscribe(Arc::new({
        let weak = agent.downgrade();
        let snapshots = snapshots.clone();
        move |event: AgentEvent, _: AbortSignal| {
            let weak = weak.clone();
            let snapshots = snapshots.clone();
            async move {
                if matches!(event, AgentEvent::MessageUpdate { .. }) {
                    snapshots.lock().unwrap().push(weak.state().unwrap());
                }
                Ok(())
            }
        }
    }));
    agent.prompt("new input").await.unwrap();
    let snapshots = snapshots.lock().unwrap();
    assert!(snapshots.len() >= 32);
    let stream_history = &snapshots[0].messages;
    for snapshot in &*snapshots {
        assert!(Arc::ptr_eq(stream_history, &snapshot.messages));
        assert_eq!(snapshot.messages.len(), 2);
    }
    let completed = agent.state();
    assert_eq!(original.messages.len(), 1);
    assert_eq!(completed.messages.len(), 3);
    assert!(!Arc::ptr_eq(stream_history, &completed.messages));
}

struct ReplacingPlugin {
    agent: Arc<OnceLock<WeakAgent>>,
    before_replacement: Arc<Mutex<Option<Arc<Vec<Message>>>>>,
}

#[pi_plugin::plugin]
impl Plugin for ReplacingPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("snapshot-replacement")
    }

    async fn message_end(
        &self,
        _: AgentPluginContext,
        event: MessageEndEvent,
    ) -> Result<MessageEndPatch, PluginError> {
        let Message::Assistant(mut message) = event.message else {
            return Ok(MessageEndPatch::default());
        };
        *self.before_replacement.lock().unwrap() =
            Some(self.agent.get().unwrap().state().unwrap().messages);
        Arc::make_mut(&mut message).content =
            vec![ContentBlock::Text(TextContent::new("replacement"))];
        Ok(MessageEndPatch {
            message: Some(Message::Assistant(message)),
        })
    }
}

#[tokio::test]
async fn hook_replacement_context_restore_and_reset_preserve_retained_history() {
    let owner = Arc::new(OnceLock::new());
    let before_replacement = Arc::new(Mutex::new(None));
    let agent = agent(
        ScriptedTurn::Text("original response".into()),
        Some(Arc::new(ReplacingPlugin {
            agent: owner.clone(),
            before_replacement: before_replacement.clone(),
        })),
    );
    assert!(owner.set(agent.downgrade()).is_ok());
    agent.prompt("new input").await.unwrap();
    let before = before_replacement.lock().unwrap().take().unwrap();
    let completed = agent.state();
    assert!(
        matches!(before.last(), Some(Message::Assistant(message)) if message.content == vec![ContentBlock::Text(TextContent::new("original response"))])
    );
    assert!(
        matches!(completed.messages.last(), Some(Message::Assistant(message)) if message.content == vec![ContentBlock::Text(TextContent::new("replacement"))])
    );
    agent
        .replace_context_messages(vec![Message::User(UserMessage::text(
            "compacted context",
            2,
        ))])
        .unwrap();
    let compacted = agent.state();
    agent
        .restore(AgentRestoreState {
            system_prompt: None,
            provider_id: ProviderId::new("scripted"),
            model_id: ModelId::new("test"),
            thinking_level: pi_core::ThinkingLevel::Off,
            active_tools: vec![],
            messages: vec![Message::User(UserMessage::text("restored context", 3))],
        })
        .unwrap();
    let restored = agent.state();
    agent.reset().unwrap();
    assert_eq!(completed.messages.len(), 3);
    assert_eq!(
        compacted.messages.as_ref(),
        &vec![Message::User(UserMessage::text("compacted context", 2))]
    );
    assert_eq!(
        restored.messages.as_ref(),
        &vec![Message::User(UserMessage::text("restored context", 3))]
    );
    assert!(agent.state().messages.is_empty());
}
