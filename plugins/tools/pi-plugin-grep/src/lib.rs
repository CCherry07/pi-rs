use async_trait::async_trait;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::{WalkBuilder, overrides::OverrideBuilder};
use pi_core::{PluginId, ToolCallId, ToolResult, ToolSpec};
use pi_plugin::{Plugin, RegisterContext, Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_tool_support::with_prompt;
use pi_tool_support::{
    execution, hashline_tag, invalid, optional_number, optional_result_limit, require_str,
    resolve_to_cwd, spec, truncate_lines_by_bytes,
};
use serde_json::{Map, Value, json};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::Arc;

const DEFAULT_LIMIT: usize = 100;
const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const MAX_LINE_CHARS: usize = 500;

pub struct GrepPlugin;
pub struct GrepTool;

pub struct ConfiguredGrepPlugin {
    hashline: bool,
}

impl ConfiguredGrepPlugin {
    pub fn new(hashline: bool) -> Self {
        Self { hashline }
    }
}

struct ConfiguredGrepTool {
    hashline: bool,
}

#[pi_plugin::plugin]
impl Plugin for ConfiguredGrepPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("grep-tool")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(ConfiguredGrepTool {
            hashline: self.hashline,
        }))
    }
}
#[pi_plugin::plugin]
impl Plugin for GrepPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("grep-tool")
    }
    fn register(&self, c: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        c.register_tool(Arc::new(GrepTool))
    }
}
#[async_trait]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        with_prompt(
            spec(
                "grep",
                "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars.",
                json!({
                    "type":"object","properties":{
                        "pattern":{"type":"string","description":"Search pattern (regex or literal string)"},"path":{"type":"string","description":"Directory or file to search (default: current directory)"},"glob":{"type":"string","description":"Filter files by glob pattern relative to path, e.g. 'src/**/*.ts'. Without '/', e.g. '*.ts', it matches file names at any depth"},
                        "ignoreCase":{"type":"boolean","description":"Case-insensitive search (default: false)"},"literal":{"type":"boolean","description":"Treat pattern as literal string instead of regex (default: false)"},
                        "context":{"type":"number","description":"Number of lines to show before and after each match (default: 0)"},"limit":{"type":"number","description":"Maximum number of matches to return (default: 100)"}
                    },"required":["pattern"]
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
        execute_grep(false, context, input).await
    }
}

#[async_trait]
impl Tool for ConfiguredGrepTool {
    fn spec(&self) -> ToolSpec {
        let mut spec = GrepTool.spec();
        if self.hashline {
            spec.parameters["properties"]["hashline"] = json!({"type":"boolean"});
        }
        spec
    }

    async fn execute(
        &self,
        context: ToolContext,
        _id: ToolCallId,
        input: Value,
        _updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        execute_grep(self.hashline, context, input).await
    }
}

async fn execute_grep(
    enable_hashline: bool,
    context: ToolContext,
    input: Value,
) -> Result<ToolResult, ToolError> {
    let pattern = require_str(&input, "pattern")?.to_string();
    let path = resolve_to_cwd(
        context.cwd(),
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
    let context_lines = optional_number(&input, "context")?
        .unwrap_or(0.0)
        .floor()
        .clamp(0.0, 9_007_199_254_740_991.0) as usize;
    let limit = optional_result_limit(&input, "limit", DEFAULT_LIMIT)?;
    let hashline = enable_hashline
        && input
            .get("hashline")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let matcher = build_matcher(&pattern, literal, ignore_case)?;
    let metadata = std::fs::metadata(&path)
        .map_err(|_| execution(format!("Path not found: {}", path.display())))?;
    let search_is_directory = metadata.is_dir();
    if !search_is_directory && !metadata.is_file() {
        return Err(execution(format!(
            "Path is not a file or directory: {}",
            path.display()
        )));
    }
    let signal = context.signal().clone();
    let result = tokio::task::spawn_blocking(move || {
        search_files(
            &path,
            glob.as_deref(),
            &matcher,
            SearchOptions {
                limit,
                context_lines,
                hashline,
            },
            &signal,
        )
    })
    .await
    .map_err(|e| execution(e.to_string()))??;

    if result.match_count == 0 {
        return Ok(ToolResult::text("No matches found"));
    }

    let (mut text, truncation) = truncate_lines_by_bytes(&result.lines, MAX_OUTPUT_BYTES);
    let mut notices = Vec::new();
    let mut details = Map::new();
    if result.match_limit_reached {
        notices.push(format!(
            "{limit} matches limit reached. Use limit={} for more, or refine pattern",
            limit.saturating_mul(2)
        ));
        details.insert("matchLimitReached".into(), json!(limit));
    }
    if let Some(mut truncation) = truncation {
        truncation["totalLines"] = json!(result.output_line_count);
        truncation["totalBytes"] = json!(result.output_bytes.saturating_sub(1));
        notices.push("50.0KB limit reached".to_string());
        details.insert("truncation".into(), truncation);
    }
    if result.lines_truncated {
        notices
            .push("Some lines truncated to 500 chars. Use read tool to see full lines".to_string());
        details.insert("linesTruncated".into(), Value::Bool(true));
    }
    if !notices.is_empty() {
        text.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }
    let mut tool_result = ToolResult::text(text);
    tool_result.details = (!details.is_empty()).then_some(Value::Object(details));
    Ok(tool_result)
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

#[derive(Clone, Copy)]
struct SearchOptions {
    limit: usize,
    context_lines: usize,
    hashline: bool,
}

#[derive(Debug, Default)]
struct SearchOutcome {
    lines: Vec<String>,
    match_count: usize,
    match_limit_reached: bool,
    lines_truncated: bool,
    output_bytes: usize,
    output_line_count: usize,
}

fn search_files(
    path: &Path,
    glob: Option<&str>,
    matcher: &RegexMatcher,
    options: SearchOptions,
    signal: &pi_core::AbortSignal,
) -> Result<SearchOutcome, ToolError> {
    let mut outcome = SearchOutcome::default();
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .before_context(options.context_lines)
        .after_context(options.context_lines)
        .binary_detection(BinaryDetection::quit(b'\0'))
        .build();
    let metadata = std::fs::metadata(path).map_err(|error| execution(error.to_string()))?;
    let override_root = if metadata.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let overrides = glob
        .filter(|pattern| !pattern.is_empty())
        .map(|pattern| {
            let mut builder = OverrideBuilder::new(override_root);
            let (negation, pattern) = pattern
                .strip_prefix('!')
                .map_or(("", pattern), |p| ("!", p));
            let pattern = format!("{negation}{}", pattern.trim_start_matches("./"));
            builder
                .add(&pattern)
                .map_err(|error| invalid(format!("invalid glob: {error}")))?;
            builder
                .build()
                .map_err(|error| invalid(format!("invalid glob: {error}")))
        })
        .transpose()?;

    let mut search = |file: &Path| -> Result<bool, ToolError> {
        let mut sink = SearchSink {
            display_path: format_path(file, path, metadata.is_dir()),
            options,
            signal,
            outcome: &mut outcome,
            trailing_context_end: None,
            had_bom: false,
        };
        // Inspect the BOM on the same opened stream, only for the optional anchor extension.
        // The searcher's decoder strips it, while read's anchor helper includes it in line one.
        let result = if options.hashline {
            let file = std::fs::File::open(file).map_err(|error| execution(error.to_string()))?;
            let mut reader = BufReader::new(file);
            sink.had_bom = reader
                .fill_buf()
                .map_err(|error| execution(error.to_string()))?
                .starts_with(b"\xef\xbb\xbf");
            searcher.search_reader(matcher, reader, &mut sink)
        } else {
            searcher.search_path(matcher, file, &mut sink)
        };
        result.map_err(|error| execution(format!("{}: {error}", file.display())))?;
        if signal.is_aborted() {
            return Err(ToolError::Aborted);
        }
        Ok(sink.outcome.match_limit_reached)
    };
    if metadata.is_file() {
        // Like rg, explicitly named files are searched even when a glob would exclude them.
        search(path)?;
    } else {
        let mut builder = WalkBuilder::new(path);
        builder
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true);
        if let Some(overrides) = overrides {
            builder.overrides(overrides);
        }
        for entry in builder.build() {
            if signal.is_aborted() {
                return Err(ToolError::Aborted);
            }
            let entry = entry.map_err(|error| execution(error.to_string()))?;
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            if search(entry.path())? {
                break;
            }
        }
    }
    Ok(outcome)
}

struct SearchSink<'a> {
    display_path: String,
    options: SearchOptions,
    signal: &'a pi_core::AbortSignal,
    outcome: &'a mut SearchOutcome,
    trailing_context_end: Option<u64>,
    had_bom: bool,
}

impl SearchSink<'_> {
    fn record(&mut self, line_number: u64, bytes: &[u8], is_match: bool) -> bool {
        if self.signal.is_aborted() {
            return false;
        }
        let below_limit = self.outcome.match_count < self.options.limit;
        let in_window = self
            .trailing_context_end
            .is_some_and(|end| line_number <= end);
        if below_limit || in_window {
            let text = String::from_utf8_lossy(bytes);
            let line = sanitize_line(&text);
            let (body, truncated) =
                format_line(line, line_number, self.options.hashline, self.had_bom);
            self.outcome.lines_truncated |= truncated;
            let separator = if below_limit && is_match { ':' } else { '-' };
            let rendered = format!(
                "{}{separator}{line_number}{separator} {body}",
                self.display_path
            );
            // Keep one line past the byte cap for the shared truncator, then count only.
            if self.outcome.output_bytes.saturating_sub(1) <= MAX_OUTPUT_BYTES {
                self.outcome.lines.push(rendered.clone());
            }
            self.outcome.output_bytes += rendered.len() + 1;
            self.outcome.output_line_count += 1;
        }
        if below_limit {
            if is_match {
                self.outcome.match_count += 1;
                if self.outcome.match_count == self.options.limit {
                    self.trailing_context_end =
                        Some(line_number.saturating_add(self.options.context_lines as u64));
                }
            }
            true
        } else {
            self.outcome.match_limit_reached |= is_match;
            // Another match is proof of truncation, but finish the last allowed trailing window.
            !self.outcome.match_limit_reached
                || self
                    .trailing_context_end
                    .is_some_and(|end| line_number < end)
        }
    }
}

