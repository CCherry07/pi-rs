#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use async_trait::async_trait;
use pi_core::{
    AgentPlugin, PluginId, RegisterContext, Tool, ToolCallId, ToolContext, ToolError, ToolResult,
    ToolSpec, ToolUpdateSink,
};
use pi_tool_support::with_prompt;
use pi_tool_support::{
    MAX_WRITE_BYTES, execution, invalid, require_str, resolve_to_cwd, snapshot_and_atomic_replace,
    spec,
};
use serde::Deserialize;
use serde_json::{Value, json};
use similar::ChangeTag;
use tokio::sync::Mutex as AsyncMutex;
use unicode_normalization::UnicodeNormalization as _;

pub struct EditPlugin;
pub struct EditTool;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditInput {
    old_text: String,
    new_text: String,
}

struct Replacement {
    index: usize,
    length: usize,
    new_text: String,
}

#[pi_core::agent_plugin]
impl AgentPlugin for EditPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("edit-tool")
    }
    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        context.register_tool(Arc::new(EditTool))
    }
}

#[async_trait]
impl Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        with_prompt(
            spec(
                "edit",
                "Edit one file with one or more unique, non-overlapping text replacements matched against the original file.",
                json!({
                    "type":"object","properties":{
                    "path":{"type":"string","description":"Path to the file to edit (relative or absolute)"},
                        "edits":{"type":"array","items":{"type":"object","properties":{"oldText":{"type":"string"},"newText":{"type":"string"}},"required":["oldText","newText"],"additionalProperties":false}}
                    },"required":["path"],"additionalProperties":false
                }),
            ),
            "Make precise file edits with exact text replacement, including multiple disjoint edits in one call",
            [
                "Use edit for precise changes (edits[].oldText must match exactly)",
                "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls",
                "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.",
                "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.",
            ],
        )
    }

    async fn prepare_arguments(
        &self,
        _context: &ToolContext,
        mut input: Value,
    ) -> Result<Value, ToolError> {
        let object = input
            .as_object_mut()
            .ok_or_else(|| invalid("arguments must be an object"))?;
        if let Some(Value::String(raw)) = object.get("edits").cloned()
            && let Ok(parsed) = serde_json::from_str::<Value>(&raw)
        {
            object.insert(
                "edits".to_string(),
                if parsed.is_object() {
                    Value::Array(vec![parsed])
                } else {
                    parsed
                },
            );
        }
        if object.get("edits").is_some_and(Value::is_object) {
            let edit = object.remove("edits").unwrap();
            object.insert("edits".to_string(), Value::Array(vec![edit]));
        }
        let has_explicit_edits = object.contains_key("edits");
        let legacy = (object.remove("oldText"), object.remove("newText"));
        if !has_explicit_edits && let (Some(old), Some(new)) = legacy {
            object
                .entry("edits".to_string())
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()
                .ok_or_else(|| invalid("edits must be an array"))?
                .push(json!({"oldText":old,"newText":new}));
        }
        Ok(input)
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let requested = require_str(&input, "path")?;
        let edits: Vec<EditInput> = if let Some(edits) = input.get("edits") {
            let edits = if let Some(raw) = edits.as_str() {
                serde_json::from_str(raw).map_err(|e| invalid(e.to_string()))?
            } else if edits.is_object() {
                Value::Array(vec![edits.clone()])
            } else {
                edits.clone()
            };
            serde_json::from_value(edits).map_err(|e| invalid(e.to_string()))?
        } else {
            vec![EditInput {
                old_text: require_str(&input, "oldText")?.to_string(),
                new_text: require_str(&input, "newText")?.to_string(),
            }]
        };
        if edits.is_empty() {
            return Err(invalid("edits must contain at least one replacement"));
        }
        if edits.iter().any(|edit| edit.old_text.is_empty()) {
            return Err(invalid("oldText must not be empty"));
        }
        if edits
            .iter()
            .any(|edit| edit.new_text.len() > MAX_WRITE_BYTES)
        {
            return Err(invalid("newText is too large"));
        }
        let path = resolve_to_cwd(context.cwd(), requested)?;
        let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let queue = file_queue(&key);
        let _guard = tokio::select! {biased;()=context.signal().wait()=>return Err(ToolError::Aborted),guard=queue.lock()=>guard};
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let metadata = tokio::fs::symlink_metadata(&path)
            .await
            .map_err(|e| execution(format!("Could not edit file: {requested}. {e}.")))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(execution("target must be a regular non-symlink file"));
        }
        if metadata.len() > MAX_WRITE_BYTES as u64 {
            return Err(execution(format!("file exceeds {MAX_WRITE_BYTES} bytes")));
        }
        let original = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| execution(format!("Could not edit file: {requested}. {e}.")))?;
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let (bom, content) = original
            .strip_prefix('\u{feff}')
            .map_or(("", original.as_str()), |text| ("\u{feff}", text));
        let ending = if content
            .find("\r\n")
            .is_some_and(|crlf| content.find('\n').is_some_and(|lf| crlf <= lf))
        {
            "\r\n"
        } else {
            "\n"
        };
        let base = normalize_lf(content);
        let (diff_base, updated) = apply_edits(&base, &edits, requested)?;
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let restored = format!(
            "{bom}{}",
            if ending == "\r\n" {
                updated.replace('\n', "\r\n")
            } else {
                updated.clone()
            }
        );
        if restored.len() > MAX_WRITE_BYTES {
            return Err(execution(format!(
                "edited file exceeds {MAX_WRITE_BYTES} bytes"
            )));
        }
        let (diff, first_changed_line) = generate_diff(&diff_base, &updated, 4);
        let patch = generate_patch(requested, &diff_base, &updated, 4);
        let expected = original.into_bytes();
        let bytes = restored.into_bytes();
        let path_copy = path.clone();
        tokio::task::spawn_blocking(move || {
            snapshot_and_atomic_replace(&path_copy, &expected, &bytes)
        })
        .await
        .map_err(|e| execution(e.to_string()))?
        .map_err(|e| execution(e.to_string()))?;
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let mut result = ToolResult::text(format!(
            "Successfully replaced {} block(s) in {requested}.",
            edits.len()
        ));
        result.details =
            Some(json!({"diff":diff,"patch":patch,"firstChangedLine":first_changed_line}));
        Ok(result)
    }
}

