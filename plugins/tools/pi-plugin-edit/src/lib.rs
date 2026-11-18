#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{PluginId, ToolCallId, ToolResult, ToolSpec};
use pi_plugin::{Plugin, RegisterContext, Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_tool_support::with_prompt;
use pi_tool_support::{
    execution, invalid, require_str, resolve_to_cwd, spec, with_file_mutation_queue,
};
use serde_json::{Value, json};
use similar::ChangeTag;
mod replacement;
use replacement::{EditInput, apply_edits, apply_to_original, normalize_lf};

pub struct EditPlugin;
pub struct EditTool;

#[pi_plugin::plugin]
impl Plugin for EditPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("edit-tool")
    }
    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(EditTool))
    }
}

#[async_trait]
impl Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        let mut tool = with_prompt(
            spec(
                "edit",
                "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.",
                json!({
                    "type":"object","properties":{
                    "path":{"type":"string","description":"Path to the file to edit (relative or absolute)"},
                        "edits":{"type":"array","description":"One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.","items":{"type":"object","properties":{
                            "oldText":{"type":"string","description":"Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."},
                            "newText":{"type":"string","description":"Replacement text for this targeted edit."}
                        },"required":["oldText","newText"]}}
                    },"required":["path","edits"]
                }),
            ),
            "Make precise file edits with exact text replacement, including multiple disjoint edits in one call",
            [
                "Use edit for targeted changes. Batch changes to one file in a single call.",
                "Each edits[].oldText must exactly and uniquely match the original file. Keep matches short and non-overlapping; merge nearby changes without including large unchanged regions.",
            ],
        );
        tool.constrained_sampling = Some(pi_core::ConstrainedSampling::JsonSchema {
            strict: pi_core::ToolSchemaStrict::Prefer,
        });
        tool
    }

    async fn prepare_arguments(
        &self,
        _context: &ToolContext,
        mut input: Value,
    ) -> Result<Value, ToolError> {
        let Some(object) = input.as_object_mut() else {
            return Ok(input);
        };
        let single = |value: &Value| {
            value.get("oldText").is_some_and(Value::is_string)
                && value.get("newText").is_some_and(Value::is_string)
        };
        if let Some(Value::String(raw)) = object.get("edits") {
            if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                if parsed.is_array() {
                    object.insert("edits".into(), parsed);
                } else if single(&parsed) {
                    object.insert("edits".into(), Value::Array(vec![parsed]));
                }
            }
        } else if object.get("edits").is_some_and(single) {
            let edit = object.remove("edits").unwrap();
            object.insert("edits".into(), Value::Array(vec![edit]));
        }
        if object.get("oldText").is_some_and(Value::is_string)
            && object.get("newText").is_some_and(Value::is_string)
        {
            let old = object.remove("oldText").unwrap();
            let new = object.remove("newText").unwrap();
            let mut edits = match object.remove("edits") {
                Some(Value::Array(edits)) => edits,
                _ => Vec::new(),
            };
            edits.push(json!({"oldText":old,"newText":new}));
            object.insert("edits".into(), Value::Array(edits));
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
        let input = self.prepare_arguments(&context, input).await?;
        let requested = require_str(&input, "path")?.to_string();
        let edits: Vec<EditInput> = input
            .get("edits")
            .filter(|v| v.is_array() && !v.as_array().unwrap().is_empty())
            .ok_or_else(|| {
                execution(
                    "Edit tool input is invalid. edits must contain at least one replacement.",
                )
            })
            .and_then(|edits| {
                serde_json::from_value(edits.clone()).map_err(|error| invalid(error.to_string()))
            })?;
        let path = resolve_to_cwd(context.cwd(), &requested)?;
        with_file_mutation_queue(path.clone(), async move {
            context.signal().check().map_err(|_| ToolError::Aborted)?;
            let metadata = tokio::fs::metadata(&path).await;
            context.signal().check().map_err(|_| ToolError::Aborted)?;
            metadata.map_err(|error| {
                execution(format!(
                    "Could not edit file: {requested}. {}.",
                    file_access_error(&error)
                ))
            })?;
            let bytes = tokio::fs::read(&path).await;
            context.signal().check().map_err(|_| ToolError::Aborted)?;
            let bytes = bytes.map_err(|error| execution(error.to_string()))?;
            let original = String::from_utf8_lossy(&bytes);
            let (bom, content) = original
                .strip_prefix('\u{feff}')
                .map_or(("", original.as_ref()), |text| ("\u{feff}", text));
            let base = normalize_lf(content);
            let applied = apply_edits(&base, &edits, &requested)?;
            let restored = format!("{bom}{}", apply_to_original(content, &applied.replacements));
            let (diff, first_changed_line) = generate_diff(&base, &applied.new_content, 4);
            let patch = generate_patch(&requested, &base, &applied.new_content, 4);
            context.signal().check().map_err(|_| ToolError::Aborted)?;
            // Direct writes preserve symlink targets and hard links, like Pi.
            let written = tokio::fs::write(&path, restored.as_bytes()).await;
            context.signal().check().map_err(|_| ToolError::Aborted)?;
            written.map_err(|error| execution(error.to_string()))?;
            let mut result = ToolResult::text(format!(
                "Successfully replaced {} block(s) in {requested}.",
                edits.len()
            ));
            result.details =
                Some(json!({"diff":diff,"patch":patch,"firstChangedLine":first_changed_line}));
            Ok(result)
        })
        .await
    }
}

