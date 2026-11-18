#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use pi_core::{
    ContentBlock, ModelImageResizeOptions, ModelInput, PluginId, ToolCallId, ToolResult, ToolSpec,
};
use pi_media::image::{ImagePolicy, detect_mime_type, process_image};
use pi_plugin::{Plugin, RegisterContext, Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_tool_support::{
    execution, hashline_tag, optional_number, require_str, resolve_read_path, spec, with_prompt,
};
use serde_json::{Value, json};

pub struct ReadPlugin;
pub struct ReadTool;

pub struct ConfiguredReadPlugin {
    resize_options: Option<ModelImageResizeOptions>,
    auto_resize_images: bool,
    hashline: bool,
}

impl ConfiguredReadPlugin {
    pub fn new(auto_resize_images: bool) -> Self {
        Self {
            resize_options: None,
            auto_resize_images,
            hashline: false,
        }
    }

    /// Fallback used only when the current model has no image resize profile.
    pub fn with_resize_options(mut self, options: ModelImageResizeOptions) -> Self {
        self.resize_options = Some(options);
        self
    }

    pub fn with_hashline(mut self, enabled: bool) -> Self {
        self.hashline = enabled;
        self
    }
}

struct ConfiguredReadTool {
    resize_options: Option<ModelImageResizeOptions>,
    auto_resize_images: bool,
    hashline: bool,
}

const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const MAX_OUTPUT_LINES: usize = 2_000;

#[pi_plugin::plugin]
impl Plugin for ReadPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("read")
    }
    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(ReadTool))
    }
}

#[pi_plugin::plugin]
impl Plugin for ConfiguredReadPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("read")
    }

    fn register(&self, context: &mut RegisterContext<'_>) -> pi_plugin::Result<()> {
        context.register_tool(Arc::new(ConfiguredReadTool {
            resize_options: self.resize_options,
            auto_resize_images: self.auto_resize_images,
            hashline: self.hashline,
        }))
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        let mut tool = with_prompt(
            spec(
                "read",
                "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
                json!({
                    "type":"object","properties":{
                        "path":{"type":"string","description":"Path to the file to read (relative or absolute)"},"offset":{"type":"number","description":"Line number to start reading from (1-indexed)"},
                        "limit":{"type":"number","description":"Maximum number of lines to read"}
                    },"required":["path"]
                }),
            ),
            "Read file contents",
            ["Use read to examine files instead of cat or sed."],
        );
        tool.output_schema = Some(json!({"anyOf":[
            {"type":"string"},
            {"type":"object","properties":{
                "type":{"const":"image","type":"string"},
                "data":{"type":"string"},"mimeType":{"type":"string"},"note":{"type":"string"}
            },"required":["type","data","mimeType","note"]}
        ]}));
        tool.constrained_sampling = Some(pi_core::ConstrainedSampling::JsonSchema {
            strict: pi_core::ToolSchemaStrict::Prefer,
        });
        tool
    }

    async fn execute(
        &self,
        context: ToolContext,
        id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        execute_read(true, false, None, context, id, input, updates).await
    }
}

#[async_trait]
impl Tool for ConfiguredReadTool {
    fn spec(&self) -> ToolSpec {
        let mut spec = ReadTool.spec();
        if self.hashline {
            spec.parameters["properties"]["hashline"] = json!({"type":"boolean"});
        }
        spec
    }

    async fn execute(
        &self,
        context: ToolContext,
        id: ToolCallId,
        input: Value,
        updates: ToolUpdateSink,
    ) -> Result<ToolResult, ToolError> {
        execute_read(
            self.auto_resize_images,
            self.hashline,
            self.resize_options,
            context,
            id,
            input,
            updates,
        )
        .await
    }
}