impl Sink for SearchSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        Ok(self.record(
            mat.line_number().expect("line numbers enabled"),
            mat.bytes(),
            true,
        ))
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        context: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        Ok(self.record(
            context.line_number().expect("line numbers enabled"),
            context.bytes(),
            false,
        ))
    }
}

fn format_path(path: &Path, search_root: &Path, search_is_directory: bool) -> String {
    let display = if search_is_directory {
        path.strip_prefix(search_root).unwrap_or(path)
    } else {
        path.file_name().map(Path::new).unwrap_or(path)
    };
    display.to_string_lossy().replace('\\', "/")
}

fn sanitize_line(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

fn format_line(line: &str, line_number: u64, hashline: bool, had_bom: bool) -> (String, bool) {
    let tag = hashline.then(|| {
        let index = usize::try_from(line_number.saturating_sub(1)).unwrap_or(usize::MAX);
        hashline_tag(index, line, had_bom)
    });
    let (line, truncated) = truncate_line(line);
    if let Some(tag) = tag {
        (format!("{tag}:{line}"), truncated)
    } else {
        (line, truncated)
    }
}

fn truncate_line(line: &str) -> (String, bool) {
    let mut units = 0;
    for (offset, ch) in line.char_indices() {
        units += ch.len_utf16();
        if units > MAX_LINE_CHARS {
            return (format!("{}... [truncated]", &line[..offset]), true);
        }
    }
    (line.to_string(), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;

    #[tokio::test]
    async fn hashline_output_requires_configured_extension() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.txt"), "needle").unwrap();
        assert!(
            GrepTool.spec().parameters["properties"]
                .get("hashline")
                .is_none()
        );
        let tool = ConfiguredGrepTool { hashline: true };
        assert!(
            tool.spec().parameters["properties"]
                .get("hashline")
                .is_some()
        );
        let plain = run_grep(
            directory.path(),
            json!({"pattern":"needle","hashline":true}),
        )
        .await;
        assert_eq!(result_text(&plain), "a.txt:1: needle");
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let tagged = tool
            .execute(
                ToolContext::standalone(directory.path().into(), signal),
                ToolCallId::new("grep"),
                json!({"pattern":"needle","hashline":true}),
                updates,
            )
            .await
            .unwrap();
        assert_eq!(
            result_text(&tagged),
            format!(
                "a.txt:1: {}:needle",
                pi_tool_support::hashline_tag(0, "needle", false)
            )
        );
    }

    async fn run_grep(cwd: &Path, input: Value) -> ToolResult {
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        GrepTool
            .execute(
                ToolContext::standalone(cwd.to_path_buf(), signal),
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
                ToolContext::standalone(dir.path().to_path_buf(), signal),
                ToolCallId::new("1"),
                json!({"pattern":"hello"}),
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
    async fn glob_overrides_gitignore_and_explicit_files_override_globs_like_rg() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "needle").unwrap();
        let globbed = run_grep(dir.path(), json!({"pattern":"needle", "glob":"*.txt"})).await;
        assert_eq!(result_text(&globbed), "ignored.txt:1: needle");
        let explicit = run_grep(
            dir.path(),
            json!({"pattern":"needle", "path":"ignored.txt", "glob":"*.rs"}),
        )
        .await;
        assert_eq!(result_text(&explicit), "ignored.txt:1: needle");
    }

    #[tokio::test]
    async fn configured_hashline_uses_read_tags_with_bom_and_original_long_lines() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!("needle {}", "x".repeat(600));
        std::fs::write(
            dir.path().join("a.txt"),
            format!("\u{feff}needle\r\n{long}\n"),
        )
        .unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ConfiguredGrepTool { hashline: true }
            .execute(
                ToolContext::standalone(dir.path().into(), signal),
                ToolCallId::new("grep"),
                json!({"pattern":"needle", "hashline":true}),
                updates,
            )
            .await
            .unwrap();
        let text = result_text(&result);
        assert!(text.starts_with(&format!(
            "a.txt:1: {}:needle\n",
            hashline_tag(0, "needle", true)
        )));
        assert!(text.contains(&format!("a.txt:2: {}:", hashline_tag(1, &long, true))));
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
        assert!(!text.contains("matches limit reached"));
        assert!(result.details.is_none());
    }

    #[tokio::test]
    async fn merges_overlapping_context_and_keeps_match_markers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.txt"),
            "before\nneedle one\nneedle two\nafter\n",
        )
        .unwrap();
        let result = run_grep(
            dir.path(),
            json!({"pattern":"needle", "context":1, "limit":2}),
        )
        .await;
        assert_eq!(
            result_text(&result),
            "a.txt-1- before\na.txt:2: needle one\na.txt:3: needle two\na.txt-4- after"
        );
        assert!(result.details.is_none());
    }

    #[tokio::test]
    async fn extra_matches_in_the_last_window_are_context_and_prove_truncation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.txt"),
            "before\nneedle one\nneedle two\nafter\noutside\nneedle three\n",
        )
        .unwrap();
        let result = run_grep(
            dir.path(),
            json!({"pattern":"needle", "context":2, "limit":1}),
        )
        .await;
        assert_eq!(
            result_text(&result),
            "a.txt-1- before\na.txt:2: needle one\na.txt-3- needle two\na.txt-4- after\n\n[1 matches limit reached. Use limit=2 for more, or refine pattern]"
        );
        assert_eq!(result.details.as_ref().unwrap()["matchLimitReached"], 1);
    }

    #[tokio::test]
    async fn preserves_embedded_cr_and_truncates_at_utf16_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let even = "😀".repeat(251);
        let odd = format!("a{}", "😀".repeat(250));
        std::fs::write(
            dir.path().join("a.txt"),
            format!("left\rright\r\n{even}\n{odd}\nend\r"),
        )
        .unwrap();
        let result = run_grep(dir.path(), json!({"pattern":"."})).await;
        assert_eq!(
            result_text(&result),
            format!(
                "a.txt:1: left\rright\na.txt:2: {}... [truncated]\na.txt:3: a{}... [truncated]\na.txt:4: end\n\n[Some lines truncated to 500 chars. Use read tool to see full lines]",
                "😀".repeat(250),
                "😀".repeat(249)
            )
        );
        assert_eq!(result.details.as_ref().unwrap()["linesTruncated"], true);
    }

    #[tokio::test]
    async fn globs_are_root_relative_and_strip_dot_slash_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["root/src/a.ts", "root/pkg/src/b.ts", "elsewhere/src/c.ts"] {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "needle").unwrap();
        }
        for glob in ["src/*.ts", "./src/*.ts", "././src/*.ts"] {
            let result = run_grep(
                dir.path(),
                json!({"pattern":"needle", "path":"root", "glob":glob}),
            )
            .await;
            assert_eq!(result_text(&result), "src/a.ts:1: needle", "{glob}");
        }
        let recursive = run_grep(
            dir.path(),
            json!({"pattern":"needle", "path":"root", "glob":"**/src/*.ts"}),
        )
        .await;
        assert!(result_text(&recursive).contains("src/a.ts:1: needle"));
        assert!(result_text(&recursive).contains("pkg/src/b.ts:1: needle"));
        let excluded = run_grep(
            dir.path(),
            json!({"pattern":"needle", "path":"root", "glob":"!./src/*.ts"}),
        )
        .await;
        assert_eq!(result_text(&excluded), "pkg/src/b.ts:1: needle");
    }

    #[tokio::test]
    async fn floors_numeric_limits_and_context_like_pi() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("a.txt"),
            "before\nneedle\nafter\nneedle\nend",
        )
        .unwrap();
        for limit in [-1.0, 0.0, 1.9] {
            let result = run_grep(
                directory.path(),
                json!({"pattern":"needle", "limit":limit, "context":1.9}),
            )
            .await;
            let text = result_text(&result);
            assert!(text.contains("before"));
            assert!(text.contains("after"));
            assert!(!text.contains("end"));
            assert_eq!(result.details.as_ref().unwrap()["matchLimitReached"], 1);
        }
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
