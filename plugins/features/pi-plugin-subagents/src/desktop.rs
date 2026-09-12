//! Plugin-owned desktop data and commands. The host transports opaque widget
//! entries and dispatches ordinary registered commands; it never parses agent IDs.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    Command, CommandContext, CommandError, CommandOutcome, CommandSpec, NoticeLevel,
    RegisterContext, SessionContext,
};
use pi_plugin_sdk::desktop::WidgetPublisher;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::SubagentRuntime;

pub(crate) const WIDGET_KEY: &str = "subagents.tasks";
const MAX_WIDGET_BYTES: usize = 240 * 1024;
const MAX_HISTORY_TASKS: usize = 128;

fn bounded_text(value: &str, maximum: usize) -> String {
    if value.len() <= maximum {
        return value.into();
    }
    let mut end = maximum.saturating_sub('…'.len_utf8());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn minimal_task(id: &str, value: &Value) -> Option<Value> {
    if value.get("agentId")?.as_str()? != id || id.len() > 128 {
        return None;
    }
    let text = |value: &Value, limit| value.as_str().map(|value| bounded_text(value, limit));
    Some(json!({
        "agentId":id,"agent":text(&value["agent"],128),"task":text(&value["task"],1024),
        "state":text(&value["state"],32),"updatedAt":value["updatedAt"].as_u64().unwrap_or(0),
        "totalTokens":value["totalTokens"].as_u64().unwrap_or(0),
        "session":{"sessionId":text(&value["session"]["sessionId"],128),
            "isolatedSessionId":text(&value["session"]["isolatedSessionId"],128),
            "ownerSessionId":text(&value["session"]["ownerSessionId"],128)}
    }))
}

#[derive(Default)]
pub(crate) struct DesktopState {
    sessions: HashMap<String, DesktopSession>,
    suspended: HashSet<String>,
}

struct DesktopSession {
    agents: BTreeMap<String, Value>,
    publisher: WidgetPublisher,
}

impl Default for DesktopSession {
    fn default() -> Self {
        Self {
            agents: BTreeMap::new(),
            publisher: WidgetPublisher::new(WIDGET_KEY).expect("static widget key is valid"),
        }
    }
}

impl DesktopState {
    pub(crate) fn needs_restore(&self, owner: &str) -> bool {
        !self.sessions.contains_key(owner)
    }

    pub(crate) fn restore(&mut self, owner: &str, entries: Vec<Value>) {
        self.suspended.remove(owner);
        self.sessions.entry(owner.into()).or_insert_with(|| {
            let agents = entries
                .iter()
                .rev()
                .find(|entry| {
                    entry["customType"] == "pi.ui.widget" && entry["data"]["key"] == WIDGET_KEY
                })
                .and_then(|entry| {
                    let value = entry.get("data")?.get("value")?;
                    if value.get("version")?.as_u64()? != 1 {
                        return None;
                    }
                    if value.get("ownerSessionId")?.as_str()? != owner {
                        return None;
                    }
                    let agents = value.get("agents")?.as_object()?;
                    Some(
                        agents
                            .iter()
                            .filter_map(|(key, value)| {
                                minimal_task(key, value).map(|value| (key.clone(), value))
                            })
                            .collect(),
                    )
                })
                .unwrap_or_default();
            DesktopSession {
                agents,
                ..DesktopSession::default()
            }
        });
    }

    pub(crate) fn suspend(&mut self, owner: &str) {
        self.suspended.insert(owner.into());
    }

    pub(crate) fn forget(&mut self, owner: &str) {
        self.sessions.remove(owner);
        self.suspended.remove(owner);
    }

    pub(crate) fn publish(
        &mut self,
        runtime_id: &str,
        owner: &str,
        context: &SessionContext,
        live: BTreeMap<String, Value>,
    ) {
        if self.suspended.contains(owner) {
            return;
        }
        let session = self.sessions.entry(owner.into()).or_default();
        let live_ids = live.keys().cloned().collect::<Vec<_>>();
        session.agents.extend(
            live.into_iter()
                .filter_map(|(id, value)| minimal_task(&id, &value).map(|value| (id, value))),
        );
        if session.agents.is_empty() {
            return;
        }
        let historical = session
            .agents
            .iter()
            .filter(|(id, _)| !live_ids.contains(id))
            .map(|(id, value)| (value["updatedAt"].as_u64().unwrap_or(0), id.clone()))
            .collect::<std::collections::BTreeSet<_>>();
        let mut value = json!({"version":1,"runtimeId":runtime_id,"ownerSessionId":owner,
            "agents":session.agents,"liveAgentIds":live_ids});
        // Keep all live children. Old terminal previews can be read from their original
        // tool/session history after eviction; the widget never stores full transcripts.
        for (_, id) in historical {
            if session.agents.len() <= MAX_HISTORY_TASKS
                && serde_json::to_vec(&value).is_ok_and(|bytes| bytes.len() <= MAX_WIDGET_BYTES)
            {
                break;
            }
            session.agents.remove(&id);
            value["agents"]
                .as_object_mut()
                .expect("widget agent map")
                .remove(&id);
        }
        let _ = session.publisher.publish(context, &value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::{
        ModelsContextAccess, PluginContextEpoch, PluginContextResult, SessionContextAccess,
        UiContextAccess,
    };
    use std::sync::Mutex;

    #[derive(Default)]
    struct Journal {
        entries: Mutex<Vec<Value>>,
    }
    impl ModelsContextAccess for Journal {}
    #[async_trait]
    impl UiContextAccess for Journal {}
    #[async_trait]
    impl SessionContextAccess for Journal {
        fn append_entry(
            &self,
            custom_type: String,
            data: Option<Value>,
        ) -> PluginContextResult<()> {
            self.entries
                .lock()
                .unwrap()
                .push(json!({"customType":custom_type,"data":data}));
            Ok(())
        }
    }

    #[test]
    fn snapshots_coalesce_and_rebind_after_retired_context_without_replaying_ownership() {
        let journal = Arc::new(Journal::default());
        let epoch = PluginContextEpoch::new(journal.clone());
        let context = epoch.context().session;
        let mut desktop = DesktopState::default();
        let mut agents = BTreeMap::from([(
            "agent-1".into(),
            json!({
                "agentId":"agent-1","agent":"reviewer","state":"running",
                "session":{"sessionId":"child","ownerSessionId":"owner"},"totalTokens":0,
            }),
        )]);
        desktop.publish("runtime-1", "owner", &context, agents.clone());
        desktop.publish("runtime-1", "owner", &context, agents.clone());
        assert_eq!(journal.entries.lock().unwrap().len(), 1);
        epoch.retire();
        agents.get_mut("agent-1").unwrap()["state"] = json!("interrupted");
        desktop.publish("runtime-1", "owner", &context, agents.clone());
        assert_eq!(journal.entries.lock().unwrap().len(), 1);
        let next = PluginContextEpoch::new(journal.clone());
        let next_context = next.context().session;
        desktop.publish("runtime-1", "owner", &next_context, agents);
        assert_eq!(journal.entries.lock().unwrap().len(), 2);

        let mut restarted = DesktopState::default();
        restarted.restore("owner", journal.entries.lock().unwrap().clone());
        restarted.publish("runtime-2", "owner", &next_context, BTreeMap::new());
        let entries = journal.entries.lock().unwrap();
        let restored = &entries.last().unwrap()["data"]["value"];
        assert_eq!(restored["runtimeId"], "runtime-2");
        assert_eq!(restored["agents"]["agent-1"]["state"], "interrupted");
        assert_eq!(restored["liveAgentIds"], json!([]));
    }

    #[test]
    fn bounded_history_preserves_live_children_and_shutdown_suppresses_publication() {
        let journal = Arc::new(Journal::default());
        let epoch = PluginContextEpoch::new(journal.clone());
        let context = epoch.context().session;
        let mut desktop = DesktopState::default();
        let make_task = |id: &str| {
            json!({
                "agentId":id,"agent":"reviewer","task":"多字节\"任务".repeat(4000),
                "state":"running","updatedAt":1,"session":{"sessionId":"child","ownerSessionId":"owner"},
                "accidentalTranscript":"x".repeat(5000)
            })
        };
        let historical = (0..200)
            .map(|index| {
                let id = format!("historical-{index}");
                (id.clone(), make_task(&id))
            })
            .collect::<BTreeMap<_, _>>();
        desktop.restore(
            "owner",
            vec![json!({"customType":"pi.ui.widget","data":{
                "key":WIDGET_KEY,"value":{"version":1,"ownerSessionId":"owner","agents":historical}
            }})],
        );
        let live = (0..64)
            .map(|index| {
                let id = format!("live-{index}");
                (id.clone(), make_task(&id))
            })
            .collect::<BTreeMap<_, _>>();
        desktop.publish("runtime", "owner", &context, live.clone());
        let entries = journal.entries.lock().unwrap();
        let snapshot = &entries[0]["data"]["value"];
        assert!(serde_json::to_vec(snapshot).unwrap().len() <= MAX_WIDGET_BYTES);
        assert!(snapshot["agents"].as_object().unwrap().len() <= MAX_HISTORY_TASKS);
        assert_eq!(snapshot["liveAgentIds"].as_array().unwrap().len(), 64);
        for id in live.keys() {
            let task = &snapshot["agents"][id];
            assert!(task["task"].as_str().unwrap().ends_with('…'));
            assert!(task.get("accidentalTranscript").is_none());
        }
        drop(entries);
        desktop.suspend("owner");
        desktop.publish("runtime", "owner", &context, BTreeMap::new());
        assert_eq!(journal.entries.lock().unwrap().len(), 1);
    }

    #[test]
    fn latest_tombstone_does_not_resurrect_an_older_snapshot() {
        let mut desktop = DesktopState::default();
        desktop.restore(
            "owner",
            vec![
                json!({"customType":"pi.ui.widget","data":{"key":WIDGET_KEY,"value":{
                    "version":1,"ownerSessionId":"owner","agents":{"agent":{"agentId":"agent"}}
                }}}),
                json!({"customType":"pi.ui.widget","data":{"key":WIDGET_KEY,"value":null}}),
            ],
        );
        assert!(desktop.sessions["owner"].agents.is_empty());
    }
}

#[derive(Clone, Copy)]
enum Action {
    Interrupt,
    FollowUp,
}

struct SubagentCommand {
    runtime: SubagentRuntime,
    action: Action,
}

pub(crate) fn register_commands(
    context: &mut RegisterContext<'_>,
    runtime: &SubagentRuntime,
) -> pi_core::Result<()> {
    for action in [Action::Interrupt, Action::FollowUp] {
        context.register_command(Arc::new(SubagentCommand {
            runtime: runtime.clone(),
            action,
        }))?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    target: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowUp {
    target: String,
    task: String,
}

fn required(value: &str, field: &str) -> Result<(), CommandError> {
    if value.trim().is_empty() {
        return Err(CommandError::InvalidArguments(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

#[async_trait]
impl Command for SubagentCommand {
    fn spec(&self) -> CommandSpec {
        match self.action {
            Action::Interrupt => CommandSpec {
                name: "subagents:interrupt".into(),
                description: "Stop a direct child's active turn".into(),
                argument_hint: Some("{\"target\":\"agent-id\"}".into()),
            },
            Action::FollowUp => CommandSpec {
                name: "subagents:followup".into(),
                description: "Give a direct child more work in its existing session".into(),
                argument_hint: Some("{\"target\":\"agent-id\",\"task\":\"...\"}".into()),
            },
        }
    }

    async fn execute(
        &self,
        context: CommandContext,
        arguments: String,
    ) -> Result<CommandOutcome, CommandError> {
        context
            .signal()
            .check()
            .map_err(|_| CommandError::Aborted)?;
        let owner = context.session.id()?;
        let notice = match self.action {
            Action::Interrupt => {
                let input: Target = serde_json::from_str(&arguments)
                    .map_err(|error| CommandError::InvalidArguments(error.to_string()))?;
                required(&input.target, "target")?;
                self.runtime
                    .interrupt(&owner, &input.target)
                    .map_err(CommandError::Execution)?;
                "Agent interruption requested."
            }
            Action::FollowUp => {
                let input: FollowUp = serde_json::from_str(&arguments)
                    .map_err(|error| CommandError::InvalidArguments(error.to_string()))?;
                required(&input.target, "target")?;
                required(&input.task, "task")?;
                let (started, _) = self
                    .runtime
                    .follow_up(&owner, &input.target, input.task)
                    .await
                    .map_err(CommandError::Execution)?;
                if started {
                    "Follow-up started in the existing agent session."
                } else {
                    "Follow-up delivered to the running agent."
                }
            }
        };
        context.ui.notify(NoticeLevel::Info, notice)?;
        Ok(CommandOutcome::Handled)
    }
}
