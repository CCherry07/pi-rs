use pi_plugin::{PluginContextError, ToolContext, ToolError};
use serde_json::{Map, Value};

pub(crate) const STORE_ENTRY_TYPE: &str = "codemode-store";
pub(crate) const MAX_STORE_BYTES: usize = 1024 * 1024;

pub(crate) fn read(context: &ToolContext) -> Result<Map<String, Value>, ToolError> {
    let snapshot = match context.session.snapshot() {
        Ok(snapshot) => snapshot,
        Err(PluginContextError::Unbound | PluginContextError::Unavailable(_)) => {
            return Ok(Map::new());
        }
        Err(error) => return Err(error.into()),
    };
    let mut values = Map::new();
    for entry in snapshot.branch() {
        let raw = entry.raw();
        if raw["type"] != "custom" || raw["customType"] != STORE_ENTRY_TYPE {
            continue;
        }
        let data = &raw["data"];
        let (Some(set), Some(delete)) = (data["set"].as_object(), data["delete"].as_array()) else {
            continue;
        };
        if !delete.iter().all(Value::is_string) {
            continue;
        }
        for key in delete.iter().filter_map(Value::as_str) {
            values.remove(key);
        }
        values.extend(set.clone());
    }
    if serde_json::to_vec(&values).map_or(true, |data| data.len() > MAX_STORE_BYTES) {
        return Err(ToolError::Execution(
            "codemode store exceeds the 1 MiB limit".into(),
        ));
    }
    Ok(values)
}

pub(crate) fn persist(context: &ToolContext, writes: Value) -> Result<(), ToolError> {
    let set = writes["set"]
        .as_object()
        .ok_or_else(|| ToolError::Execution("Invalid codemode store writes".into()))?;
    let delete = writes["delete"]
        .as_array()
        .ok_or_else(|| ToolError::Execution("Invalid codemode store deletes".into()))?;
    if !delete.iter().all(Value::is_string) {
        return Err(ToolError::Execution(
            "Invalid codemode store deletes".into(),
        ));
    }
    if set.is_empty() && delete.is_empty() {
        return Ok(());
    }
    // Enforce the final byte budget on the authoritative branch, independent
    // of JavaScript string lengths or script mutations to built-in methods.
    let mut next = read(context)?;
    for key in delete.iter().filter_map(Value::as_str) {
        next.remove(key);
    }
    next.extend(set.clone());
    if serde_json::to_vec(&next).map_or(true, |data| data.len() > MAX_STORE_BYTES) {
        return Err(ToolError::Execution(
            "codemode store exceeds the 1 MiB limit".into(),
        ));
    }
    match context.session.append_entry(STORE_ENTRY_TYPE, Some(writes)) {
        Ok(()) | Err(PluginContextError::Unbound | PluginContextError::Unavailable(_)) => Ok(()),
        Err(error) => Err(error.into()),
    }
}
