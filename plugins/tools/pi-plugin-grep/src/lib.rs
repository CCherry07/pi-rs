use async_trait::async_trait;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, sinks::Lossy};
use ignore::{WalkBuilder, overrides::OverrideBuilder};
use pi_core::{
    AgentPlugin, PluginId, RegisterContext, Tool, ToolCallId, ToolContext, ToolError, ToolResult,
    ToolSpec, ToolUpdateSink,
};
use pi_tool_support::with_prompt;
use pi_tool_support::{
    execution, invalid, optional_positive_usize, require_str, resolve_to_cwd, spec,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const DEFAULT_LIMIT: usize = 100;
const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const MAX_LINE_CHARS: usize = 500;

pub struct GrepPlugin;
pub struct GrepTool;
#[pi_core::agent_plugin]
impl AgentPlugin for GrepPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("grep-tool")
    }
    fn register(&self, c: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        c.register_tool(Arc::new(GrepTool))
    }
}
#[async_trait]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        with_prompt(
            spec(
                "grep",
                "Search text files for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB. Long lines are truncated to 500 chars.",
                json!({
                    "type":"object","properties":{
                        "pattern":{"type":"string"},"path":{"type":"string","description":"Directory or file to search (default: current directory)"},"glob":{"type":"string"},
                        "ignoreCase":{"type":"boolean"},"literal":{"type":"boolean"},
                        "context":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1},
                        "hashline":{"type":"boolean"}
                    },"required":["pattern"],"additionalProperties":false
                }),
            ),
            "Search file contents for patterns (respects .gitignore)",
            std::iter::empty::<&str>(),
        )
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        let pattern = require_str(&input, "pattern")?.to_string();
        let path = resolve_to_cwd(
            &context.cwd,
            input.get("path").and_then(Value::as_str).unwrap_or("."),
        )?;
        let glob = input.get("glob").and_then(Value::as_str).map(str::to_owned);
        let ignore_case = input
            .get("ignoreCase")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let literal = input
            .get("literal")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let context_lines = input
            .get("context")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0);
        let limit = optional_positive_usize(&input, "limit", DEFAULT_LIMIT)?;
        let hashline = input
            .get("hashline")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let matcher = build_matcher(&pattern, literal, ignore_case)?;
        let search_root = path.clone();
        let metadata = std::fs::metadata(&path)
            .map_err(|_| execution(format!("Path not found: {}", path.display())))?;
        let search_is_directory = metadata.is_dir();
        if !search_is_directory && !metadata.is_file() {
            return Err(execution(format!(
                "Path is not a file or directory: {}",
                path.display()
            )));
        }
        let signal = context.abort_signal.clone();
        let result = tokio::task::spawn_blocking(move || {
            search_files(&path, glob.as_deref(), &matcher, limit, &signal)
        })
        .await
        .map_err(|e| execution(e.to_string()))??;

        if result.matches.is_empty() {
            return Ok(ToolResult::text("No matches found"));
        }

        let (output_lines, lines_truncated) = format_matches(
            &result.matches,
            &search_root,
            search_is_directory,
            context_lines,
            hashline,
        )?;
        let (mut text, truncation) = truncate_output(&output_lines);
        let mut notices = Vec::new();
        let mut details = Map::new();
        if result.match_limit_reached {
            notices.push(format!(
                "{limit} matches limit reached. Use limit={} for more, or refine pattern",
                limit.saturating_mul(2)
            ));
            details.insert("matchLimitReached".into(), json!(limit));
        }
        if let Some(truncation) = truncation {
            notices.push("50.0KB limit reached".to_string());
            details.insert("truncation".into(), truncation);
        }
        if lines_truncated {
            notices.push(
                "Some lines truncated to 500 chars. Use read tool to see full lines".to_string(),
            );
            details.insert("linesTruncated".into(), Value::Bool(true));
        }
        if !notices.is_empty() {
            text.push_str(&format!("\n\n[{}]", notices.join(". ")));
        }
        let mut tool_result = ToolResult::text(text);
        tool_result.details = (!details.is_empty()).then_some(Value::Object(details));
        Ok(tool_result)
    }
}

