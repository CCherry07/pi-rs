//! Lightweight decoding for the built-in, read-only transcript projection.

use serde::Deserialize;
use serde_json::value::RawValue;

use crate::history::{CustomEntry, SessionEntry, SessionMutation, SessionRecord};

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum EntryKind {
    Entry,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CustomKind {
    Custom,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CustomMutation<'a> {
    #[serde(rename = "kind")]
    _kind: EntryKind,
    #[serde(rename = "type")]
    _entry_type: CustomKind,
    seq: u64,
    #[serde(
        default,
        deserialize_with = "crate::history::types::strict_optional::deserialize"
    )]
    lane: Option<String>,
    id: String,
    #[serde(deserialize_with = "crate::history::types::required_nullable::deserialize")]
    parent_id: Option<String>,
    #[serde(rename = "timestamp", with = "crate::history::types::iso_timestamp_ms")]
    timestamp_ms: i64,
    custom_type: String,
    #[serde(borrow)]
    data: Option<&'a RawValue>,
}

pub(super) fn decode_custom_mutation(line: &[u8]) -> Result<SessionMutation, serde_json::Error> {
    let custom: CustomMutation<'_> = serde_json::from_slice(line)?;
    // Native provenance participates in the default context. All other custom
    // state is for extension projectors/runtime restoration, never display.
    // RawValue scans JSON syntax without allocating the opaque payload tree.
    let data = if matches!(
        custom.custom_type.as_str(),
        crate::history::ISOLATED_CONTEXT_TYPE | crate::history::ISOLATED_ORIGIN_TYPE
    ) {
        custom
            .data
            .map(|data| serde_json::from_str(data.get()))
            .transpose()?
    } else {
        None
    };
    Ok(SessionMutation::Entry {
        lane: custom.lane,
        record: SessionRecord {
            id: custom.id,
            seq: custom.seq,
            parent_id: custom.parent_id,
            timestamp_ms: custom.timestamp_ms,
            entry: SessionEntry::Custom(CustomEntry {
                custom_type: custom.custom_type,
                data,
            }),
        },
    })
}
