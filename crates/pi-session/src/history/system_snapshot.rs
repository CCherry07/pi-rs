//! Fold transcript system deltas without losing tool declarations or section order.
use pi_core::session::SystemMessageSnapshot;
use serde_json::{Map, Value, json};

use super::{AgentMessage, HistoryError};

pub(crate) fn current_system_snapshot(
    messages: &[AgentMessage],
) -> Result<Option<SystemMessageSnapshot>, HistoryError> {
    let mut content = Vec::new();
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut tools: Vec<Value> = Vec::new();
    let mut timestamp = None;
    for message in messages.iter().filter(|message| message.role() == "system") {
        let value = serde_json::to_value(message)
            .map_err(|error| HistoryError::InvalidPayload(error.to_string()))?;
        timestamp = timestamp.or_else(|| value["timestamp"].as_i64());
        let text = match &value["content"] {
            Value::String(text) => text.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter(|block| block["type"] == "text")
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !text.is_empty() {
            content.push(text);
        }
        if let Some(values) = value["sections"].as_object() {
            for (name, value) in values {
                if value.is_null() {
                    sections.retain(|(key, _)| key != name);
                } else if let Some(text) = value.as_str() {
                    if let Some((_, previous)) = sections.iter_mut().find(|(key, _)| key == name) {
                        *previous = text.to_string();
                    } else {
                        sections.push((name.clone(), text.to_string()));
                    }
                }
            }
        }
        for key in ["toolsRemoved", "toolsAdded"] {
            let Some(value) = value.get(key) else {
                continue;
            };
            if value.is_null() {
                continue;
            }
            let values = value.as_array().ok_or_else(|| {
                HistoryError::InvalidPayload(format!("system {key} must be an array"))
            })?;
            for tool in values {
                let name = tool.get("name").and_then(Value::as_str).ok_or_else(|| {
                    HistoryError::InvalidPayload(format!("system {key} item requires a name"))
                })?;
                if key == "toolsRemoved" {
                    tools.retain(|existing| existing["name"] != name);
                } else if let Some(existing) =
                    tools.iter_mut().find(|existing| existing["name"] == name)
                {
                    *existing = tool.clone();
                } else {
                    tools.push(tool.clone());
                }
            }
        }
    }
    let Some(timestamp) = timestamp else {
        return Ok(None);
    };
    let mut result = json!({"role":"system","content":content.join("\n\n"),"timestamp":timestamp});
    if !sections.is_empty() {
        result["sections"] = Value::Object(
            sections
                .into_iter()
                .map(|(name, text)| (name, Value::String(text)))
                .collect::<Map<_, _>>(),
        );
    }
    if !tools.is_empty() {
        result["toolsAdded"] = json!(tools);
    }
    let message = AgentMessage::custom(result)
        .map_err(|error| HistoryError::InvalidPayload(error.to_string()))?;
    SystemMessageSnapshot::new(message)
        .map(Some)
        .map_err(|error| HistoryError::InvalidPayload(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_snapshot_matches_actual_legacy_fold() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/rpc-compaction-oracle.json"
        ))
        .unwrap();
        let messages: Vec<AgentMessage> =
            serde_json::from_value(fixture["systemFold"]["messages"].clone()).unwrap();
        let actual = current_system_snapshot(&messages).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            fixture["systemFold"]["expected"]
        );
    }
}
