//! Non-destructive import bridge from coding-agent v1-v3 JSONL into the v4
//! harness journal used by `pi-session`.
//!
//! The live storage codec intentionally remains v4-only. Compatibility is
//! concentrated here so every caller gets the same migration, validation, and
//! preservation rules without weakening normal resume semantics.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use pi_core::{ProviderId, Usage, UsageCost};
use serde::Serialize;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{
    AgentMessage, BranchSummaryEntry, CompactionEntry, CustomEntry, CustomMessageEntry, HeaderKind,
    MAIN_LANE, MessageEntry, ModelChangeEntry, SESSION_SCHEMA_VERSION, SessionEntry, SessionError,
    SessionFact, SessionHeader, SessionLog, SessionMutation, SessionRecord, ThinkingLevelEntry,
};

const LEGACY_HEADER_TYPE: &str = "session";
const IMPORT_METADATA_KEY: &str = "piCodingAgentImport";
const LEGACY_UNKNOWN_PREFIX: &str = "pi.coding-agent.legacy";

/// Session formats accepted by the import boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionFileFormat {
    V4,
    Legacy { version: u32 },
}

/// Information about a completed, non-destructive session import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacySessionImportReport {
    pub source_format: SessionFileFormat,
    pub destination: PathBuf,
    pub entry_count: usize,
}

/// Inspects only the first physical line. Normal v4 decoding remains strict;
/// this function merely routes a file to the correct import codec.
pub fn inspect_session_file(path: &Path) -> Result<SessionFileFormat, SessionError> {
    let file = File::open(path)?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line)?;
    if line.trim().is_empty() {
        return Err(SessionError::MissingHeader);
    }
    let value: Value =
        serde_json::from_str(line.trim_end()).map_err(|error| SessionError::InvalidJson {
            line: 1,
            message: error.to_string(),
        })?;
    if value.get("kind").and_then(Value::as_str) == Some("header")
        && value.get("version").and_then(Value::as_u64) == Some(u64::from(SESSION_SCHEMA_VERSION))
    {
        return Ok(SessionFileFormat::V4);
    }
    if value.get("type").and_then(Value::as_str) == Some(LEGACY_HEADER_TYPE) {
        let version = value.get("version").and_then(Value::as_u64).unwrap_or(1);
        let version =
            u32::try_from(version).map_err(|_| SessionError::UnsupportedSchema(u32::MAX))?;
        if (1..=3).contains(&version) {
            return Ok(SessionFileFormat::Legacy { version });
        }
        return Err(SessionError::UnsupportedSchema(version));
    }
    Err(SessionError::MissingHeader)
}

/// Copies a v4 source or migrates a coding-agent v1-v3 source into a new v4
/// destination. The source is never opened for writing. The destination must
/// not exist and is removed when conversion or validation fails.
pub fn import_session_file(
    source: &Path,
    destination: &Path,
) -> Result<LegacySessionImportReport, SessionError> {
    let format = inspect_session_file(source)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if destination.exists() {
        return Err(SessionError::AlreadyExists(
            destination.display().to_string(),
        ));
    }

    let entry_count = match format {
        SessionFileFormat::V4 => {
            std::fs::copy(source, destination)?;
            match SessionLog::open(destination) {
                Ok((_, document)) => document.entries.len(),
                Err(error) => {
                    let _ = std::fs::remove_file(destination);
                    return Err(error);
                }
            }
        }
        SessionFileFormat::Legacy { version } => {
            match migrate_legacy(source, destination, version) {
                Ok(count) => count,
                Err(error) => {
                    let _ = std::fs::remove_file(destination);
                    return Err(error);
                }
            }
        }
    };
    Ok(LegacySessionImportReport {
        source_format: format,
        destination: destination.to_path_buf(),
        entry_count,
    })
}

