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
    context.signal().check().map_err(|_| ToolError::Aborted)?;
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
    context.signal().check().map_err(|_| ToolError::Aborted)?;
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

    async fn execute_grep(
        cwd: &Path,
        input: Value,
        signal: pi_core::AbortSignal,
    ) -> Result<ToolResult, ToolError> {
        let (updates, _) = ToolUpdateSink::channel();
        GrepTool
            .execute(
                ToolContext::standalone(cwd.to_path_buf(), signal),
                ToolCallId::new("test"),
                input,
                updates,
            )
            .await
    }

    async fn run_grep(cwd: &Path, input: Value) -> ToolResult {
        let (_, signal) = AbortHandle::new();
        execute_grep(cwd, input, signal).await.unwrap()
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
    async fn public_grep_truncation_drops_or_keeps_complete_surrogate_pairs_at_utf16_boundary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("surrogates.txt"),
            format!("{}😀tail\n{}😀tail\n", "a".repeat(499), "a".repeat(498)),
        )
        .unwrap();

        let result = run_grep(
            dir.path(),
            json!({"pattern":"tail", "path":"surrogates.txt"}),
        )
        .await;

        assert_eq!(
            result_text(&result),
            format!(
                "surrogates.txt:1: {}... [truncated]\nsurrogates.txt:2: {}😀... [truncated]\n\n[Some lines truncated to 500 chars. Use read tool to see full lines]",
                "a".repeat(499),
                "a".repeat(498)
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

    #[tokio::test]
    async fn respects_global_match_limit_and_includes_context_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("context.txt"),
            "before\nmatch one\nafter\nmiddle\nmatch two\nafter two",
        )
        .unwrap();

        let result = run_grep(
            dir.path(),
            json!({"pattern":"match", "path":"context.txt", "limit":1, "context":1}),
        )
        .await;

        assert_eq!(
            result_text(&result),
            "context.txt-1- before\ncontext.txt:2: match one\ncontext.txt-3- after\n\n[1 matches limit reached. Use limit=2 for more, or refine pattern]"
        );
        assert_eq!(result.details.as_ref().unwrap()["matchLimitReached"], 1);
    }

    #[tokio::test]
    async fn keeps_lone_cr_context_line_numbers_and_strips_only_terminators() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lone-cr.txt"),
            "before\rprogress\nneedle\nafter",
        )
        .unwrap();
        let result = run_grep(
            dir.path(),
            json!({"pattern":"needle", "path":"lone-cr.txt", "context":1}),
        )
        .await;

        assert_eq!(
            result_text(&result),
            "lone-cr.txt-1- before\rprogress\nlone-cr.txt:2: needle\nlone-cr.txt-3- after"
        );
    }

    #[tokio::test]
    async fn prints_overlapping_context_once_and_keeps_last_match_trailing_context() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rg-context.txt"),
            "a\nmatch one\nb\nmatch two\nmatch three\nc\nmatch four",
        )
        .unwrap();
        let result = run_grep(
            dir.path(),
            json!({"pattern":"match", "path":"rg-context.txt", "context":1, "limit":2}),
        )
        .await;

        assert_eq!(
            result_text(&result).split("\n\n").next().unwrap(),
            "rg-context.txt-1- a\nrg-context.txt:2: match one\nrg-context.txt-3- b\nrg-context.txt:4: match two\nrg-context.txt-5- match three"
        );
        assert_eq!(result.details.as_ref().unwrap()["matchLimitReached"], 2);
    }

    #[tokio::test]
    async fn counts_allowed_matches_past_byte_cap_for_limit_and_continuation_details() {
        let dir = tempfile::tempdir().unwrap();
        let line = format!("match {}", "x".repeat(400));
        std::fs::write(
            dir.path().join("byte-cap.txt"),
            std::iter::repeat_n(line, 200)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let result = run_grep(
            dir.path(),
            json!({"pattern":"match", "path":"byte-cap.txt", "limit":150}),
        )
        .await;
        let text = result_text(&result);
        let details = result.details.as_ref().unwrap();

        assert!(
            text.contains("150 matches limit reached. Use limit=300 for more, or refine pattern")
        );
        assert!(text.contains("50.0KB limit reached"));
        assert_eq!(details["matchLimitReached"], 150);
        assert_eq!(details["truncation"]["totalLines"], 150);
        assert!(details["truncation"]["totalBytes"].as_u64().unwrap() > 50 * 1024);
    }

    #[tokio::test]
    async fn reports_truncation_when_stored_output_ends_exactly_at_byte_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines = vec!["x".repeat(490); 103];
        lines.push("x".repeat(7));
        lines.push("extra x".to_string());
        std::fs::write(dir.path().join("x"), lines.join("\n")).unwrap();

        let result = run_grep(dir.path(), json!({"pattern":"x", "path":"x", "limit":1000})).await;

        assert!(result_text(&result).ends_with("\n\n[50.0KB limit reached]"));
        assert_eq!(
            result.details.as_ref().unwrap()["truncation"]["totalLines"],
            105
        );
    }

    #[tokio::test]
    async fn accepts_huge_context_without_exponent_or_allocation_failures() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("huge-context.txt"), "before\nneedle\nafter").unwrap();

        let result = run_grep(
            dir.path(),
            json!({"pattern":"needle", "path":"huge-context.txt", "context":1e30}),
        )
        .await;

        assert_eq!(
            result_text(&result),
            "huge-context.txt-1- before\nhuge-context.txt:2: needle\nhuge-context.txt-3- after"
        );
    }

    #[tokio::test]
    async fn decodes_utf16_context_and_lossy_utf8_search_rows() {
        let dir = tempfile::tempdir().unwrap();
        let utf16 = "\u{feff}before\nneedle 中文\nafter\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        std::fs::write(dir.path().join("utf16.txt"), utf16).unwrap();
        std::fs::write(
            dir.path().join("invalid-utf8.txt"),
            b"before\n\xffneedle\nafter\n",
        )
        .unwrap();

        let utf16 = run_grep(
            dir.path(),
            json!({"pattern":"needle", "path":"utf16.txt", "context":1}),
        )
        .await;
        assert_eq!(
            result_text(&utf16),
            "utf16.txt-1- before\nutf16.txt:2: needle 中文\nutf16.txt-3- after"
        );

        let utf8 = run_grep(
            dir.path(),
            json!({"pattern":"needle", "path":"invalid-utf8.txt", "context":1}),
        )
        .await;
        assert_eq!(
            result_text(&utf8),
            "invalid-utf8.txt-1- before\ninvalid-utf8.txt:2: �needle\ninvalid-utf8.txt-3- after"
        );
    }

    #[tokio::test]
    async fn exact_match_limit_has_no_notice_and_fractional_limit_is_floored() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("exact-limit.txt"),
            "match one\nx\nmatch two\ny",
        )
        .unwrap();

        for context in [0, 1] {
            let exact = run_grep(
                dir.path(),
                json!({"pattern":"match", "path":"exact-limit.txt", "limit":2, "context":context}),
            )
            .await;
            assert!(!result_text(&exact).contains("limit reached"));
            assert!(exact.details.is_none());

            let over = run_grep(
                dir.path(),
                json!({"pattern":"match", "path":"exact-limit.txt", "limit":1, "context":context}),
            )
            .await;
            assert!(result_text(&over).contains("1 matches limit reached. Use limit=2 for more"));
            assert_eq!(over.details.as_ref().unwrap()["matchLimitReached"], 1);
        }

        let fractional = run_grep(
            dir.path(),
            json!({"pattern":"match", "path":"exact-limit.txt", "limit":1.5}),
        )
        .await;
        assert_eq!(fractional.details.as_ref().unwrap()["matchLimitReached"], 1);
    }

    #[tokio::test]
    async fn ignores_ripgrep_configuration_and_external_binary_state() {
        static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _guard = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("rg-config");
        std::fs::write(&config, "--fixed-strings\n").unwrap();
        std::fs::write(dir.path().join("fixture.txt"), "aXb\na.b\n").unwrap();
        let previous = std::env::var_os("RIPGREP_CONFIG_PATH");
        // SAFETY: this package's tests serialize mutations of this variable, and the
        // in-process implementation never launches another thread that reads it.
        unsafe { std::env::set_var("RIPGREP_CONFIG_PATH", &config) };

        let result = run_grep(
            dir.path(),
            json!({"pattern":"a.b", "path":"fixture.txt", "literal":false}),
        )
        .await;

        match previous {
            Some(value) => unsafe { std::env::set_var("RIPGREP_CONFIG_PATH", value) },
            None => unsafe { std::env::remove_var("RIPGREP_CONFIG_PATH") },
        }
        assert_eq!(
            result_text(&result),
            "fixture.txt:1: aXb\nfixture.txt:2: a.b"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn roots_folder_globs_at_real_and_symlinked_search_paths() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("migrations/main")).unwrap();
        std::fs::create_dir_all(repo.join("nested/migrations/main")).unwrap();
        std::fs::write(repo.join("migrations/main/a.js"), "hello\n").unwrap();
        std::fs::write(repo.join("nested/migrations/main/b.js"), "hello\n").unwrap();
        let link = dir.path().join("repo-link");
        symlink(&repo, &link).unwrap();

        for path in [&repo, &link] {
            let anchored = run_grep(
                dir.path(),
                json!({"pattern":"hello", "path":path, "glob":"./migrations/main/*.js"}),
            )
            .await;
            assert_eq!(result_text(&anchored), "migrations/main/a.js:1: hello");

            let recursive = run_grep(
                dir.path(),
                json!({"pattern":"hello", "path":path, "glob":"**/migrations/main/*.js"}),
            )
            .await;
            let mut lines = result_text(&recursive).lines().collect::<Vec<_>>();
            lines.sort_unstable();
            assert_eq!(
                lines,
                [
                    "migrations/main/a.js:1: hello",
                    "nested/migrations/main/b.js:1: hello",
                ]
            );

            let negated = run_grep(
                dir.path(),
                json!({"pattern":"hello", "path":path, "glob":"!./migrations/**"}),
            )
            .await;
            assert_eq!(
                result_text(&negated),
                "nested/migrations/main/b.js:1: hello"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn treats_flag_like_pattern_as_text_without_executing_it() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grep-injection-marker");
        let payload = dir.path().join("payload.sh");
        std::fs::write(
            &payload,
            format!(
                "#!/bin/sh\necho executed > {}\ncat \"$1\"\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.path().join("target.txt"), "target\n").unwrap();

        let result = run_grep(
            dir.path(),
            json!({"pattern":format!("--pre={}", payload.display()), "path":dir.path()}),
        )
        .await;

        assert_eq!(result_text(&result), "No matches found");
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn uses_tool_context_cwd_for_relative_search_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ctx-cwd-grep.txt"), "match in ctx.cwd").unwrap();

        let result = run_grep(dir.path(), json!({"pattern":"match"})).await;

        assert_eq!(result_text(&result), "ctx-cwd-grep.txt:1: match in ctx.cwd");
    }

    #[tokio::test]
    async fn pre_aborted_search_does_no_filesystem_work() {
        let dir = tempfile::tempdir().unwrap();
        let (abort, signal) = AbortHandle::new();
        abort.abort();

        let result = execute_grep(
            dir.path(),
            json!({"pattern":"needle", "path":"missing"}),
            signal,
        )
        .await;

        assert!(matches!(result, Err(ToolError::Aborted)));
    }

    #[tokio::test]
    async fn rejects_invalid_regexes_and_missing_or_non_file_paths() {
        let dir = tempfile::tempdir().unwrap();
        let (_, signal) = AbortHandle::new();
        let invalid = execute_grep(dir.path(), json!({"pattern":"["}), signal)
            .await
            .unwrap_err();
        assert!(matches!(invalid, ToolError::InvalidArguments(_)));

        let (_, signal) = AbortHandle::new();
        let missing = execute_grep(
            dir.path(),
            json!({"pattern":"needle", "path":"missing"}),
            signal,
        )
        .await
        .unwrap_err();
        assert!(missing.to_string().contains("Path not found:"));
    }
}
