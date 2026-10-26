use std::sync::{Arc, Mutex};

use pi_plugin::{
    AbortHandle, AgentPluginContext, ContextEvent, ContextPatch, Message, Plugin, PluginDriver,
    PluginError, PluginId, RunId, UserMessage,
};

struct ContextProbe {
    index: usize,
    events: Arc<Mutex<Vec<ContextEvent>>>,
}

#[pi_plugin::plugin]
impl Plugin for ContextProbe {
    fn id(&self) -> PluginId {
        PluginId::new(format!("context-{}", self.index))
    }

    async fn context(
        &self,
        _: AgentPluginContext,
        event: ContextEvent,
    ) -> Result<ContextPatch, PluginError> {
        self.events.lock().unwrap().push(event);
        match self.index {
            1 => Err(PluginError::Hook {
                plugin_id: self.id(),
                hook: "context",
                message: "failed transform".into(),
            }),
            2 => Ok(ContextPatch {
                messages: Some(vec![Message::User(UserMessage::text("replacement", 2))]),
            }),
            _ => Ok(ContextPatch::default()),
        }
    }
}

#[tokio::test]
async fn context_shares_read_snapshots_and_only_replaces_after_a_successful_patch() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let driver = PluginDriver::new(
        (0..4)
            .map(|index| {
                Arc::new(ContextProbe {
                    index,
                    events: events.clone(),
                }) as Arc<dyn Plugin>
            })
            .collect(),
    )
    .unwrap();
    let (_, signal) = AbortHandle::new();
    let original = Message::User(UserMessage::text("original", 1));
    let output = driver
        .context(
            &RunId::new("run"),
            std::path::Path::new("."),
            &signal,
            vec![original.clone()],
        )
        .await
        .unwrap();
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].messages.as_ptr(), events[1].messages.as_ptr());
    assert_eq!(events[0].messages.as_ptr(), events[2].messages.as_ptr());
    assert_ne!(events[0].messages.as_ptr(), events[3].messages.as_ptr());
    assert_eq!(events[0].messages.as_slice(), &[original]);
    assert_eq!(events[3].messages.as_slice(), output);
    assert_eq!(
        output,
        vec![Message::User(UserMessage::text("replacement", 2))]
    );
    assert_eq!(driver.diagnostics().len(), 1);
    assert_eq!(driver.diagnostics()[0].plugin_id.as_str(), "context-1");
}
