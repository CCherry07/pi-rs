#![forbid(unsafe_code)]

use std::{future::Future, path::PathBuf, sync::Arc};

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
use replacement::{
    AppliedEdits, EditInput, Replacement, apply_edits, apply_to_original, normalize_lf_cow,
};

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
            // Match Pi's preflight access check: the edit target must be both readable and
            // writable before any content is read or transformed. Opening without truncation is
            // side-effect free and gives portable ENOENT/EACCES classification.
            let access = tokio::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .await;
            context.signal().check().map_err(|_| ToolError::Aborted)?;
            access.map_err(|error| {
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
            let base = normalize_lf_cow(content);
            let applied = apply_edits(base.as_ref(), &edits, &requested)?;
            let edit_diff = finish_edit(
                path.clone(),
                &requested,
                EditSource {
                    content,
                    base: base.as_ref(),
                    bom,
                },
                applied,
                context.signal(),
                generate_edit_diff,
                tokio::fs::write,
            )
            .await?;
            let mut result = ToolResult::text(format!(
                "Successfully replaced {} block(s) in {requested}.",
                edits.len()
            ));
            result.details = Some(json!({
                "diff": edit_diff.diff,
                "patch": edit_diff.patch,
                "firstChangedLine": edit_diff.first_changed_line
            }));
            Ok(result)
        })
        .await
    }
}

struct EditSource<'a> {
    content: &'a str,
    base: &'a str,
    bom: &'a str,
}

async fn finish_edit<GenerateDiff, Write, WriteFuture>(
    path: PathBuf,
    requested: &str,
    source: EditSource<'_>,
    applied: AppliedEdits,
    signal: &pi_core::AbortSignal,
    generate_diff: GenerateDiff,
    write: Write,
) -> Result<EditDiff, ToolError>
where
    GenerateDiff: FnOnce(&str, &str, &str, &[Replacement], usize) -> Result<EditDiff, ToolError>,
    Write: FnOnce(PathBuf, String) -> WriteFuture,
    WriteFuture: Future<Output = std::io::Result<()>>,
{
    let EditSource { content, base, bom } = source;
    // Plan every fallible detail before constructing or writing the final file. In particular, a
    // diff-window failure can never leave a successful mutation behind.
    let edit_diff = generate_diff(
        requested,
        base,
        &applied.new_content,
        &applied.replacements,
        4,
    )?;
    let changed = if content == base {
        applied.new_content
    } else {
        apply_to_original(content, &applied.replacements)
    };
    let restored = if bom.is_empty() {
        changed
    } else {
        let mut restored = String::with_capacity(bom.len() + changed.len());
        restored.push_str(bom);
        restored.push_str(&changed);
        restored
    };
    signal.check().map_err(|_| ToolError::Aborted)?;
    // Direct writes preserve symlink targets and hard links, like Pi. Moving the String also lets
    // Tokio reuse its allocation instead of copying an entire large file.
    let written = write(path, restored).await;
    signal.check().map_err(|_| ToolError::Aborted)?;
    written.map_err(|error| execution(error.to_string()))?;
    Ok(edit_diff)
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

fn count_line_feeds(text: &str, start: usize, end: usize) -> usize {
    text.as_bytes()[start..end]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
}

fn count_lines(text: &str) -> usize {
    count_line_feeds(text, 0, text.len()) + 1
}

fn generate_diff_with_offsets(
    old: &str,
    new: &str,
    old_start_line: usize,
    new_start_line: usize,
    width: usize,
    context: usize,
) -> (Vec<String>, Option<usize>) {
    let old_lines = count_lines(old);
    let new_lines = count_lines(new);
    let diff = if old_lines.max(new_lines) > 10_000 {
        let algorithm = if old_lines.min(new_lines) <= 256 {
            similar::Algorithm::Lcs
        } else {
            similar::Algorithm::Patience
        };
        similar::TextDiff::configure()
            .algorithm(algorithm)
            .diff_lines(old, new)
    } else {
        similar::TextDiff::from_lines(old, new)
    };
    let changes = diff
        .iter_all_changes()
        .map(|change| {
            (
                change.tag(),
                change.value().trim_end_matches('\n').to_string(),
            )
        })
        .collect::<Vec<_>>();
    let mut old_line = old_start_line;
    let mut new_line = new_start_line;
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
        let marker = |out: &mut Vec<String>| out.push(format!(" {}...", " ".repeat(width)));
        let lines = |range: std::ops::Range<usize>, out: &mut Vec<String>| {
            for index in range {
                let (line, _, text) = &equal[index];
                out.push(format!(" {line:>width$} {text}"));
            }
        };
        if before && after {
            if len <= context * 2 {
                lines(0..len, out);
            } else {
                lines(0..context, out);
                marker(out);
                lines(len - context..len, out);
            }
        } else if before {
            lines(0..len.min(context), out);
            if len > context {
                marker(out);
            }
        } else if after {
            if len > context {
                marker(out);
            }
            lines(len.saturating_sub(context)..len, out);
        }
        equal.clear();
    };
    let mut last_change = false;
    for (tag, text) in &changes {
        if *tag == ChangeTag::Equal {
            equal.push((old_line, new_line, text.clone()));
            old_line += 1;
            new_line += 1;
            continue;
        }
        flush(&mut equal, last_change, true, &mut out);
        first.get_or_insert(new_line);
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
    }
    flush(&mut equal, last_change, false, &mut out);
    (out, first)
}