fn migrate_legacy(source: &Path, destination: &Path, version: u32) -> Result<usize, SessionError> {
    let mut values = read_legacy_lines(source)?;
    let Some(header_value) = values.first().cloned() else {
        return Err(SessionError::MissingHeader);
    };
    require_legacy_header(&header_value, version)?;
    if version == 1 {
        migrate_v1_tree(&mut values)?;
    }
    if version <= 2 {
        migrate_hook_message_roles(&mut values);
    }

    let header = convert_header(&header_value, version)?;
    let raw_entries = &values[1..];
    validate_legacy_tree(raw_entries)?;
    let raw_by_id = raw_entries
        .iter()
        .map(|entry| Ok((entry_id(entry)?.to_string(), entry)))
        .collect::<Result<HashMap<_, _>, SessionError>>()?;

    let mut mutations = Vec::new();
    let mut facts = Vec::new();
    let mut seq = 1u64;
    for raw in raw_entries {
        let id = entry_id(raw)?.to_string();
        let parent_id = optional_string(raw, "parentId")?.map(str::to_string);
        let timestamp_ms = legacy_timestamp(raw, "timestamp")?;
        let entry = convert_entry(raw, &raw_by_id)?;
        mutations.push(SessionMutation::Entry {
            lane: None,
            record: SessionRecord {
                id,
                seq,
                parent_id,
                timestamp_ms,
                entry,
            },
        });
        seq = seq.saturating_add(1);
        collect_fact(raw, &mut facts)?;
    }

    mutations.push(SessionMutation::Lane {
        seq,
        lane: MAIN_LANE.to_string(),
        leaf_id: raw_entries
            .last()
            .map(entry_id)
            .transpose()?
            .map(str::to_string),
    });
    seq = seq.saturating_add(1);
    for fact in facts {
        mutations.push(SessionMutation::Fact { seq, fact });
        seq = seq.saturating_add(1);
    }

    write_v4(destination, &header, &mutations)?;
    SessionLog::open(destination)?;
    Ok(raw_entries.len())
}

fn read_legacy_lines(path: &Path) -> Result<Vec<Value>, SessionError> {
    let file = File::open(path)?;
    let mut values = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value =
            serde_json::from_str::<Value>(&line).map_err(|error| SessionError::InvalidJson {
                line: index + 1,
                message: error.to_string(),
            })?;
        if !value.is_object() {
            return Err(SessionError::InvalidJson {
                line: index + 1,
                message: "legacy session item is not a JSON object".to_string(),
            });
        }
        values.push(value);
    }
    Ok(values)
}

fn require_legacy_header(value: &Value, expected_version: u32) -> Result<(), SessionError> {
    if value.get("type").and_then(Value::as_str) != Some(LEGACY_HEADER_TYPE) {
        return Err(SessionError::MissingHeader);
    }
    require_string(value, "id")?;
    require_string(value, "cwd")?;
    let version = value.get("version").and_then(Value::as_u64).unwrap_or(1);
    if version != u64::from(expected_version) {
        return Err(SessionError::UnsupportedSchema(
            u32::try_from(version).unwrap_or(u32::MAX),
        ));
    }
    Ok(())
}

fn migrate_v1_tree(values: &mut [Value]) -> Result<(), SessionError> {
    let mut previous = None::<String>;
    for (index, entry) in values.iter_mut().enumerate().skip(1) {
        let object = entry.as_object_mut().ok_or_else(|| {
            SessionError::InvalidEntry(format!("legacy item {index} is not an object"))
        })?;
        let id = format!("legacy-{index:08x}");
        object.insert("id".to_string(), Value::String(id.clone()));
        object.insert(
            "parentId".to_string(),
            previous.clone().map_or(Value::Null, Value::String),
        );
        previous = Some(id);
    }
    let index_ids = values
        .iter()
        .map(|value| value.get("id").and_then(Value::as_str).map(str::to_string))
        .collect::<Vec<_>>();
    for entry in values.iter_mut().skip(1) {
        if entry.get("type").and_then(Value::as_str) != Some("compaction") {
            continue;
        }
        let kept_index = entry.get("firstKeptEntryIndex").and_then(Value::as_u64);
        if let Some(kept_index) = kept_index {
            let kept_index = usize::try_from(kept_index).map_err(|_| {
                SessionError::InvalidEntry("legacy compaction index is too large".to_string())
            })?;
            let kept_id = index_ids
                .get(kept_index)
                .and_then(Option::as_deref)
                .ok_or_else(|| {
                    SessionError::InvalidEntry(format!(
                        "legacy compaction references missing entry index {kept_index}"
                    ))
                })?;
            let object = entry
                .as_object_mut()
                .expect("legacy entry object was checked");
            object.insert(
                "firstKeptEntryId".to_string(),
                Value::String(kept_id.to_string()),
            );
            object.remove("firstKeptEntryIndex");
        }
    }
    Ok(())
}