async fn execute_read(
    auto_resize_images: bool,
    enable_hashline: bool,
    fallback_resize_options: Option<ModelImageResizeOptions>,
    context: ToolContext,
    _id: ToolCallId,
    input: Value,
    _updates: ToolUpdateSink,
) -> Result<ToolResult, ToolError> {
    context.signal().check().map_err(|_| ToolError::Aborted)?;
    let requested = require_str(&input, "path")?;
    let path = resolve_read_path(context.cwd(), requested)?;
    let requested_offset = optional_number(&input, "offset")?.unwrap_or(1.0);
    let limit = optional_number(&input, "limit")?;
    let hashline = enable_hashline
        && input
            .get("hashline")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let bytes = tokio::select! {
        biased;
        () = context.signal().wait() => return Err(ToolError::Aborted),
        result = tokio::fs::read(&path) => result
            .map_err(|e| execution(format!("cannot read {requested}: {e}")))?,
    };
    if let Some(mime) = detect_mime_type(&bytes) {
        let model = context.models.current().ok().flatten();
        let resize_options = model
            .as_ref()
            .and_then(|model| model.input_limits)
            .and_then(|limits| limits.images)
            .and_then(|images| images.resize)
            .or(fallback_resize_options)
            .unwrap_or_default();
        let policy = ImagePolicy {
            auto_resize: auto_resize_images,
            ..ImagePolicy::default()
        }
        .with_resize_options(resize_options);
        let processed = tokio::task::spawn_blocking(move || process_image(&bytes, &policy))
            .await
            .map_err(|error| execution(format!("image processing task failed: {error}")))?;
        context.signal().check().map_err(|_| ToolError::Aborted)?;
        let processed = match processed {
            Ok(processed) => processed,
            Err(error) => {
                let message = match error {
                    pi_media::MediaError::InvalidImage => pi_media::MediaError::InvalidImage,
                    _ => pi_media::MediaError::ImageSizeLimit,
                };
                let note = format!("Read image file [{mime}]\n{message}");
                let mut result = ToolResult::text(note.clone());
                result.structured_content = Some(Value::String(note));
                return Ok(result);
            }
        };
        let mut note = format!("Read image file [{}]", processed.content.mime_type);
        if !processed.hints.is_empty() {
            note.push('\n');
            note.push_str(&processed.hints.join("\n"));
        }
        let mut result = ToolResult::text(note.clone());
        result.structured_content = Some(json!({
            "type":"image", "data":processed.content.data,
            "mimeType":processed.content.mime_type, "note":note,
        }));
        result.content.push(ContentBlock::Image(processed.content));
        if let Some(model) = model.filter(|model| !model.input.contains(&ModelInput::Image)) {
            result.details = Some(json!({"imageUnsupportedByModel": model.id}));
        }
        return Ok(result);
    }

    let text = String::from_utf8_lossy(&bytes);
    let had_bom = text.starts_with('\u{feff}');
    let lines = text.split('\n').collect::<Vec<_>>();
    let start_line = (requested_offset - 1.0).max(0.0);
    let start = start_line as usize;
    let offset = start_line + 1.0;
    if start_line >= lines.len() as f64 {
        return Err(execution(format!(
            "Offset {requested_offset} is beyond end of file ({} lines total)",
            lines.len()
        )));
    }

    let requested_end = limit.map(|limit| (start_line + limit).min(lines.len() as f64));
    let selected_end = requested_end
        .map(|end| {
            let end = end.trunc();
            if end < 0.0 {
                lines.len().saturating_sub((-end) as usize)
            } else {
                end as usize
            }
        })
        .unwrap_or(lines.len())
        .max(start);
    let selected = if hashline {
        lines[start..selected_end]
            .iter()
            .enumerate()
            .map(|(relative, line)| {
                let line_index = start + relative;
                let line = if had_bom && line_index == 0 {
                    line.strip_prefix('\u{feff}').unwrap_or(line)
                } else {
                    line
                };
                format!("{}:{}", hashline_tag(line_index, line, had_bom), line)
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        lines[start..selected_end].join("\n")
    };
    context.signal().check().map_err(|_| ToolError::Aborted)?;

    let truncation = truncate_head(&selected);
    let mut details = None;
    let mut output = if truncation.first_line_exceeds_limit {
        details = Some(json!({"truncation": truncation.to_json()}));
        let first_line_bytes = lines[start].len();
        format!(
            "[Line {offset} is {}, exceeds {} limit. Use bash: sed -n '{offset}p' {requested} | head -c {MAX_OUTPUT_BYTES}]",
            format_size(first_line_bytes),
            format_size(MAX_OUTPUT_BYTES)
        )
    } else if truncation.truncated {
        let end_line = offset + truncation.output_lines.saturating_sub(1) as f64;
        let next_offset = end_line + 1.0;
        let mut output = truncation.content.clone();
        if truncation.truncated_by == Some(TruncatedBy::Lines) {
            output.push_str(&format!(
                    "\n\n[Showing lines {offset}-{end_line} of {}. Use offset={next_offset} to continue.]",
                    lines.len()
                ));
        } else {
            output.push_str(&format!(
                    "\n\n[Showing lines {offset}-{end_line} of {} ({} limit). Use offset={next_offset} to continue.]",
                    lines.len(),
                    format_size(MAX_OUTPUT_BYTES)
                ));
        }
        details = Some(json!({"truncation": truncation.to_json()}));
        output
    } else {
        truncation.content.clone()
    };

    if !truncation.truncated
        && let Some(end) = requested_end.filter(|end| *end < lines.len() as f64)
    {
        let remaining = lines.len() as f64 - end;
        output.push_str(&format!(
            "\n\n[{remaining} more lines in file. Use offset={} to continue.]",
            end + 1.0
        ));
    }
    let mut result = ToolResult::text(output.clone());
    result.structured_content = Some(Value::String(output));
    result.details = details;
    Ok(result)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TruncatedBy {
    Lines,
    Bytes,
}

#[derive(Debug)]
struct HeadTruncation {
    content: String,
    truncated: bool,
    truncated_by: Option<TruncatedBy>,
    total_lines: usize,
    total_bytes: usize,
    output_lines: usize,
    output_bytes: usize,
    first_line_exceeds_limit: bool,
}

impl HeadTruncation {
    fn to_json(&self) -> Value {
        json!({
            "content": self.content,
            "truncated": self.truncated,
            "truncatedBy": self.truncated_by.map(|reason| match reason {
                TruncatedBy::Lines => "lines",
                TruncatedBy::Bytes => "bytes",
            }),
            "totalLines": self.total_lines,
            "totalBytes": self.total_bytes,
            "outputLines": self.output_lines,
            "outputBytes": self.output_bytes,
            "lastLinePartial": false,
            "firstLineExceedsLimit": self.first_line_exceeds_limit,
            "maxLines": MAX_OUTPUT_LINES,
            "maxBytes": MAX_OUTPUT_BYTES,
        })
    }
}

fn truncate_head(content: &str) -> HeadTruncation {
    let lines = lines_for_counting(content);
    let total_lines = lines.len();
    let total_bytes = content.len();
    if total_lines <= MAX_OUTPUT_LINES && total_bytes <= MAX_OUTPUT_BYTES {
        return HeadTruncation {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            first_line_exceeds_limit: false,
        };
    }

    if lines
        .first()
        .is_some_and(|line| line.len() > MAX_OUTPUT_BYTES)
    {
        return HeadTruncation {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            first_line_exceeds_limit: true,
        };
    }

    let mut kept = Vec::new();
    let mut output_bytes = 0usize;
    let mut truncated_by = TruncatedBy::Lines;
    for line in lines.iter().take(MAX_OUTPUT_LINES) {
        let line_bytes = line.len() + usize::from(!kept.is_empty());
        if output_bytes.saturating_add(line_bytes) > MAX_OUTPUT_BYTES {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        kept.push(*line);
        output_bytes += line_bytes;
    }
    if kept.len() >= MAX_OUTPUT_LINES && output_bytes <= MAX_OUTPUT_BYTES {
        truncated_by = TruncatedBy::Lines;
    }
    let content = kept.join("\n");
    HeadTruncation {
        output_bytes: content.len(),
        output_lines: kept.len(),
        content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        first_line_exceeds_limit: false,
    }
}

fn lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines = content.split('\n').collect::<Vec<_>>();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use image::ImageFormat;
    use pi_core::AbortHandle;
    use std::io::Cursor;

    #[tokio::test]
    async fn hashline_is_explicitly_enabled_and_plain_reads_return_structured_text() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.txt"), "one\ntwo").unwrap();
        assert!(
            ReadTool.spec().parameters["properties"]
                .get("hashline")
                .is_none()
        );
        assert!(
            ConfiguredReadTool {
                resize_options: None,
                auto_resize_images: true,
                hashline: true
            }
            .spec()
            .parameters["properties"]
                .get("hashline")
                .is_some()
        );
        assert!(!ConfiguredReadPlugin::new(true).hashline);
        assert!(ConfiguredReadPlugin::new(true).with_hashline(true).hashline);
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ReadTool
            .execute(
                ToolContext::standalone(directory.path().into(), signal),
                ToolCallId::new("read"),
                json!({"path":"a.txt","hashline":true}),
                updates,
            )
            .await
            .unwrap();
        assert_eq!(result.structured_content, Some(json!("one\ntwo")));
        assert!(matches!(&result.content[0], ContentBlock::Text(text) if text.text == "one\ntwo"));
    }

    #[tokio::test]
    async fn returns_supported_image_block() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==")
            .unwrap();
        std::fs::write(dir.path().join("a.png"), &bytes).unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ReadTool
            .execute(
                ToolContext::standalone(dir.path().to_path_buf(), signal),
                ToolCallId::new("1"),
                json!({"path":"a.png"}),
                updates,
            )
            .await
            .unwrap();
        assert!(
            matches!(&result.content[1], ContentBlock::Image(image) if image.mime_type == "image/png" && base64::engine::general_purpose::STANDARD.decode(&image.data).unwrap() == bytes)
        );
        let ContentBlock::Image(image) = &result.content[1] else {
            unreachable!()
        };
        assert_eq!(
            result.structured_content,
            Some(json!({
                "type":"image", "data":image.data, "mimeType":"image/png", "note":"Read image file [image/png]",
            }))
        );
        assert!(result.details.is_none());
    }

    #[tokio::test]
    async fn configured_image_resize_setting_controls_dimension_processing() {
        let dir = tempfile::tempdir().unwrap();
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            2_001,
            1,
            image::Rgba([1, 2, 3, 255]),
        ));
        let mut encoded = Cursor::new(Vec::new());
        image.write_to(&mut encoded, ImageFormat::Png).unwrap();
        let original = encoded.into_inner();
        std::fs::write(dir.path().join("wide.png"), &original).unwrap();

        let execute = |auto_resize_images| {
            let cwd = dir.path().to_path_buf();
            async move {
                let (_, signal) = AbortHandle::new();
                let (updates, _) = ToolUpdateSink::channel();
                ConfiguredReadTool {
                    resize_options: None,
                    auto_resize_images,
                    hashline: false,
                }
                .execute(
                    ToolContext::standalone(cwd, signal),
                    ToolCallId::new("1"),
                    json!({"path":"wide.png"}),
                    updates,
                )
                .await
                .unwrap()
            }
        };

        let resized = execute(true).await;
        let ContentBlock::Image(resized_image) = &resized.content[1] else {
            panic!("expected image")
        };
        let resized_bytes = base64::engine::general_purpose::STANDARD
            .decode(&resized_image.data)
            .unwrap();
        let dimensions =
            image::load_from_memory_with_format(&resized_bytes, ImageFormat::Png).unwrap();
        assert_eq!((dimensions.width(), dimensions.height()), (2_000, 1));
        assert!(matches!(
            &resized.content[0],
            ContentBlock::Text(text) if text.text.contains("original 2001x1, displayed at 2000x1")
        ));

        let unchanged = execute(false).await;
        let ContentBlock::Image(unchanged_image) = &unchanged.content[1] else {
            panic!("expected image")
        };
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&unchanged_image.data)
                .unwrap(),
            original
        );
        assert!(matches!(
            &unchanged.content[0],
            ContentBlock::Text(text) if !text.text.contains("displayed at")
        ));
    }

    #[tokio::test]
    async fn numeric_ranges_follow_pi_line_slicing() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("range.txt"), "one\ntwo\nthree\nfour").unwrap();
        for (offset, limit, expected) in [
            (
                0.0,
                2.0,
                "one\ntwo\n\n[2 more lines in file. Use offset=3 to continue.]",
            ),
            (
                2.5,
                1.5,
                "two\nthree\n\n[1 more lines in file. Use offset=4 to continue.]",
            ),
            (
                1.0,
                -1.0,
                "one\ntwo\nthree\n\n[5 more lines in file. Use offset=0 to continue.]",
            ),
            (
                1.0,
                0.0,
                "\n\n[4 more lines in file. Use offset=1 to continue.]",
            ),
        ] {
            let (_, signal) = AbortHandle::new();
            let (updates, _) = ToolUpdateSink::channel();
            let result = ReadTool
                .execute(
                    ToolContext::standalone(directory.path().to_path_buf(), signal),
                    ToolCallId::new("range"),
                    json!({"path":"range.txt", "offset":offset, "limit":limit}),
                    updates,
                )
                .await
                .unwrap();
            assert!(
                matches!(&result.content[0], ContentBlock::Text(text) if text.text == expected),
                "offset={offset}, limit={limit}: {:?}",
                result.content
            );
        }
    }

    #[tokio::test]
    async fn reads_window_with_hashlines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ConfiguredReadTool {
            resize_options: None,
            auto_resize_images: true,
            hashline: true,
        }
        .execute(
            ToolContext::standalone(dir.path().to_path_buf(), signal),
            ToolCallId::new("1"),
            json!({"path":"a.txt","offset":2,"limit":1,"hashline":true}),
            updates,
        )
        .await
        .unwrap();
        let text = match &result.content[0] {
            pi_core::ContentBlock::Text(v) => &v.text,
            _ => panic!(),
        };
        assert!(text.starts_with("2#"));
        assert!(text.contains(":two"));
    }

    #[tokio::test]
    async fn preserves_text_exactly_without_hashlines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "\u{feff}one\r\ntwo\r\n").unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ReadTool
            .execute(
                ToolContext::standalone(dir.path().to_path_buf(), signal),
                ToolCallId::new("1"),
                json!({"path":"a.txt"}),
                updates,
            )
            .await
            .unwrap();
        let text = match &result.content[0] {
            pi_core::ContentBlock::Text(v) => &v.text,
            _ => panic!(),
        };
        assert_eq!(text, "\u{feff}one\r\ntwo\r\n");
        assert!(result.details.is_none());
    }

    #[tokio::test]
    async fn reads_an_absolute_path_outside_the_working_directory() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let target = root.path().join("shared").join("SKILL.md");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::create_dir(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "shared skill").unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();

        let result = ReadTool
            .execute(
                ToolContext::standalone(cwd, signal),
                ToolCallId::new("1"),
                json!({"path":target}),
                updates,
            )
            .await
            .unwrap();

        assert!(
            matches!(&result.content[0], ContentBlock::Text(text) if text.text.contains("shared skill"))
        );
    }

    #[tokio::test]
    async fn reports_oversized_first_line() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x".repeat(MAX_OUTPUT_BYTES + 1)).unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ReadTool
            .execute(
                ToolContext::standalone(dir.path().to_path_buf(), signal),
                ToolCallId::new("1"),
                json!({"path":"a.txt"}),
                updates,
            )
            .await
            .unwrap();
        let text = match &result.content[0] {
            pi_core::ContentBlock::Text(v) => &v.text,
            _ => panic!(),
        };
        assert!(text.contains("exceeds 50.0KB limit"));
        assert!(text.contains("head -c 51200"));
        assert_eq!(
            result
                .details
                .as_ref()
                .and_then(|details| details.pointer("/truncation/truncatedBy"))
                .and_then(Value::as_str),
            Some("bytes")
        );
    }

    #[tokio::test]
    async fn truncates_at_legacy_line_limit_and_reports_details() {
        let dir = tempfile::tempdir().unwrap();
        let content = (1..=2_500)
            .map(|line| format!("Line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("large.txt"), content).unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ReadTool
            .execute(
                ToolContext::standalone(dir.path().to_path_buf(), signal),
                ToolCallId::new("1"),
                json!({"path":"large.txt"}),
                updates,
            )
            .await
            .unwrap();
        let text = match &result.content[0] {
            ContentBlock::Text(value) => &value.text,
            _ => panic!("expected text"),
        };
        assert!(text.starts_with("Line 1\n"));
        assert!(text.contains("Line 2000"));
        assert!(!text.contains("Line 2001"));
        assert!(text.ends_with("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"));
        assert_eq!(
            result
                .details
                .as_ref()
                .and_then(|details| details.pointer("/truncation/truncatedBy"))
                .and_then(Value::as_str),
            Some("lines")
        );
    }

    #[test]
    fn truncation_counts_utf8_bytes_and_not_a_trailing_newline_as_a_line() {
        let content = format!("{}\n", vec!["line"; MAX_OUTPUT_LINES].join("\n"));
        let exact_limit = truncate_head(&content);
        assert!(!exact_limit.truncated);
        assert_eq!(exact_limit.total_lines, MAX_OUTPUT_LINES);
        assert_eq!(exact_limit.output_lines, MAX_OUTPUT_LINES);
        assert_eq!(exact_limit.total_bytes, content.len());

        let first = "é".repeat(12_500);
        let second = "🙂".repeat(8_000);
        let oversized = format!("{first}\n{second}");
        let by_bytes = truncate_head(&oversized);
        assert!(by_bytes.truncated);
        assert_eq!(by_bytes.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(by_bytes.content, first);
        assert_eq!(by_bytes.output_bytes, 25_000);
        assert!(!by_bytes.first_line_exceeds_limit);
    }

    #[tokio::test]
    async fn user_limit_gets_continuation_without_truncation_details() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\nfour").unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ReadTool
            .execute(
                ToolContext::standalone(dir.path().to_path_buf(), signal),
                ToolCallId::new("1"),
                json!({"path":"a.txt","offset":2,"limit":2}),
                updates,
            )
            .await
            .unwrap();
        assert!(
            matches!(&result.content[0], ContentBlock::Text(text) if text.text == "two\nthree\n\n[1 more lines in file. Use offset=4 to continue.]")
        );
        assert!(result.details.is_none());
    }

    #[tokio::test]
    async fn converts_bmp_to_png() {
        let dir = tempfile::tempdir().unwrap();
        let mut bmp = vec![0; 58];
        bmp[0..2].copy_from_slice(b"BM");
        bmp[2..6].copy_from_slice(&58_u32.to_le_bytes());
        bmp[10..14].copy_from_slice(&54_u32.to_le_bytes());
        bmp[14..18].copy_from_slice(&40_u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&1_i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&1_i32.to_le_bytes());
        bmp[26..28].copy_from_slice(&1_u16.to_le_bytes());
        bmp[28..30].copy_from_slice(&24_u16.to_le_bytes());
        bmp[34..38].copy_from_slice(&4_u32.to_le_bytes());
        bmp[56] = 0xff;
        std::fs::write(dir.path().join("a.bmp"), bmp).unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ReadTool
            .execute(
                ToolContext::standalone(dir.path().to_path_buf(), signal),
                ToolCallId::new("1"),
                json!({"path":"a.bmp"}),
                updates,
            )
            .await
            .unwrap();
        assert!(
            matches!(&result.content[0], ContentBlock::Text(text) if text.text.contains("Read image file [image/png]") && text.text.contains("converted from image/bmp"))
        );
        assert!(
            matches!(&result.content[1], ContentBlock::Image(image) if image.mime_type == "image/png" && base64::engine::general_purpose::STANDARD.decode(&image.data).unwrap().starts_with(b"\x89PNG"))
        );
    }

    #[test]
    fn hashline_matches_legacy_algorithm_properties() {
        assert_eq!(
            &hashline_tag(0, "hello world", false)[2..],
            &hashline_tag(8, " hello  world ", false)[2..]
        );
        assert_ne!(hashline_tag(0, "", false), hashline_tag(1, "", false));
        assert_ne!(
            hashline_tag(0, "hello", false),
            hashline_tag(0, "hello", true)
        );
    }
}