#[cfg(test)]
fn generate_diff(old: &str, new: &str, context: usize) -> (String, Option<usize>) {
    let width = count_lines(old).max(count_lines(new)).to_string().len();
    let (output, first) = generate_diff_with_offsets(old, new, 1, 1, width, context);
    (output.join("\n"), first)
}

#[cfg(test)]
fn generate_patch(path: &str, old: &str, new: &str, context: usize) -> String {
    let diff = similar::TextDiff::from_lines(old, new);
    diff.unified_diff()
        .context_radius(context)
        .header(path, path)
        .to_string()
}

#[derive(Debug)]
struct EditDiff {
    diff: String,
    patch: String,
    first_changed_line: Option<usize>,
}

#[derive(Debug)]
struct DiffWindow {
    old_start: usize,
    old_end: usize,
    old_line: usize,
    new_start: usize,
    new_end: usize,
    new_line: usize,
}

const DIFF_WINDOW_MARGIN_LINES: usize = 32;

fn line_start_before(text: &str, offset: usize) -> usize {
    if offset == 0 {
        return 0;
    }
    text.as_bytes()[..offset]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1)
}

fn add_delta(value: usize, delta: isize) -> Option<usize> {
    if delta < 0 {
        value.checked_sub(delta.unsigned_abs())
    } else {
        value.checked_add(delta as usize)
    }
}