fn migrate_hook_message_roles(values: &mut [Value]) {
    for entry in values.iter_mut().skip(1) {
        if entry.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        if let Some(message) = entry.get_mut("message").and_then(Value::as_object_mut)
            && message.get("role").and_then(Value::as_str) == Some("hookMessage")
        {
            message.insert("role".to_string(), Value::String("custom".to_string()));
        }
    }
}

fn convert_header(value: &Value, version: u32) -> Result<SessionHeader, SessionError> {
    let parent = value
        .get("parentSession")
        .or_else(|| value.get("branchedFrom"))
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let mut metadata = Map::new();
    metadata.insert(
        IMPORT_METADATA_KEY.to_string(),
        json!({
            "sourceVersion": version,
            "sourceHeader": value,
        }),
    );
    Ok(SessionHeader {
        kind: HeaderKind::Header,
        version: SESSION_SCHEMA_VERSION,
        id: require_string(value, "id")?.to_string(),
        created_at: legacy_timestamp(value, "timestamp")?,
        cwd: PathBuf::from(require_string(value, "cwd")?),
        parent_session_id: None,
        legacy_parent_session_path: parent,
        metadata: Some(metadata),
    })
}

fn validate_legacy_tree(entries: &[Value]) -> Result<(), SessionError> {
    let mut seen = HashSet::new();
    for entry in entries {
        let id = entry_id(entry)?;
        if !seen.insert(id.to_string()) {
            return Err(SessionError::AlreadyExists(id.to_string()));
        }
        if let Some(parent) = optional_string(entry, "parentId")?
            && !seen.contains(parent)
        {
            return Err(SessionError::InvalidEntry(format!(
                "legacy entry {id} references missing or later parent {parent}"
            )));
        }
        legacy_timestamp(entry, "timestamp")?;
    }
    Ok(())
}