fn build_matcher(
    pattern: &str,
    literal: bool,
    ignore_case: bool,
) -> Result<RegexMatcher, ToolError> {
    let mut builder = RegexMatcherBuilder::new();
    builder
        .case_insensitive(ignore_case)
        .fixed_strings(literal)
        .line_terminator(Some(b'\n'))
        .ban_byte(Some(b'\0'));
    builder
        .build(pattern)
        .map_err(|error| invalid(format!("invalid pattern: {error}")))
}

#[derive(Debug)]
struct MatchRecord {
    path: PathBuf,
    line_number: u64,
    line: String,
}

#[derive(Debug)]
struct SearchOutcome {
    matches: Vec<MatchRecord>,
    match_limit_reached: bool,
}

fn search_files(
    path: &Path,
    glob: Option<&str>,
    matcher: &RegexMatcher,
    limit: usize,
    signal: &pi_core::AbortSignal,
) -> Result<SearchOutcome, ToolError> {
    let mut records = Vec::new();
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(b'\0'))
        .build();
    let metadata = std::fs::metadata(path).map_err(|error| execution(error.to_string()))?;
    let override_root = if metadata.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let overrides = glob
        .map(|pattern| {
            let mut builder = OverrideBuilder::new(override_root);
            builder
                .add(pattern)
                .map_err(|error| invalid(format!("invalid glob: {error}")))?;
            builder
                .build()
                .map_err(|error| invalid(format!("invalid glob: {error}")))
        })
        .transpose()?;

    if metadata.is_file() {
        if overrides
            .as_ref()
            .is_none_or(|matcher| !matcher.matched(path, false).is_ignore())
        {
            search_file(&mut searcher, matcher, path, &mut records, limit, signal)?;
        }
    } else {
        let mut builder = WalkBuilder::new(path);
        builder
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true);
        for entry in builder.build() {
            if signal.is_aborted() {
                return Err(ToolError::Aborted);
            }
            let entry = entry.map_err(|error| execution(error.to_string()))?;
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            if overrides
                .as_ref()
                .is_some_and(|matcher| matcher.matched(entry.path(), false).is_ignore())
            {
                continue;
            }
            search_file(
                &mut searcher,
                matcher,
                entry.path(),
                &mut records,
                limit,
                signal,
            )?;
            if records.len() >= limit {
                break;
            }
        }
    }
    Ok(SearchOutcome {
        match_limit_reached: records.len() >= limit,
        matches: records,
    })
}

fn search_file(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    path: &Path,
    records: &mut Vec<MatchRecord>,
    limit: usize,
    signal: &pi_core::AbortSignal,
) -> Result<(), ToolError> {
    let record_path = path.to_path_buf();
    searcher
        .search_path(
            matcher,
            path,
            Lossy(|line_number, line| {
                if signal.is_aborted() {
                    return Ok(false);
                }
                records.push(MatchRecord {
                    path: record_path.clone(),
                    line_number,
                    line: sanitize_line(line),
                });
                Ok(records.len() < limit)
            }),
        )
        .map_err(|error| execution(format!("{}: {error}", path.display())))?;
    if signal.is_aborted() {
        Err(ToolError::Aborted)
    } else {
        Ok(())
    }
}

fn format_matches(
    records: &[MatchRecord],
    search_root: &Path,
    search_is_directory: bool,
    context_lines: usize,
    hashline: bool,
) -> Result<(Vec<String>, bool), ToolError> {
    let mut output = Vec::new();
    let mut lines_truncated = false;
    let mut batch_start = 0;
    while batch_start < records.len() {
        let path = &records[batch_start].path;
        let mut batch_end = batch_start + 1;
        while batch_end < records.len() && records[batch_end].path == *path {
            batch_end += 1;
        }
        let batch = &records[batch_start..batch_end];
        let display_path = format_path(path, search_root, search_is_directory);
        let context = if context_lines == 0 {
            None
        } else {
            Some(
                load_context_lines(path, batch, context_lines).map_err(|error| {
                    execution(format!(
                        "cannot read {} for context: {error}",
                        path.display()
                    ))
                })?,
            )
        };

        for record in batch {
            if let Some(context) = &context {
                let start = record
                    .line_number
                    .saturating_sub(u64::try_from(context_lines).unwrap_or(u64::MAX))
                    .max(1);
                let end = record
                    .line_number
                    .saturating_add(u64::try_from(context_lines).unwrap_or(u64::MAX));
                for (&line_number, line) in context.range(start..=end) {
                    let (body, was_truncated) = format_line(line, line_number, hashline);
                    lines_truncated |= was_truncated;
                    if line_number == record.line_number {
                        output.push(format!("{display_path}:{line_number}: {body}"));
                    } else {
                        output.push(format!("{display_path}-{line_number}- {body}"));
                    }
                }
            } else {
                let (body, was_truncated) = format_line(&record.line, record.line_number, hashline);
                lines_truncated |= was_truncated;
                output.push(format!("{display_path}:{}: {body}", record.line_number));
            }
        }
        batch_start = batch_end;
    }
    Ok((output, lines_truncated))
}

