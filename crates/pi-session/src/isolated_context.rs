//! Durable context initialization without importing ancestor configuration or usage.

use pi_core::Message;
use serde::{Deserialize, Serialize};

use crate::{AgentMessage, SessionEntry, SessionRecord};

pub(crate) const CUSTOM_TYPE: &str = "pi.isolated_context";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IsolatedContextSeed {
    pub parent_session_id: String,
    pub messages: Vec<AgentMessage>,
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