fn convert_entry(
    raw: &Value,
    raw_by_id: &HashMap<String, &Value>,
) -> Result<SessionEntry, SessionError> {
    let entry_type = require_string(raw, "type")?;
    match entry_type {
        "message" => {
            let mut message = raw.get("message").cloned().ok_or_else(|| {
                SessionError::InvalidEntry("message is missing payload".to_string())
            })?;
            normalize_agent_message(&mut message)?;
            Ok(SessionEntry::Message(MessageEntry {
                message: AgentMessage::custom(message)?,
                terminate: false,
            }))
        }
        "thinking_level_change" => Ok(SessionEntry::ThinkingLevelChange(ThinkingLevelEntry {
            thinking_level: require_string(raw, "thinkingLevel")?.to_string(),
        })),
        "model_change" => Ok(SessionEntry::ModelChange(ModelChangeEntry {
            provider: ProviderId::new(require_string(raw, "provider")?),
            model_id: require_string(raw, "modelId")?.into(),
        })),
        "compaction" => Ok(SessionEntry::Compaction(CompactionEntry {
            summary: require_string(raw, "summary")?.to_string(),
            retained_tail: retained_tail(raw, raw_by_id)?,
            tokens_before: require_u64(raw, "tokensBefore")?,
            details: raw.get("details").cloned(),
            usage: raw.get("usage").map(parse_usage).transpose()?,
        })),
        "branch_summary" => Ok(SessionEntry::BranchSummary(BranchSummaryEntry {
            from_id: require_string(raw, "fromId")?.to_string(),
            summary: require_string(raw, "summary")?.to_string(),
            details: raw.get("details").cloned(),
            usage: raw.get("usage").map(parse_usage).transpose()?,
        })),
        "custom" => Ok(SessionEntry::Custom(CustomEntry {
            custom_type: require_string(raw, "customType")?.to_string(),
            data: raw.get("data").cloned(),
        })),
        "custom_message" => Ok(SessionEntry::CustomMessage(CustomMessageEntry {
            custom_type: require_string(raw, "customType")?.to_string(),
            content: serde_json::from_value(
                raw.get("content").cloned().unwrap_or_else(|| json!([])),
            )
            .map_err(|error| SessionError::InvalidPayload(error.to_string()))?,
            display: raw.get("display").and_then(Value::as_bool).unwrap_or(false),
            details: raw.get("details").cloned(),
        })),
        "label" | "session_info" => Ok(legacy_metadata_entry(entry_type, raw)),
        other => Ok(SessionEntry::Custom(CustomEntry {
            custom_type: format!("{LEGACY_UNKNOWN_PREFIX}.{other}"),
            data: Some(raw.clone()),
        })),
    }
}

fn legacy_metadata_entry(entry_type: &str, raw: &Value) -> SessionEntry {
    SessionEntry::Custom(CustomEntry {
        custom_type: format!("{LEGACY_UNKNOWN_PREFIX}.{entry_type}"),
        data: Some(raw.clone()),
    })
}

fn collect_fact(raw: &Value, facts: &mut Vec<SessionFact>) -> Result<(), SessionError> {
    match require_string(raw, "type")? {
        "label" => facts.push(SessionFact::Label {
            target_id: require_string(raw, "targetId")?.to_string(),
            label: optional_string(raw, "label")?.map(str::to_string),
        }),
        "session_info" => facts.push(SessionFact::Name {
            name: optional_string(raw, "name")?.map(str::to_string),
        }),
        _ => {}
    }
    Ok(())
}

fn retained_tail(
    compaction: &Value,
    raw_by_id: &HashMap<String, &Value>,
) -> Result<Vec<AgentMessage>, SessionError> {
    let first_kept = require_string(compaction, "firstKeptEntryId")?;
    let mut path = Vec::<&Value>::new();
    let mut current = optional_string(compaction, "parentId")?;
    let mut visited = HashSet::new();
    while let Some(id) = current {
        if !visited.insert(id.to_string()) {
            return Err(SessionError::InvalidEntry(format!(
                "cycle in legacy compaction ancestry at {id}"
            )));
        }
        let entry = raw_by_id.get(id).ok_or_else(|| {
            SessionError::InvalidEntry(format!("legacy compaction references missing parent {id}"))
        })?;
        path.push(*entry);
        current = optional_string(entry, "parentId")?;
    }
    path.reverse();
    let start = path
        .iter()
        .position(|entry| entry_id(entry).ok() == Some(first_kept))
        .ok_or_else(|| {
            SessionError::InvalidEntry(format!(
                "legacy compaction firstKeptEntryId {first_kept} is not an ancestor"
            ))
        })?;
    let mut messages = Vec::new();
    for entry in &path[start..] {
        messages.extend(legacy_context_messages(entry)?);
    }
    Ok(messages)
}