fn load_context_lines(
    path: &Path,
    records: &[MatchRecord],
    context_lines: usize,
) -> std::io::Result<BTreeMap<u64, String>> {
    let context = u64::try_from(context_lines).unwrap_or(u64::MAX);
    let mut ranges = records
        .iter()
        .map(|record| {
            (
                record.line_number.saturating_sub(context).max(1),
                record.line_number.saturating_add(context),
            )
        })
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ranges {
        if let Some((_, previous_end)) = merged.last_mut()
            && start <= previous_end.saturating_add(1)
        {
            *previous_end = (*previous_end).max(end);
        } else {
            merged.push((start, end));
        }
    }

    let mut selected = BTreeMap::new();
    let mut reader = BufReader::new(std::fs::File::open(path)?);
    let mut buffer = Vec::new();
    let mut line_number = 0u64;
    let mut range_index = 0usize;
    loop {
        buffer.clear();
        if reader.read_until(b'\n', &mut buffer)? == 0 {
            break;
        }
        line_number += 1;
        while range_index < merged.len() && line_number > merged[range_index].1 {
            range_index += 1;
        }
        if range_index >= merged.len() {
            break;
        }
        let (start, end) = merged[range_index];
        if line_number >= start && line_number <= end {
            selected.insert(
                line_number,
                sanitize_line(&String::from_utf8_lossy(&buffer)),
            );
        }
    }
    Ok(selected)
}

fn format_path(path: &Path, search_root: &Path, search_is_directory: bool) -> String {
    let display = if search_is_directory {
        path.strip_prefix(search_root).unwrap_or(path)
    } else {
        path.file_name().map(Path::new).unwrap_or(path)
    };
    display.to_string_lossy().replace('\\', "/")
}

fn sanitize_line(line: &str) -> String {
    line.trim_end_matches('\n').replace('\r', "")
}

fn format_line(line: &str, line_number: u64, hashline: bool) -> (String, bool) {
    let hash_value = hashline.then(|| {
        let index = usize::try_from(line_number.saturating_sub(1)).unwrap_or(usize::MAX);
        hash(index, line)
    });
    let (line, truncated) = truncate_line(line);
    if hashline {
        (
            format!("{line_number}#{}:{line}", hash_value.unwrap()),
            truncated,
        )
    } else {
        (line, truncated)
    }
}

fn truncate_line(line: &str) -> (String, bool) {
    if line.chars().count() <= MAX_LINE_CHARS {
        return (line.to_string(), false);
    }
    let truncated = line.chars().take(MAX_LINE_CHARS).collect::<String>();
    (format!("{truncated}... [truncated]"), true)
}

fn truncate_output(lines: &[String]) -> (String, Option<Value>) {
    let total_bytes = lines.iter().map(String::len).sum::<usize>() + lines.len().saturating_sub(1);
    if total_bytes <= MAX_OUTPUT_BYTES {
        return (lines.join("\n"), None);
    }

    let mut output = String::new();
    let mut output_lines = 0usize;
    for line in lines {
        let separator = usize::from(!output.is_empty());
        if output
            .len()
            .saturating_add(separator)
            .saturating_add(line.len())
            > MAX_OUTPUT_BYTES
        {
            break;
        }
        if separator == 1 {
            output.push('\n');
        }
        output.push_str(line);
        output_lines += 1;
    }
    let output_bytes = output.len();
    let details = json!({
        "content": output.clone(),
        "truncated": true,
        "truncatedBy": "bytes",
        "totalLines": lines.len(),
        "totalBytes": total_bytes,
        "outputLines": output_lines,
        "outputBytes": output_bytes,
        "lastLinePartial": false,
        "firstLineExceedsLimit": false,
        "maxLines": 9_007_199_254_740_991u64,
        "maxBytes": MAX_OUTPUT_BYTES
    });
    (output, Some(details))
}