fn file_access_error(error: &std::io::Error) -> String {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT",
        std::io::ErrorKind::NotADirectory => "ENOTDIR",
        std::io::ErrorKind::PermissionDenied => "EACCES",
        _ => return error.to_string(),
    };
    format!("Error code: {code}")
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
    async fn legacy_fields_are_appended_to_explicit_edits_like_pi() {
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
                "edits": [{"oldText": "one", "newText": "ONE"}, {"oldText":"", "newText":""}]
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
        assert_eq!(parameters["required"], json!(["path", "edits"]));
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
    async fn fuzzy_edit_preserves_untouched_lines_and_mixed_line_endings() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a");
        std::fs::write(
            &p,
            "\u{feff}keep ‘smart’  \r\nold\u{00a0}value  \nkeep—dash\r\n",
        )
        .unwrap();
        run(
            &p,
            json!({"path":"a","edits":[{"oldText":"old value","newText":"new\nvalue"}]}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "\u{feff}keep ‘smart’  \r\nnew\nvalue\nkeep—dash\r\n"
        );
    }

    #[tokio::test]
    async fn exact_match_still_rejects_fuzzy_equivalent_duplicates() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a");
        let before = "say 'hello'\nsay ‘hello’";
        std::fs::write(&path, before).unwrap();
        let error = run(
            &path,
            json!({"path":"a", "edits":[{"oldText":"say 'hello'", "newText":"changed"}]}),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("Found 2 occurrences"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edit_follows_symlinks_preserves_hardlinks_and_decodes_utf8_lossily() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("target");
        let alias = root.path().join("a");
        let hardlink = root.path().join("hardlink");
        std::fs::write(&path, b"before\n\xff\n").unwrap();
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        std::fs::hard_link(&path, &hardlink).unwrap();
        run(
            &alias,
            json!({"path":"a", "edits":[{"oldText":"before", "newText":"after"}]}),
        )
        .await
        .unwrap();
        assert!(
            std::fs::symlink_metadata(&alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(hardlink).unwrap(),
            "after\n\u{fffd}\n"
        );
    }

    #[tokio::test]
    async fn edit_preserves_classic_mac_endings_and_cr_lf_seams() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a");
        std::fs::write(&path, "keep\rchange\rtail\r").unwrap();
        run(
            &path,
            json!({"path":"a", "edits":[{"oldText":"change", "newText":"changed\nsecond"}]}),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "keep\rchanged\rsecond\rtail\r"
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