fn legacy_context_messages(raw: &Value) -> Result<Vec<AgentMessage>, SessionError> {
    let timestamp = legacy_timestamp(raw, "timestamp")?;
    match require_string(raw, "type")? {
        "message" => {
            let mut value = raw.get("message").cloned().ok_or_else(|| {
                SessionError::InvalidEntry("message is missing payload".to_string())
            })?;
            normalize_agent_message(&mut value)?;
            Ok(vec![AgentMessage::custom(value)?])
        }
        "custom_message" => Ok(vec![AgentMessage::custom(json!({
            "role": "custom",
            "customType": require_string(raw, "customType")?,
            "content": raw.get("content").cloned().unwrap_or_else(|| json!([])),
            "display": raw.get("display").and_then(Value::as_bool).unwrap_or(false),
            "details": raw.get("details").cloned(),
            "timestamp": timestamp,
        }))?]),
        "branch_summary" => Ok(vec![AgentMessage::custom(json!({
            "role": "branchSummary",
            "summary": require_string(raw, "summary")?,
            "fromId": require_string(raw, "fromId")?,
            "timestamp": timestamp,
        }))?]),
        "compaction" => Ok(vec![AgentMessage::custom(json!({
            "role": "compactionSummary",
            "summary": require_string(raw, "summary")?,
            "tokensBefore": require_u64(raw, "tokensBefore")?,
            "timestamp": timestamp,
        }))?]),
        _ => Ok(Vec::new()),
    }
}

fn normalize_agent_message(value: &mut Value) -> Result<(), SessionError> {
    let object = value.as_object_mut().ok_or_else(|| {
        SessionError::InvalidPayload("legacy agent message is not an object".to_string())
    })?;
    if object.get("role").and_then(Value::as_str) == Some("hookMessage") {
        object.insert("role".to_string(), Value::String("custom".to_string()));
    }
    if object.get("role").and_then(Value::as_str) == Some("assistant") {
        let usage = object
            .entry("usage")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| {
                SessionError::InvalidPayload("assistant usage is not an object".to_string())
            })?;
        for field in ["input", "output", "cacheRead", "cacheWrite"] {
            usage.entry(field.to_string()).or_insert(json!(0));
        }
        if !usage.contains_key("totalTokens") {
            let total = ["input", "output", "cacheRead", "cacheWrite"]
                .iter()
                .filter_map(|field| usage.get(*field).and_then(Value::as_u64))
                .fold(0u64, u64::saturating_add);
            usage.insert("totalTokens".to_string(), json!(total));
        }
        if let Some(cost) = usage.get_mut("cost") {
            normalize_cost(cost)?;
        }
    }
    Ok(())
}

fn parse_usage(value: &Value) -> Result<Usage, SessionError> {
    let object = value
        .as_object()
        .ok_or_else(|| SessionError::InvalidPayload("legacy usage is not an object".to_string()))?;
    let input = optional_u64(object, "input")?;
    let output = optional_u64(object, "output")?;
    let cache_read = optional_u64(object, "cacheRead")?;
    let cache_write = optional_u64(object, "cacheWrite")?;
    Ok(Usage {
        input,
        output,
        cache_read,
        cache_write,
        cache_write_1h: object.get("cacheWrite1h").map(value_u64).transpose()?,
        reasoning: object.get("reasoning").map(value_u64).transpose()?,
        total_tokens: object
            .get("totalTokens")
            .map(value_u64)
            .transpose()?
            .unwrap_or_else(|| {
                input
                    .saturating_add(output)
                    .saturating_add(cache_read)
                    .saturating_add(cache_write)
            }),
        cost: object
            .get("cost")
            .map(parse_cost)
            .transpose()?
            .unwrap_or_default(),
    })
}

fn parse_cost(value: &Value) -> Result<UsageCost, SessionError> {
    let object = value.as_object().ok_or_else(|| {
        SessionError::InvalidPayload("legacy usage cost is not an object".to_string())
    })?;
    Ok(UsageCost {
        input: optional_f64(object, "input")?,
        output: optional_f64(object, "output")?,
        cache_read: optional_f64(object, "cacheRead")?,
        cache_write: optional_f64(object, "cacheWrite")?,
        total: optional_f64(object, "total")?,
    })
}