fn hash(index: usize, line: &str) -> String {
    let mut value = index as u32;
    for byte in line.bytes() {
        value = value.wrapping_mul(16777619) ^ u32::from(byte);
    }
    let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    format!(
        "{}{}",
        chars[((value / 26) % 26) as usize] as char,
        chars[(value % 26) as usize] as char
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;

    async fn run_grep(cwd: &Path, input: Value) -> ToolResult {
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        GrepTool
            .execute(
                ToolContext {
                    cwd: cwd.to_path_buf(),
                    abort_signal: signal,
                },
                ToolCallId::new("test"),
                input,
                updates,
            )
            .await
            .unwrap()
    }

    fn result_text(result: &ToolResult) -> &str {
        match &result.content[0] {
            pi_core::ContentBlock::Text(value) => &value.text,
            _ => panic!("expected text result"),
        }
    }

    #[tokio::test]
    async fn searches_text_and_skips_ignored_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("visible.txt"), "hello world\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "hello hidden\n").unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = GrepTool
            .execute(
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
                ToolCallId::new("1"),
                json!({"pattern":"hello","glob":"*.txt"}),
                updates,
            )
            .await
            .unwrap();
        let text = match &result.content[0] {
            pi_core::ContentBlock::Text(v) => &v.text,
            _ => panic!(),
        };
        assert!(text.contains("visible.txt"));
        assert!(!text.contains("ignored.txt"));
    }

    #[tokio::test]
    async fn limit_counts_matches_instead_of_context_rows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("matches.txt"),
            "before one\nneedle one\nafter one\ngap\nbefore two\nneedle two\nafter two\n",
        )
        .unwrap();

        let result = run_grep(
            dir.path(),
            json!({"pattern":"needle", "context":1, "limit":2}),
        )
        .await;
        let text = result_text(&result);

        assert!(text.contains("matches.txt:2: needle one"));
        assert!(text.contains("matches.txt:6: needle two"));
        assert!(text.contains("2 matches limit reached"));
        assert_eq!(result.details.as_ref().unwrap()["matchLimitReached"], 2);
    }

    #[tokio::test]
    async fn truncates_long_lines_like_pi() {
        let dir = tempfile::tempdir().unwrap();
        let long_line = format!("needle{}", "x".repeat(600));
        std::fs::write(dir.path().join("long.txt"), long_line).unwrap();

        let result = run_grep(dir.path(), json!({"pattern":"needle"})).await;
        let text = result_text(&result);

        assert!(text.contains("... [truncated]"));
        assert!(text.contains("Some lines truncated to 500 chars"));
        assert_eq!(result.details.as_ref().unwrap()["linesTruncated"], true);
    }

    #[tokio::test]
    async fn streams_files_larger_than_ten_megabytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        let mut contents = vec![b'x'; 10 * 1024 * 1024 + 1];
        contents.extend_from_slice(b"\nneedle at end\n");
        std::fs::write(path, contents).unwrap();

        let result = run_grep(dir.path(), json!({"pattern":"needle"})).await;

        assert!(result_text(&result).contains("large.txt:2: needle at end"));
    }

    #[tokio::test]
    async fn single_file_search_includes_basename_and_accepts_flag_like_literals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("single.txt");
        std::fs::write(&path, "value -n stays search text\n").unwrap();

        let result = run_grep(
            dir.path(),
            json!({"pattern":"-n", "path":path, "literal":true}),
        )
        .await;

        assert_eq!(
            result_text(&result),
            "single.txt:1: value -n stays search text"
        );
    }

    #[tokio::test]
    async fn truncates_total_output_at_fifty_kibibytes() {
        let dir = tempfile::tempdir().unwrap();
        let lines = (0..1000)
            .map(|index| format!("needle {index} {}", "x".repeat(100)))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("many.txt"), lines).unwrap();

        let result = run_grep(dir.path(), json!({"pattern":"needle", "limit":1000})).await;
        let text = result_text(&result);

        assert!(text.len() < 52 * 1024);
        assert!(text.contains("50.0KB limit reached"));
        assert_eq!(
            result.details.as_ref().unwrap()["truncation"]["truncated"],
            true
        );
    }
}