fn file_queue(path: &std::path::Path) -> Arc<AsyncMutex<()>> {
    static QUEUES: OnceLock<Mutex<HashMap<std::path::PathBuf, Weak<AsyncMutex<()>>>>> =
        OnceLock::new();
    let mut queues = QUEUES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(queue) = queues.get(path).and_then(Weak::upgrade) {
        return queue;
    }
    let queue = Arc::new(AsyncMutex::new(()));
    queues.insert(path.to_path_buf(), Arc::downgrade(&queue));
    queue
}
fn normalize_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}
fn fuzzy(text: &str) -> String {
    text.nfkc()
        .collect::<String>()
        .split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .replace(['\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'], "'")
        .replace(['\u{201c}', '\u{201d}', '\u{201e}', '\u{201f}'], "\"")
        .replace(
            [
                '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}',
            ],
            "-",
        )
}
fn occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        0
    } else {
        haystack.match_indices(needle).count()
    }
}
fn apply_edits(
    content: &str,
    edits: &[EditInput],
    path: &str,
) -> Result<(String, String), ToolError> {
    let normalized = edits
        .iter()
        .map(|e| EditInput {
            old_text: normalize_lf(&e.old_text),
            new_text: normalize_lf(&e.new_text),
        })
        .collect::<Vec<_>>();
    let use_fuzzy = normalized.iter().any(|edit| {
        !content.contains(&edit.old_text) && fuzzy(content).contains(&fuzzy(&edit.old_text))
    });
    let base = if use_fuzzy {
        fuzzy(content)
    } else {
        content.to_string()
    };
    let mut replacements = Vec::new();
    for (i, edit) in normalized.iter().enumerate() {
        let needle = if use_fuzzy {
            fuzzy(&edit.old_text)
        } else {
            edit.old_text.clone()
        };
        let count = occurrences(&base, &needle);
        if count == 0 {
            return Err(execution(if edits.len() == 1 {
                format!(
                    "Could not find the exact text in {path}. The old text must match exactly including all whitespace and newlines."
                )
            } else {
                format!("Could not find edits[{i}] in {path}.")
            }));
        }
        if count > 1 {
            return Err(execution(format!(
                "Found {count} occurrences of edits[{i}] in {path}. Each oldText must be unique."
            )));
        }
        replacements.push((
            i,
            Replacement {
                index: base.find(&needle).unwrap(),
                length: needle.len(),
                new_text: edit.new_text.clone(),
            },
        ));
    }
    replacements.sort_by_key(|(_, r)| r.index);
    for pair in replacements.windows(2) {
        if pair[0].1.index + pair[0].1.length > pair[1].1.index {
            return Err(execution(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                pair[0].0, pair[1].0
            )));
        }
    }
    let mut output = base.clone();
    for (_, r) in replacements.into_iter().rev() {
        output.replace_range(r.index..r.index + r.length, &r.new_text);
    }
    if output == content {
        return Err(execution(format!(
            "No changes made to {path}. The replacements produced identical content."
        )));
    }
    Ok((content.to_string(), output))
}