fn normalize_cost(value: &mut Value) -> Result<(), SessionError> {
    let object = value.as_object_mut().ok_or_else(|| {
        SessionError::InvalidPayload("assistant usage cost is not an object".to_string())
    })?;
    for field in ["input", "output", "cacheRead", "cacheWrite", "total"] {
        object.entry(field.to_string()).or_insert(json!(0.0));
    }
    Ok(())
}

fn write_v4(
    path: &Path,
    header: &SessionHeader,
    mutations: &[SessionMutation],
) -> Result<(), SessionError> {
    let file = OpenOptions::new().create_new(true).write(true).open(path)?;
    let mut writer = BufWriter::new(file);
    write_line(&mut writer, header, 1)?;
    for (index, mutation) in mutations.iter().enumerate() {
        write_line(&mut writer, mutation, index + 2)?;
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(())
}

fn write_line(
    writer: &mut BufWriter<File>,
    value: &impl Serialize,
    line: usize,
) -> Result<(), SessionError> {
    serde_json::to_writer(&mut *writer, value).map_err(|error| SessionError::InvalidJson {
        line,
        message: error.to_string(),
    })?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn legacy_timestamp(value: &Value, field: &str) -> Result<i64, SessionError> {
    let timestamp = value.get(field).ok_or_else(|| {
        SessionError::InvalidPayload(format!("legacy session is missing {field}"))
    })?;
    if let Some(milliseconds) = timestamp.as_i64() {
        return Ok(milliseconds);
    }
    let text = timestamp.as_str().ok_or_else(|| {
        SessionError::InvalidPayload(format!("legacy {field} is not an ISO timestamp"))
    })?;
    let parsed = OffsetDateTime::parse(text, &Rfc3339).map_err(|error| {
        SessionError::InvalidPayload(format!("invalid legacy {field}: {error}"))
    })?;
    i64::try_from(parsed.unix_timestamp_nanos() / 1_000_000).map_err(|_| {
        SessionError::InvalidPayload(format!("legacy {field} is outside the supported range"))
    })
}

fn entry_id(value: &Value) -> Result<&str, SessionError> {
    require_string(value, "id")
}

fn require_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, SessionError> {
    value.get(field).and_then(Value::as_str).ok_or_else(|| {
        SessionError::InvalidPayload(format!("legacy session field {field} is not a string"))
    })
}

fn optional_string<'a>(value: &'a Value, field: &str) -> Result<Option<&'a str>, SessionError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_str().map(Some).ok_or_else(|| {
            SessionError::InvalidPayload(format!("legacy session field {field} is not a string"))
        }),
    }
}

fn require_u64(value: &Value, field: &str) -> Result<u64, SessionError> {
    value
        .get(field)
        .map(value_u64)
        .transpose()?
        .ok_or_else(|| SessionError::InvalidPayload(format!("legacy session is missing {field}")))
}

fn value_u64(value: &Value) -> Result<u64, SessionError> {
    value.as_u64().ok_or_else(|| {
        SessionError::InvalidPayload(
            "legacy numeric value is not a non-negative integer".to_string(),
        )
    })
}

fn optional_u64(object: &Map<String, Value>, field: &str) -> Result<u64, SessionError> {
    object
        .get(field)
        .map(value_u64)
        .transpose()
        .map(Option::unwrap_or_default)
}