fn get_diff_windows(
    base: &str,
    replacements: &[Replacement],
) -> Result<Vec<DiffWindow>, ToolError> {
    let mut windows: Vec<DiffWindow> = Vec::new();
    let mut offset_delta = 0_isize;
    let mut line_delta = 0_isize;
    let mut cursor_offset = 0;
    let mut cursor_line = 0;
    let mut previous_end = 0;

    for replacement in replacements {
        let match_end = replacement
            .index
            .checked_add(replacement.length)
            .ok_or_else(|| execution("Replacement range is outside the base content."))?;
        if replacement.length == 0
            || replacement.index < previous_end
            || match_end > base.len()
            || !base.is_char_boundary(replacement.index)
            || !base.is_char_boundary(match_end)
        {
            return Err(execution("Replacement range is outside the base content."));
        }
        previous_end = match_end;

        let mut start = line_start_before(base, replacement.index);
        for _ in 0..DIFF_WINDOW_MARGIN_LINES {
            if start == 0 {
                break;
            }
            start = line_start_before(base, start - 1);
        }
        let mut end = match_end;
        for margin in 0..=DIFF_WINDOW_MARGIN_LINES {
            if end >= base.len() {
                break;
            }
            let search_from = if margin == 0 {
                match_end.saturating_sub(1).max(replacement.index)
            } else {
                end
            };
            end = base.as_bytes()[search_from..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(base.len(), |index| search_from + index + 1);
        }

        let replacement_offset_delta =
            replacement.new_text.len() as isize - replacement.length as isize;
        let replacement_line_delta = replacement
            .new_text
            .as_bytes()
            .iter()
            .filter(|byte| **byte == b'\n')
            .count() as isize
            - count_line_feeds(base, replacement.index, match_end) as isize;

        if let Some(previous) = windows
            .last_mut()
            .filter(|previous| start <= previous.old_end)
        {
            previous.old_end = previous.old_end.max(end);
            previous.new_end = add_delta(previous.old_end, offset_delta + replacement_offset_delta)
                .ok_or_else(|| execution("Replacement range is outside the new content."))?;
        } else {
            cursor_line += count_line_feeds(base, cursor_offset, start);
            cursor_offset = start;
            windows.push(DiffWindow {
                old_start: start,
                old_end: end,
                old_line: cursor_line,
                new_start: add_delta(start, offset_delta)
                    .ok_or_else(|| execution("Replacement range is outside the new content."))?,
                new_end: add_delta(end, offset_delta + replacement_offset_delta)
                    .ok_or_else(|| execution("Replacement range is outside the new content."))?,
                new_line: add_delta(cursor_line, line_delta)
                    .ok_or_else(|| execution("Replacement line is outside the new content."))?,
            });
        }
        offset_delta += replacement_offset_delta;
        line_delta += replacement_line_delta;
    }
    Ok(windows)
}

fn format_patch_range(start: usize, end: usize) -> String {
    let length = end.saturating_sub(start);
    let beginning = if length == 0 { start } else { start + 1 };
    if length == 1 {
        beginning.to_string()
    } else {
        format!("{beginning},{length}")
    }
}

fn generate_edit_diff(
    path: &str,
    base: &str,
    new: &str,
    replacements: &[Replacement],
    context: usize,
) -> Result<EditDiff, ToolError> {
    let windows = get_diff_windows(base, replacements)?;
    if windows.is_empty() {
        return Err(execution("Cannot generate a diff without replacements."));
    }
    let width = count_lines(base).max(count_lines(new)).to_string().len();
    let skip_marker = format!(" {}...", " ".repeat(width));
    let mut output = Vec::new();
    let mut patch = format!("--- {path}\n+++ {path}\n");
    let mut first_changed_line = None;

    for window in windows {
        if window.old_end > base.len()
            || window.new_end > new.len()
            || !new.is_char_boundary(window.new_start)
            || !new.is_char_boundary(window.new_end)
        {
            return Err(execution("Replacement range is outside the diff content."));
        }
        let old_window = &base[window.old_start..window.old_end];
        let new_window = &new[window.new_start..window.new_end];
        let (mut shown, first) = generate_diff_with_offsets(
            old_window,
            new_window,
            window.old_line + 1,
            window.new_line + 1,
            width,
            context,
        );
        first_changed_line = first_changed_line.or(first);
        if output.last() == Some(&skip_marker) && shown.first() == Some(&skip_marker) {
            shown.remove(0);
        }
        output.extend(shown);

        let old_lines = count_lines(old_window);
        let new_lines = count_lines(new_window);
        let local_diff = if old_lines.max(new_lines) > 10_000 {
            let algorithm = if old_lines.min(new_lines) <= 256 {
                similar::Algorithm::Lcs
            } else {
                similar::Algorithm::Patience
            };
            similar::TextDiff::configure()
                .algorithm(algorithm)
                .diff_lines(old_window, new_window)
        } else {
            similar::TextDiff::from_lines(old_window, new_window)
        };
        let mut unified = local_diff.unified_diff();
        unified.context_radius(context);
        for hunk in unified.iter_hunks() {
            let first = hunk
                .ops()
                .first()
                .ok_or_else(|| execution("empty diff hunk"))?;
            let last = hunk
                .ops()
                .last()
                .ok_or_else(|| execution("empty diff hunk"))?;
            let old_start = first.old_range().start + window.old_line;
            let old_end = last.old_range().end + window.old_line;
            let new_start = first.new_range().start + window.new_line;
            let new_end = last.new_range().end + window.new_line;
            let rendered = hunk.to_string();
            let body = rendered
                .split_once('\n')
                .map(|(_, body)| body)
                .ok_or_else(|| execution("invalid diff hunk"))?;
            patch.push_str(&format!(
                "@@ -{} +{} @@\n{body}",
                format_patch_range(old_start, old_end),
                format_patch_range(new_start, new_end)
            ));
        }
    }

    Ok(EditDiff {
        diff: output.join("\n"),
        patch,
        first_changed_line,
    })
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
    async fn diff_planning_failure_does_not_call_the_writer_or_mutate_the_file() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unchanged.txt");
        let before = "before\n";
        std::fs::write(&path, before).unwrap();
        let edits = vec![EditInput {
            old_text: "before".into(),
            new_text: "after".into(),
        }];
        let applied = apply_edits(before, &edits, "unchanged.txt").unwrap();
        let (_, signal) = AbortHandle::new();
        let writer_called = Arc::new(AtomicBool::new(false));
        let writer_observer = Arc::clone(&writer_called);

        let error = finish_edit(
            path.clone(),
            "unchanged.txt",
            EditSource {
                content: before,
                base: before,
                bom: "",
            },
            applied,
            &signal,
            |_, _, _, _, _| Err(execution("injected diff failure")),
            move |_, _| {
                writer_observer.store(true, Ordering::Relaxed);
                async { Ok(()) }
            },
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("injected diff failure"));
        assert!(!writer_called.load(Ordering::Relaxed));
        assert_eq!(std::fs::read_to_string(path).unwrap(), before);
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

    #[test]
    fn inserted_cr_and_kept_lf_remain_two_breaks_at_splice_seam() {
        let written = apply_to_original(
            "a\rb\nc",
            &[Replacement {
                index: 2,
                length: 1,
                new_text: "\r".into(),
            }],
        );

        assert_eq!(
            written.replace("\r\n", "\n").replace('\r', "\n"),
            "a\n\n\nc"
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

    struct Random(u32);

    impl Random {
        fn next(&mut self, max: usize) -> usize {
            self.0 = self.0.wrapping_add(0x6d2b_79f5);
            let mut value = self.0;
            value = (value ^ (value >> 15)).wrapping_mul(1 | value);
            value ^= value.wrapping_add((value ^ (value >> 7)).wrapping_mul(61 | value));
            value ^= value >> 14;
            ((value as u64 * max as u64) >> 32) as usize
        }
    }

    fn random_diff_case(
        random: &mut Random,
        distinct: bool,
    ) -> (String, Vec<replacement::Replacement>, String) {
        let line_count = random.next(400);
        let lines = (0..line_count)
            .map(|index| {
                if distinct {
                    format!("line {index}")
                } else {
                    ["a", "b", "", "}"][random.next(4)].to_string()
                }
            })
            .collect::<Vec<_>>();
        let mut content = lines.join("\n");
        if random.next(2) == 0 {
            content.push('\n');
        }
        let mut replacements = Vec::new();
        let mut position = 0;
        let count = 1 + random.next(4);
        for edit in 0..count {
            if position >= content.len() {
                break;
            }
            let match_index = position + random.next((content.len() - position).min(300));
            if match_index >= content.len() {
                break;
            }
            let match_length = 1 + random.next((content.len() - match_index).min(120));
            let new_lines = (0..random.next(4))
                .map(|line| format!("new {edit} {line}"))
                .collect::<Vec<_>>();
            let new_text = if random.next(3) == 0 {
                String::new()
            } else {
                let mut text = new_lines.join("\n");
                if random.next(2) == 0 {
                    text.push('\n');
                }
                text
            };
            replacements.push(replacement::Replacement {
                index: match_index,
                length: match_length,
                new_text,
            });
            position = match_index + match_length;
        }
        let mut new_content = content.clone();
        for replacement in replacements.iter().rev() {
            new_content.replace_range(
                replacement.index..replacement.index + replacement.length,
                &replacement.new_text,
            );
        }
        (content, replacements, new_content)
    }

    fn split_source_lines(text: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let mut start = 0;
        for (index, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                lines.push(text[start..=index].to_string());
                start = index + 1;
            }
        }
        if start < text.len() {
            lines.push(text[start..].to_string());
        }
        lines
    }

    fn apply_patch_for_test(original: &str, patch: &str) -> String {
        let source = split_source_lines(original);
        let lines = patch.split('\n').collect::<Vec<_>>();
        assert!(lines.first().is_some_and(|line| line.starts_with("--- ")));
        assert!(lines.get(1).is_some_and(|line| line.starts_with("+++ ")));
        let mut output = String::new();
        let mut source_index = 0;
        let mut index = 2;
        while index < lines.len() {
            if lines[index].is_empty() {
                index += 1;
                continue;
            }
            assert!(lines[index].starts_with("@@ "));
            let old = lines[index]
                .split_ascii_whitespace()
                .nth(1)
                .unwrap()
                .strip_prefix('-')
                .unwrap();
            let (start, count) = old
                .split_once(',')
                .map_or((old, "1"), |(start, count)| (start, count));
            let start: usize = start.parse().unwrap();
            let count: usize = count.parse().unwrap();
            let hunk_start = if count == 0 {
                start
            } else {
                start.saturating_sub(1)
            };
            for line in &source[source_index..hunk_start] {
                output.push_str(line);
            }
            source_index = hunk_start;
            index += 1;
            while index < lines.len() && !lines[index].starts_with("@@ ") {
                let line = lines[index];
                if line.is_empty() {
                    break;
                }
                let missing_newline = lines
                    .get(index + 1)
                    .is_some_and(|next| *next == "\\ No newline at end of file");
                let value = if missing_newline {
                    line[1..].to_string()
                } else {
                    format!("{}\n", &line[1..])
                };
                match line.as_bytes()[0] {
                    b' ' => {
                        assert_eq!(source.get(source_index), Some(&value));
                        output.push_str(&value);
                        source_index += 1;
                    }
                    b'-' => {
                        assert_eq!(source.get(source_index), Some(&value));
                        source_index += 1;
                    }
                    b'+' => output.push_str(&value),
                    prefix => panic!("unexpected patch prefix {prefix}"),
                }
                index += if missing_newline { 2 } else { 1 };
            }
        }
        for line in &source[source_index..] {
            output.push_str(line);
        }
        output
    }

    #[test]
    fn windowed_diff_matches_whole_file_for_two_thousand_distinct_line_cases() {
        let mut random = Random(1);
        for run in 0..2_000 {
            let (content, replacements, new_content) = random_diff_case(&mut random, true);
            if content == new_content || replacements.is_empty() {
                continue;
            }
            let windowed = generate_edit_diff("f.txt", &content, &new_content, &replacements, 4)
                .unwrap_or_else(|error| panic!("run {run}: {error}"));
            let whole = generate_diff(&content, &new_content, 4);
            assert_eq!(windowed.diff, whole.0, "display diff mismatch in run {run}");
            assert_eq!(
                windowed.first_changed_line, whole.1,
                "first changed line mismatch in run {run}"
            );
            assert_eq!(
                windowed.patch,
                generate_patch("f.txt", &content, &new_content, 4),
                "patch mismatch in run {run}"
            );
        }
    }

    #[test]
    fn windowed_patch_applies_for_two_thousand_repeated_line_cases() {
        let mut random = Random(2);
        for run in 0..2_000 {
            let (content, replacements, new_content) = random_diff_case(&mut random, false);
            if content == new_content || replacements.is_empty() {
                continue;
            }
            let generated = generate_edit_diff("f.txt", &content, &new_content, &replacements, 4)
                .unwrap_or_else(|error| panic!("run {run}: {error}"));
            assert_eq!(
                apply_patch_for_test(&content, &generated.patch),
                new_content,
                "patch failed in run {run}"
            );
        }
    }

    #[test]
    fn windowed_diff_handles_a_replacement_of_one_hundred_fifty_thousand_lines() {
        let new_text = "x\n".repeat(150_000);
        let replacements = vec![replacement::Replacement {
            index: 0,
            length: 2,
            new_text: new_text.clone(),
        }];
        let new_content = format!("{new_text}b\n");
        let generated =
            generate_edit_diff("f.txt", "a\nb\n", &new_content, &replacements, 4).unwrap();
        assert_eq!(
            apply_patch_for_test("a\nb\n", &generated.patch),
            new_content
        );
    }

    #[test]
    fn small_edit_uses_bounded_windows_in_a_twenty_megabyte_file() {
        let filler = format!("{}\n", "x".repeat(99));
        let half = filler.repeat(100_000);
        let base = format!("{half}TARGET = 1\n{half}");
        let replacement = replacement::Replacement {
            index: half.len(),
            length: "TARGET = 1".len(),
            new_text: "TARGET = 2".into(),
        };
        let new_content = format!("{half}TARGET = 2\n{half}");
        let windows = get_diff_windows(&base, std::slice::from_ref(&replacement)).unwrap();
        let diffed_bytes = windows
            .iter()
            .map(|window| (window.old_end - window.old_start) + (window.new_end - window.new_start))
            .sum::<usize>();
        assert!(diffed_bytes < 16 * 1024, "windowed {diffed_bytes} bytes");

        let generated = generate_edit_diff(
            "big.txt",
            &base,
            &new_content,
            std::slice::from_ref(&replacement),
            4,
        )
        .unwrap();
        let changes = generated
            .diff
            .lines()
            .filter(|line| line.starts_with(['+', '-']))
            .collect::<Vec<_>>();
        assert_eq!(changes, ["-100001 TARGET = 1", "+100001 TARGET = 2"]);
        assert!(generated.diff.len() < 4 * 1024);
        assert!(generated.patch.len() < 4 * 1024);
    }
}