fn generate_diff(old: &str, new: &str, context: usize) -> (String, Option<usize>) {
    let diff = similar::TextDiff::from_lines(old, new);
    let changes = diff
        .iter_all_changes()
        .map(|change| {
            (
                change.tag(),
                change.value().trim_end_matches('\n').to_string(),
            )
        })
        .collect::<Vec<_>>();
    let width = old
        .lines()
        .count()
        .max(new.lines().count())
        .max(1)
        .to_string()
        .len();
    let mut old_line = 1;
    let mut new_line = 1;
    let mut first = None;
    let mut out = Vec::new();
    let mut equal = Vec::new();
    let flush = |equal: &mut Vec<(usize, usize, String)>,
                 before: bool,
                 after: bool,
                 out: &mut Vec<String>| {
        if equal.is_empty() {
            return;
        }
        let len = equal.len();
        let indices: Vec<usize> = if before && after && len > context * 2 {
            (0..context).chain(len - context..len).collect()
        } else if before {
            (0..len.min(context)).collect()
        } else if after {
            (len.saturating_sub(context)..len).collect()
        } else {
            Vec::new()
        };
        let mut previous = None;
        for index in indices {
            if previous.is_some_and(|p| index > p + 1) {
                out.push(format!(" {} ...", "".repeat(width)));
            }
            let (line, _, text) = &equal[index];
            out.push(format!(" {line:>width$} {text}"));
            previous = Some(index);
        }
        equal.clear();
    };
    let mut last_change = false;
    for (index, (tag, text)) in changes.iter().enumerate() {
        if *tag == ChangeTag::Equal {
            equal.push((old_line, new_line, text.clone()));
            old_line += 1;
            new_line += 1;
            continue;
        }
        flush(&mut equal, last_change, true, &mut out);
        if first.is_none() {
            first = Some(new_line);
        }
        match tag {
            ChangeTag::Delete => {
                out.push(format!("-{old_line:>width$} {text}"));
                old_line += 1;
            }
            ChangeTag::Insert => {
                out.push(format!("+{new_line:>width$} {text}"));
                new_line += 1;
            }
            ChangeTag::Equal => {}
        }
        last_change = true;
        if changes
            .get(index + 1)
            .is_some_and(|(next, _)| *next == ChangeTag::Equal)
        {
            let future_change = changes[index + 1..]
                .iter()
                .any(|(future, _)| *future != ChangeTag::Equal);
            if !future_change {
                flush(&mut equal, true, false, &mut out);
            }
        }
    }
    flush(&mut equal, last_change, false, &mut out);
    (out.join("\n"), first)
}
fn generate_patch(path: &str, old: &str, new: &str, context: usize) -> String {
    let diff = similar::TextDiff::from_lines(old, new);
    diff.unified_diff()
        .context_radius(context)
        .header(path, path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;
    async fn run(path: &std::path::Path, input: Value) -> Result<ToolResult, ToolError> {
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        EditTool
            .execute(
                ToolContext::standalone(path.parent().unwrap().into(), signal),
                ToolCallId::new("1"),
                input,
                updates,
            )
            .await
    }

    #[tokio::test]
    async fn explicit_edits_take_precedence_over_legacy_fields() {
        let (_, signal) = AbortHandle::new();
        let prepared = EditTool
            .prepare_arguments(
                &ToolContext::standalone(std::path::PathBuf::from("."), signal),
                json!({
                    "path": "a",
                    "edits": [{"oldText": "one", "newText": "ONE"}],
                    "oldText": "",
                    "newText": ""
                }),
            )
            .await
            .unwrap();

        assert_eq!(
            prepared,
            json!({
                "path": "a",
                "edits": [{"oldText": "one", "newText": "ONE"}]
            })
        );
    }

    #[tokio::test]
    async fn legacy_fields_are_converted_when_edits_are_absent() {
        let (_, signal) = AbortHandle::new();
        let prepared = EditTool
            .prepare_arguments(
                &ToolContext::standalone(std::path::PathBuf::from("."), signal),
                json!({"path": "a", "oldText": "one", "newText": "ONE"}),
            )
            .await
            .unwrap();

        assert_eq!(
            prepared,
            json!({
                "path": "a",
                "edits": [{"oldText": "one", "newText": "ONE"}]
            })
        );
    }

    #[test]
    fn schema_exposes_only_the_current_edits_shape() {
        let parameters = EditTool.spec().parameters;
        assert!(parameters.pointer("/properties/edits").is_some());
        assert!(parameters.pointer("/properties/oldText").is_none());
        assert!(parameters.pointer("/properties/newText").is_none());
    }

    #[tokio::test]
    async fn supports_multiple_and_legacy_input() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a");
        std::fs::write(&p, "one\ntwo\nthree\n").unwrap();
        let result=run(&p,json!({"path":"a","edits":[{"oldText":"one","newText":"ONE"},{"oldText":"three","newText":"THREE"}]})).await.unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "ONE\ntwo\nTHREE\n");
        assert!(
            result.details.unwrap()["patch"]
                .as_str()
                .unwrap()
                .contains("--- a")
        );
    }
    #[tokio::test]
    async fn fuzzy_fallback_preserves_bom_and_crlf() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a");
        std::fs::write(&p, "\u{feff}hello\u{00a0}‘world’  \r\nnext\r\n").unwrap();
        run(
            &p,
            json!({"path":"a","oldText":"hello 'world'\nnext","newText":"changed\nline"}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(p).unwrap(),
            "\u{feff}changed\r\nline\r\n"
        );
    }
    #[tokio::test]
    async fn rejects_overlap() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a");
        std::fs::write(&p, "abcdef").unwrap();
        let error=run(&p,json!({"path":"a","edits":[{"oldText":"abcd","newText":"x"},{"oldText":"cdef","newText":"y"}]})).await.unwrap_err();
        assert!(error.to_string().contains("overlap"));
    }
}