fn optional_f64(object: &Map<String, Value>, field: &str) -> Result<f64, SessionError> {
    object
        .get(field)
        .map(|value| {
            value.as_f64().ok_or_else(|| {
                SessionError::InvalidPayload("legacy cost value is not numeric".to_string())
            })
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::{SessionContextBuildOptions, build_session_context};

    fn write_legacy(path: &Path, values: &[Value]) {
        let contents = values
            .iter()
            .map(|value| serde_json::to_string(value).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(path, format!("{contents}\n")).unwrap();
    }

    #[test]
    fn imports_v1_tree_and_preserves_compaction_context() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("v1.jsonl");
        let destination = directory.path().join("v4.jsonl");
        write_legacy(
            &source,
            &[
                json!({"type":"session","id":"old","timestamp":"2025-01-01T00:00:00Z","cwd":"/tmp"}),
                json!({"type":"message","timestamp":"2025-01-01T00:00:01Z","message":{"role":"user","content":"one","timestamp":1}}),
                json!({"type":"message","timestamp":"2025-01-01T00:00:02Z","message":{"role":"hookMessage","content":"kept","timestamp":2,"extensionField":{"x":1}}}),
                json!({"type":"compaction","timestamp":"2025-01-01T00:00:03Z","summary":"summary","firstKeptEntryIndex":2,"tokensBefore":9}),
                json!({"type":"message","timestamp":"2025-01-01T00:00:04Z","message":{"role":"user","content":"after","timestamp":4}}),
            ],
        );
        let original = fs::read(&source).unwrap();

        let report = import_session_file(&source, &destination).unwrap();
        let (_, document) = SessionLog::open(&destination).unwrap();
        let context = build_session_context(
            &document
                .branch()
                .unwrap()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>(),
            &SessionContextBuildOptions::default(),
        );

        assert_eq!(
            report.source_format,
            SessionFileFormat::Legacy { version: 1 }
        );
        assert_eq!(document.entries[0].id, "legacy-00000001");
        assert_eq!(
            document.entries[1].parent_id.as_deref(),
            Some("legacy-00000001")
        );
        assert_eq!(context.messages.len(), 3);
        assert_eq!(context.messages[1].role(), "custom");
        assert_eq!(context.messages[2].role(), "user");
        assert_eq!(fs::read(&source).unwrap(), original);
    }

    #[test]
    fn imports_v3_tree_facts_custom_messages_and_unknown_message_fields() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("v3.jsonl");
        let destination = directory.path().join("v4.jsonl");
        write_legacy(
            &source,
            &[
                json!({"type":"session","version":3,"id":"old","timestamp":"2025-01-01T00:00:00Z","cwd":"/tmp","parentSession":"/tmp/parent.jsonl"}),
                json!({"type":"message","id":"a","parentId":null,"timestamp":"2025-01-01T00:00:01Z","message":{"role":"user","content":"hello","timestamp":1,"extensionField":{"x":1}}}),
                json!({"type":"custom_message","id":"b","parentId":"a","timestamp":"2025-01-01T00:00:02Z","customType":"fixture","content":"context","display":true,"details":{"y":2}}),
                json!({"type":"label","id":"c","parentId":"b","timestamp":"2025-01-01T00:00:03Z","targetId":"a","label":"start"}),
                json!({"type":"session_info","id":"d","parentId":"c","timestamp":"2025-01-01T00:00:04Z","name":"Imported"}),
            ],
        );

        import_session_file(&source, &destination).unwrap();
        let (_, document) = SessionLog::open(&destination).unwrap();

        assert_eq!(document.entries.len(), 4);
        assert_eq!(document.entries[3].parent_id.as_deref(), Some("c"));
        assert_eq!(document.labels.get("a").map(String::as_str), Some("start"));
        assert_eq!(document.name.as_deref(), Some("Imported"));
        assert_eq!(
            document.header.legacy_parent_session_path.as_deref(),
            Some(Path::new("/tmp/parent.jsonl"))
        );
        assert_eq!(
            serde_json::to_value(document.messages().remove(0)).unwrap()["extensionField"],
            json!({"x":1})
        );
    }

    #[test]
    fn rejects_invalid_middle_line_without_leaving_destination() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("bad.jsonl");
        let destination = directory.path().join("v4.jsonl");
        fs::write(
            &source,
            "{\"type\":\"session\",\"version\":3,\"id\":\"old\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\nnot json\n",
        )
        .unwrap();

        assert!(import_session_file(&source, &destination).is_err());
        assert!(!destination.exists());
    }
}
