//! Durable context initialization without importing ancestor configuration or usage.

use pi_core::Message;
use serde::{Deserialize, Serialize};

use crate::{AgentMessage, SessionEntry, SessionRecord};

pub(crate) const CUSTOM_TYPE: &str = "pi.isolated_context";
pub(crate) const ORIGIN_CUSTOM_TYPE: &str = "pi.isolated_origin";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IsolatedContextSeed {
    pub parent_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_entry_id: Option<String>,
    pub messages: Vec<AgentMessage>,
}

/// The immutable context inherited when an isolated child was created.
/// This is separate from the child's own messages and survives later compaction.
#[derive(Debug, Clone)]
pub struct InheritedSessionContext {
    pub parent_session_id: String,
    /// Older seeds did not record the parent entry boundary.
    pub parent_entry_id: Option<String>,
    pub snapshot_entry_id: String,
    pub messages: Vec<AgentMessage>,
}

impl crate::SessionDocument {
    /// Immediate parent identity for an isolated session, including fresh children.
    pub fn isolated_parent_session_id(&self) -> Result<Option<String>, crate::SessionError> {
        Ok(self.branch()?.into_iter().find_map(|record| {
            if let Some(seed) = read_seed(&record.entry) {
                return Some(seed.parent_session_id);
            }
            let SessionEntry::Custom(custom) = &record.entry else {
                return None;
            };
            (custom.custom_type == ORIGIN_CUSTOM_TYPE).then_some(())?;
            custom
                .data
                .as_ref()?
                .get("parentSessionId")?
                .as_str()
                .map(str::to_owned)
        }))
    }

    pub fn inherited_context(
        &self,
    ) -> Result<Option<InheritedSessionContext>, crate::SessionError> {
        Ok(self.branch()?.into_iter().find_map(|record| {
            let seed = read_seed(&record.entry)?;
            Some(InheritedSessionContext {
                parent_session_id: seed.parent_session_id,
                parent_entry_id: seed.parent_entry_id,
                snapshot_entry_id: record.id.clone(),
                messages: seed.messages,
            })
        }))
    }
}

pub(crate) fn is_seed(entry: &SessionEntry) -> bool {
    matches!(entry, SessionEntry::Custom(custom) if custom.custom_type == CUSTOM_TYPE)
}

pub(crate) fn read_seed(entry: &SessionEntry) -> Option<IsolatedContextSeed> {
    let SessionEntry::Custom(custom) = entry else {
        return None;
    };
    if custom.custom_type != CUSTOM_TYPE {
        return None;
    }
    serde_json::from_value(custom.data.as_ref()?.clone()).ok()
}

/// MessageEnd persistence is awaited before tool dispatch. Even if earlier
/// siblings have finished, the last non-result message is the requesting
/// assistant. Excluding that entire batch gives every sibling the same prefix.
pub(crate) fn fork_entries<'a>(
    entries: &'a [SessionRecord],
    active_request: Option<&Message>,
) -> &'a [SessionRecord] {
    if let Some(request @ Message::Assistant(assistant)) = active_request
        && !assistant.tool_calls().is_empty()
        && let Some(index) = entries.iter().rposition(|record| {
            matches!(&record.entry, SessionEntry::Message(entry) if entry.message.as_standard() == Some(request))
        })
    {
        return &entries[..index];
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_seed_without_parent_boundary_keeps_the_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let log = crate::SessionLog::create(
            directory.path().join("child.jsonl"),
            crate::SessionHeader::new("child", directory.path()),
        )
        .unwrap();
        let record = log
            .append_session_record(SessionEntry::Custom(crate::CustomEntry {
                custom_type: CUSTOM_TYPE.into(),
                data: Some(serde_json::json!({
                    "parentSessionId": "parent",
                    "messages": [],
                })),
            }))
            .unwrap();
        let document = log.load().unwrap();
        let inherited = document.inherited_context().unwrap().unwrap();
        assert_eq!(inherited.parent_session_id, "parent");
        assert_eq!(inherited.parent_entry_id, None);
        assert_eq!(inherited.snapshot_entry_id, record.id);
        assert!(inherited.messages.is_empty());
        assert_eq!(
            document.isolated_parent_session_id().unwrap().as_deref(),
            Some("parent")
        );
    }
}
