#![forbid(unsafe_code)]

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use pi_core::{
    AgentPlugin, PluginId, RegisterContext, Tool, ToolCallId, ToolContext, ToolError, ToolResult,
    ToolSpec, ToolUpdateSink,
};
use pi_tool_support::{
    MAX_WRITE_BYTES, execution, hashline_tag, invalid, require_str, resolve_to_cwd,
    snapshot_and_atomic_replace, spec,
};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};

pub struct HashlineEditPlugin;
pub struct HashlineEditTool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Operation {
    Replace,
    Prepend,
    Append,
}

impl Operation {
    const fn precedence(self) -> u8 {
        match self {
            Self::Replace => 0,
            Self::Append => 1,
            Self::Prepend => 2,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditOp {
    op: Operation,
    pos: Option<String>,
    end: Option<String>,
    lines: Option<Value>,
}
struct Resolved {
    op: Operation,
    start: usize,
    end: usize,
    lines: Vec<String>,
}

#[pi_core::agent_plugin]
impl AgentPlugin for HashlineEditPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("hashline-edit-tool")
    }
    fn register(&self, c: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        c.register_tool(Arc::new(HashlineEditTool))
    }
}

#[async_trait]
impl Tool for HashlineEditTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "hashline_edit",
            "Apply stale-safe replace/prepend/append edits using LINE#HASH anchors from read(hashline=true).",
            json!({
                "type":"object","properties":{"path":{"type":"string","description":"Path to the file to edit (relative or absolute)"},"edits":{"type":"array","items":{"type":"object","properties":{
                    "op":{"type":"string","enum":["replace","prepend","append"]},"pos":{"type":"string"},"end":{"type":"string"},"lines":{}
                },"required":["op"],"additionalProperties":false}}},"required":["path","edits"],"additionalProperties":false
            }),
        )
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context
            .abort_signal
            .check()
            .map_err(|_| ToolError::Aborted)?;
        let requested = require_str(&input, "path")?;
        let edits: Vec<EditOp> = serde_json::from_value(
            input
                .get("edits")
                .cloned()
                .ok_or_else(|| invalid("edits is required"))?,
        )
        .map_err(|e| invalid(e.to_string()))?;
        if edits.is_empty() {
            return Err(invalid("no edits provided"));
        }
        let path = resolve_to_cwd(&context.cwd, requested)?;
        let metadata = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|e| execution(e.to_string()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(execution("target must be a regular non-symlink file"));
        }
        if metadata.len() > MAX_WRITE_BYTES as u64 {
            return Err(execution("file is too large"));
        }
        let original = tokio::fs::read(&path)
            .await
            .map_err(|e| execution(e.to_string()))?;
        let content =
            String::from_utf8(original.clone()).map_err(|_| execution("file is not UTF-8"))?;
        let had_bom = content.starts_with('\u{feff}');
        let without_bom = content.strip_prefix('\u{feff}').unwrap_or(&content);
        let ending = if without_bom.contains("\r\n") {
            "\r\n"
        } else if without_bom.contains('\r') {
            "\r"
        } else {
            "\n"
        };
        let normalized = without_bom.replace("\r\n", "\n").replace('\r', "\n");
        let trailing_newline = normalized.ends_with('\n');
        let mut file_lines = normalized.split('\n').map(String::from).collect::<Vec<_>>();
        if trailing_newline {
            file_lines.pop();
        }
        if file_lines.is_empty() {
            file_lines.push(String::new());
        }
        let mut resolved = Vec::new();
        let mut seen = HashSet::new();
        for edit in edits {
            context
                .abort_signal
                .check()
                .map_err(|_| ToolError::Aborted)?;
            let replacement = extract_lines(edit.lines.as_ref())
                .into_iter()
                .map(|line| strip_prefix(&line).to_string())
                .collect::<Vec<_>>();
            let start = match (&edit.pos, edit.op) {
                (Some(anchor), _) => validate_anchor(anchor, &file_lines, had_bom)?,
                (None, Operation::Prepend) => 0,
                (None, Operation::Append) => file_lines.len().saturating_sub(1),
                (None, Operation::Replace) => return Err(invalid("replace requires pos")),
            };
            let end = match &edit.end {
                Some(anchor) => validate_anchor(anchor, &file_lines, had_bom)?,
                None => start,
            };
            if end < start {
                return Err(invalid("end anchor precedes pos anchor"));
            }
            let key = (edit.op, start, end, replacement.clone());
            if seen.insert(key) {
                resolved.push(Resolved {
                    op: edit.op,
                    start,
                    end,
                    lines: replacement,
                });
            }
        }
        resolved.sort_by(|a, b| {
            b.start
                .cmp(&a.start)
                .then_with(|| a.op.precedence().cmp(&b.op.precedence()))
        });
        for i in 0..resolved.len() {
            for j in i + 1..resolved.len() {
                if resolved[i].start <= resolved[j].end && resolved[j].start <= resolved[i].end {
                    return Err(invalid("overlapping edits; combine them"));
                }
            }
        }
        let mut changed = false;
        for edit in resolved {
            match edit.op {
                Operation::Replace => {
                    if file_lines[edit.start..=edit.end] != edit.lines {
                        file_lines.splice(edit.start..=edit.end, edit.lines);
                        changed = true;
                    }
                }
                Operation::Prepend => {
                    if !edit.lines.is_empty() {
                        file_lines.splice(edit.start..edit.start, edit.lines);
                        changed = true;
                    }
                }
                Operation::Append => {
                    if !edit.lines.is_empty() {
                        file_lines.splice(edit.start + 1..edit.start + 1, edit.lines);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return Err(execution("no changes made; all edits were no-ops"));
        }
        let mut output = file_lines.join("\n");
        if trailing_newline {
            output.push('\n');
        }
        if ending != "\n" {
            output = output.replace('\n', ending);
        }
        if had_bom {
            output.insert(0, '\u{feff}');
        }
        if output.len() > MAX_WRITE_BYTES {
            return Err(execution("edited file is too large"));
        }
        context
            .abort_signal
            .check()
            .map_err(|_| ToolError::Aborted)?;
        let path_copy = path.clone();
        let bytes = output.into_bytes();
        tokio::task::spawn_blocking(move || {
            snapshot_and_atomic_replace(&path_copy, &original, &bytes)
        })
        .await
        .map_err(|e| execution(e.to_string()))?
        .map_err(|e| execution(e.to_string()))?;
        Ok(ToolResult::text(format!(
            "Successfully applied hashline edits to {requested}."
        )))
    }
}

fn tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[\s>+\-]*(\d+)\s*#\s*([ZPMQVRWSNKTXJBYH]{2})").unwrap())
}
fn prefix_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[\s>+\-]*\d+\s*#\s*[ZPMQVRWSNKTXJBYH]{2}\s*:").unwrap())
}
fn strip_prefix(line: &str) -> &str {
    prefix_regex()
        .find(line)
        .map_or(line, |found| &line[found.end()..])
}
fn validate_anchor(anchor: &str, lines: &[String], had_bom: bool) -> Result<usize, ToolError> {
    let caps = tag_regex()
        .captures(anchor)
        .ok_or_else(|| invalid(format!("invalid anchor: {anchor}")))?;
    let line: usize = caps[1]
        .parse()
        .map_err(|_| invalid("invalid line number"))?;
    if line == 0 || line > lines.len() {
        return Err(invalid(format!("line {line} out of range")));
    }
    let actual = hashline_tag(line - 1, &lines[line - 1], had_bom);
    if actual != format!("{}#{}", line, &caps[2]) {
        return Err(execution(format!(
            "hash mismatch at line {line}; actual is {actual}. Re-read the file."
        )));
    }
    Ok(line - 1)
}
fn extract_lines(value: Option<&Value>) -> Vec<String> {
    match value {
        None | Some(Value::Null) => vec![],
        Some(Value::String(value)) => value
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .split('\n')
            .map(String::from)
            .collect(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|v| v.as_str().map_or_else(|| v.to_string(), String::from))
            .collect(),
        Some(value) => vec![value.to_string()],
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;
    #[test]
    fn operation_deserializes_known_values_and_rejects_unknown_values() {
        assert_eq!(
            serde_json::from_str::<Operation>("\"replace\"").unwrap(),
            Operation::Replace
        );
        assert_eq!(
            serde_json::from_str::<Operation>("\"prepend\"").unwrap(),
            Operation::Prepend
        );
        assert_eq!(
            serde_json::from_str::<Operation>("\"append\"").unwrap(),
            Operation::Append
        );
        assert!(serde_json::from_str::<Operation>("\"delete\"").is_err());
    }

    #[tokio::test]
    async fn applies_bottom_up_and_rejects_stale() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("a");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let a = hashline_tag(0, "one", false);
        let c = hashline_tag(2, "three", false);
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        HashlineEditTool.execute(ToolContext{cwd:d.path().into(),abort_signal:signal},ToolCallId::new("1"),json!({"path":"a","edits":[{"op":"replace","pos":a,"lines":"ONE"},{"op":"append","pos":c,"lines":["four"]}]}),updates).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "ONE\ntwo\nthree\nfour\n"
        );
    }
}
