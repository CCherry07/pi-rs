#![forbid(unsafe_code)]

use std::{future::Future, sync::Arc};

use async_trait::async_trait;
use pi_core::{
    AbortSignal, ContentBlock, ModelImageResizeOptions, ModelInput, PluginId, ToolCallId,
    ToolResult, ToolSpec,
};
use pi_media::image::{ImagePolicy, detect_mime_type, process_image};
use pi_plugin::{Plugin, RegisterContext, Tool, ToolContext, ToolError, ToolUpdateSink};
use pi_tool_support::{
    execution, hashline_tag, optional_number, require_str, resolve_read_path, spec, with_prompt,
};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};

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
const IMAGE_TYPE_SNIFF_BYTES: usize = 4_100;
const READ_CHUNK_BYTES: usize = 64 * 1024;

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
    let signal = context.signal().clone();

    let (reader, mime) = open_snapshot_stream(&path, &signal, requested).await?;

    if let Some(mime) = mime {
        let bytes = read_all(reader, &signal, requested).await?;
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
        signal.check().map_err(|_| ToolError::Aborted)?;
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

    let start_line = (requested_offset - 1.0).max(0.0);
    let start = start_line as usize;
    let (selected, requested_end) = if is_streamable_range(requested_offset, limit) {
        let end = limit.map(|limit| start.saturating_add(limit as usize));
        let selected =
            select_lines_from_stream(reader, start, end, hashline, &signal, requested).await?;
        let requested_end =
            limit.map(|limit| (start_line + limit).min(selected.total_file_lines as f64));
        (selected, requested_end)
    } else {
        let bytes = read_all(reader, &signal, requested).await?;
        select_lines_from_buffer(&bytes, start_line, limit, hashline)
    };
    signal.check().map_err(|_| ToolError::Aborted)?;

    if start_line >= selected.total_file_lines as f64 {
        return Err(execution(format!(
            "Offset {requested_offset} is beyond end of file ({} lines total)",
            selected.total_file_lines
        )));
    }

    render_text_result(requested, start_line, requested_end, selected)
}

async fn abortable<T>(
    signal: &AbortSignal,
    operation: impl Future<Output = T>,
) -> Result<T, ToolError> {
    tokio::select! {
        biased;
        () = signal.wait() => Err(ToolError::Aborted),
        output = operation => Ok(output),
    }
}

async fn open_snapshot_stream(
    path: &std::path::Path,
    signal: &AbortSignal,
    requested: &str,
) -> Result<(impl AsyncRead + Unpin, Option<&'static str>), ToolError> {
    let mut file = abortable(signal, tokio::fs::File::open(path))
        .await?
        .map_err(|error| execution(format!("cannot read {requested}: {error}")))?;
    let metadata = abortable(signal, file.metadata())
        .await?
        .map_err(|error| execution(format!("cannot read {requested}: {error}")))?;
    // A non-empty regular file is capped at the size observed after opening. Size-zero and
    // non-regular files follow Pi's stream to EOF behavior.
    let snapshot_limit = if metadata.is_file() && metadata.len() > 0 {
        metadata.len()
    } else {
        u64::MAX
    };
    let sniff_limit = snapshot_limit.min(IMAGE_TYPE_SNIFF_BYTES as u64) as usize;
    let mut sniff = vec![0; sniff_limit];
    // One read matches the legacy MIME sniffer and must not wait for a FIFO producer to close.
    let sniffed = if sniff.is_empty() {
        0
    } else {
        read_chunk(&mut file, &mut sniff, signal, requested).await?
    };
    sniff.truncate(sniffed);
    let mime = detect_mime_type(&sniff);
    // Replay the sniffed prefix once instead of seeking. This preserves FIFOs and procfs files.
    let reader = std::io::Cursor::new(sniff).chain(file).take(snapshot_limit);
    Ok((reader, mime))
}

async fn read_chunk(
    reader: &mut (impl AsyncRead + Unpin),
    buffer: &mut [u8],
    signal: &AbortSignal,
    requested: &str,
) -> Result<usize, ToolError> {
    abortable(signal, reader.read(buffer))
        .await?
        .map_err(|error| execution(format!("cannot read {requested}: {error}")))
}

async fn read_all(
    mut reader: impl AsyncRead + Unpin,
    signal: &AbortSignal,
    requested: &str,
) -> Result<Vec<u8>, ToolError> {
    let mut output = Vec::new();
    let mut chunk = vec![0; READ_CHUNK_BYTES];
    loop {
        let read = read_chunk(&mut reader, &mut chunk, signal, requested).await?;
        if read == 0 {
            return Ok(output);
        }
        output.extend_from_slice(&chunk[..read]);
    }
}

fn is_streamable_range(offset: f64, limit: Option<f64>) -> bool {
    offset.fract() == 0.0 && limit.is_none_or(|limit| limit.fract() == 0.0 && limit >= 0.0)
}

struct SelectedLines {
    total_file_lines: usize,
    truncation: HeadTruncation,
    first_line_bytes: usize,
}

fn select_lines_from_buffer(
    bytes: &[u8],
    start_line: f64,
    limit: Option<f64>,
    hashline: bool,
) -> (SelectedLines, Option<f64>) {
    let text = String::from_utf8_lossy(bytes);
    let had_bom = text.starts_with('\u{feff}');
    let lines = text.split('\n').collect::<Vec<_>>();
    let requested_end = limit.map(|limit| (start_line + limit).min(lines.len() as f64));
    if start_line >= lines.len() as f64 {
        return (
            SelectedLines {
                total_file_lines: lines.len(),
                truncation: truncate_head(""),
                first_line_bytes: 0,
            },
            requested_end,
        );
    }

    let start = start_line as usize;
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
            .map(|(relative, line)| format_hashline(start + relative, line, had_bom))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        lines[start..selected_end].join("\n")
    };
    let truncation = truncate_head(&selected);
    let first_line_bytes = if truncation.first_line_exceeds_limit {
        lines[start].len()
    } else {
        0
    };
    (
        SelectedLines {
            total_file_lines: lines.len(),
            truncation,
            first_line_bytes,
        },
        requested_end,
    )
}

fn format_hashline(line_index: usize, line: &str, had_bom: bool) -> String {
    let line = if had_bom && line_index == 0 {
        line.strip_prefix('\u{feff}').unwrap_or(line)
    } else {
        line
    };
    format!("{}:{}", hashline_tag(line_index, line, had_bom), line)
}

fn render_text_result(
    requested: &str,
    start_line: f64,
    requested_end: Option<f64>,
    selected: SelectedLines,
) -> Result<ToolResult, ToolError> {
    let offset = start_line + 1.0;
    let truncation = selected.truncation;
    let mut details = None;
    let mut output = if truncation.first_line_exceeds_limit {
        details = Some(json!({"truncation": truncation.to_json()}));
        format!(
            "[Line {offset} is {}, exceeds {} limit. Use bash: sed -n '{offset}p' {requested} | head -c {MAX_OUTPUT_BYTES}]",
            format_size(selected.first_line_bytes),
            format_size(MAX_OUTPUT_BYTES)
        )
    } else if truncation.truncated {
        let end_line = offset + truncation.output_lines.saturating_sub(1) as f64;
        let next_offset = end_line + 1.0;
        let mut output = truncation.content.clone();
        if truncation.truncated_by == Some(TruncatedBy::Lines) {
            output.push_str(&format!(
                "\n\n[Showing lines {offset}-{end_line} of {}. Use offset={next_offset} to continue.]",
                selected.total_file_lines
            ));
        } else {
            output.push_str(&format!(
                "\n\n[Showing lines {offset}-{end_line} of {} ({} limit). Use offset={next_offset} to continue.]",
                selected.total_file_lines,
                format_size(MAX_OUTPUT_BYTES)
            ));
        }
        details = Some(json!({"truncation": truncation.to_json()}));
        output
    } else {
        truncation.content.clone()
    };

    if !truncation.truncated
        && let Some(end) = requested_end.filter(|end| *end < selected.total_file_lines as f64)
    {
        let remaining = selected.total_file_lines as f64 - end;
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

struct StreamLine {
    captured: Option<String>,
    bytes: usize,
}

impl StreamLine {
    fn new(capture: bool) -> Self {
        Self {
            captured: capture.then(String::new),
            bytes: 0,
        }
    }
}

struct StreamingSelector {
    start_line: usize,
    end_line: Option<usize>,
    hashline: bool,
    line_index: usize,
    line: StreamLine,
    had_bom: bool,
    selected_lines: usize,
    selected_bytes: usize,
    first_line_bytes: usize,
    last_line_empty: bool,
    stored_lines: Vec<String>,
    stored_bytes: usize,
    stored_past_caps: bool,
}

impl StreamingSelector {
    fn new(start_line: usize, end_line: Option<usize>, hashline: bool) -> Self {
        let capture = start_line == 0 && end_line.is_none_or(|end| end > 0);
        Self {
            start_line,
            end_line,
            hashline,
            line_index: 0,
            line: StreamLine::new(capture),
            had_bom: false,
            selected_lines: 0,
            selected_bytes: 0,
            first_line_bytes: 0,
            last_line_empty: true,
            stored_lines: Vec::new(),
            stored_bytes: 0,
            stored_past_caps: false,
        }
    }

    fn selected(&self) -> bool {
        self.line_index >= self.start_line && self.end_line.is_none_or(|end| self.line_index < end)
    }

    fn push(&mut self, text: &str) {
        let mut position = 0;
        loop {
            let newline = text[position..].find('\n').map(|index| position + index);
            let end = newline.unwrap_or(text.len());
            self.append_to_line(&text[position..end]);
            let Some(newline) = newline else {
                break;
            };
            self.finish_line();
            position = newline + 1;
        }
    }

    fn append_to_line(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.line_index == 0 && self.line.bytes == 0 && text.starts_with('\u{feff}') {
            self.had_bom = true;
        }
        if !self.selected() {
            // Keep raw progress for line zero even when it is outside the requested window, so
            // hashline reads retain the file's BOM state.
            self.line.bytes = self.line.bytes.saturating_add(text.len());
            return;
        }
        if let Some(captured) = self.line.captured.as_mut()
            && self.line.bytes <= MAX_OUTPUT_BYTES
        {
            // As in the TypeScript stream, retain at most one chunk past the cap. That is
            // enough for truncateHead to classify the line without retaining the whole line.
            captured.push_str(text);
        }
        self.line.bytes = self.line.bytes.saturating_add(text.len());
    }

    fn finish_line(&mut self) {
        if self.selected() {
            if self.selected_lines == 0 {
                self.first_line_bytes = self.line.bytes;
            }
            let transformed_bytes = if self.hashline {
                let display_bytes = self.line.bytes.saturating_sub(
                    usize::from(self.had_bom && self.line_index == 0) * '\u{feff}'.len_utf8(),
                );
                (self.line_index + 1).to_string().len() + 4 + display_bytes
            } else {
                self.line.bytes
            };
            self.selected_bytes = self
                .selected_bytes
                .saturating_add(usize::from(self.selected_lines > 0))
                .saturating_add(transformed_bytes);
            self.selected_lines += 1;
            self.last_line_empty = transformed_bytes == 0;

            if let Some(captured) = self.line.captured.take() {
                if self.stored_lines.len() > MAX_OUTPUT_LINES
                    || self.stored_bytes > MAX_OUTPUT_BYTES
                {
                    self.stored_past_caps = true;
                }
                let captured = if self.hashline {
                    format_hashline(self.line_index, &captured, self.had_bom)
                } else {
                    captured
                };
                self.stored_bytes = self
                    .stored_bytes
                    .saturating_add(usize::from(!self.stored_lines.is_empty()))
                    .saturating_add(captured.len());
                self.stored_lines.push(captured);
            }
        }

        self.line_index += 1;
        let capture = !self.stored_past_caps
            && self.line_index >= self.start_line
            && self.end_line.is_none_or(|end| self.line_index < end);
        self.line = StreamLine::new(capture);
    }

    fn finish(mut self) -> SelectedLines {
        self.finish_line();
        let mut truncation = truncate_head(&self.stored_lines.join("\n"));
        truncation.total_lines = if self.selected_bytes == 0 {
            0
        } else {
            self.selected_lines - usize::from(self.last_line_empty)
        };
        truncation.total_bytes = self.selected_bytes;
        SelectedLines {
            total_file_lines: self.line_index,
            truncation,
            first_line_bytes: self.first_line_bytes,
        }
    }
}

#[derive(Default)]
struct LossyUtf8Decoder {
    buffer: Vec<u8>,
}

impl LossyUtf8Decoder {
    fn decode(&mut self, bytes: &[u8], output: &mut impl FnMut(&str)) {
        let mut buffer = std::mem::take(&mut self.buffer);
        buffer.extend_from_slice(bytes);
        let mut position = 0;
        let mut incomplete = None;
        while position < buffer.len() {
            match std::str::from_utf8(&buffer[position..]) {
                Ok(valid) => {
                    output(valid);
                    position = buffer.len();
                }
                Err(error) => {
                    let valid_end = position + error.valid_up_to();
                    if valid_end > position {
                        output(
                            std::str::from_utf8(&buffer[position..valid_end])
                                .expect("valid prefix"),
                        );
                    }
                    position = valid_end;
                    if let Some(length) = error.error_len() {
                        output("\u{fffd}");
                        position += length;
                    } else {
                        incomplete = Some(position);
                        break;
                    }
                }
            }
        }
        if let Some(start) = incomplete {
            let length = buffer.len() - start;
            buffer.copy_within(start.., 0);
            buffer.truncate(length);
        } else {
            buffer.clear();
        }
        self.buffer = buffer;
    }

    fn finish(&mut self, output: &mut impl FnMut(&str)) {
        if !self.buffer.is_empty() {
            output("\u{fffd}");
            self.buffer.clear();
        }
    }
}

async fn select_lines_from_stream(
    mut reader: impl AsyncRead + Unpin,
    start_line: usize,
    end_line: Option<usize>,
    hashline: bool,
    signal: &AbortSignal,
    requested: &str,
) -> Result<SelectedLines, ToolError> {
    let mut selector = StreamingSelector::new(start_line, end_line, hashline);
    let mut decoder = LossyUtf8Decoder::default();
    let mut chunk = vec![0; READ_CHUNK_BYTES];
    loop {
        let read = read_chunk(&mut reader, &mut chunk, signal, requested).await?;
        if read == 0 {
            break;
        }
        decoder.decode(&chunk[..read], &mut |text| selector.push(text));
    }
    decoder.finish(&mut |text| selector.push(text));
    Ok(selector.finish())
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
    use std::{
        future::pending,
        io::Cursor,
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        task::{Context, Poll},
    };
    use tokio::io::ReadBuf;

    struct ChunkedReader {
        bytes: Vec<u8>,
        position: usize,
        chunk_size: usize,
    }

    impl ChunkedReader {
        fn new(bytes: &[u8], chunk_size: usize) -> Self {
            Self {
                bytes: bytes.to_vec(),
                position: 0,
                chunk_size,
            }
        }
    }

    impl AsyncRead for ChunkedReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.position == self.bytes.len() {
                return Poll::Ready(Ok(()));
            }
            let count = self
                .chunk_size
                .min(buffer.remaining())
                .min(self.bytes.len() - self.position);
            let end = self.position + count;
            buffer.put_slice(&self.bytes[self.position..end]);
            self.position = end;
            Poll::Ready(Ok(()))
        }
    }

    struct FirstChunkThenPending {
        sent: bool,
        dropped: Arc<AtomicBool>,
    }

    impl AsyncRead for FirstChunkThenPending {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.sent {
                return Poll::Pending;
            }
            buffer.put_slice(b"first\n");
            self.sent = true;
            Poll::Ready(Ok(()))
        }
    }

    impl Drop for FirstChunkThenPending {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    fn assert_same_selection(actual: &SelectedLines, expected: &SelectedLines, label: &str) {
        assert_eq!(
            actual.total_file_lines, expected.total_file_lines,
            "{label}"
        );
        if expected.truncation.first_line_exceeds_limit {
            assert_eq!(
                actual.first_line_bytes, expected.first_line_bytes,
                "{label}"
            );
        }
        assert_eq!(
            actual.truncation.to_json(),
            expected.truncation.to_json(),
            "{label}"
        );
    }

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
    async fn offset_and_limit_windows_match_legacy_continuation_notices() {
        let directory = tempfile::tempdir().unwrap();
        let content = (1..=100)
            .map(|line| format!("Line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(directory.path().join("windows.txt"), content).unwrap();
        for (input, expected) in [
            (json!({"path":"windows.txt", "offset":51}), (51, 100, None)),
            (
                json!({"path":"windows.txt", "limit":10}),
                (
                    1,
                    10,
                    Some("[90 more lines in file. Use offset=11 to continue.]"),
                ),
            ),
            (
                json!({"path":"windows.txt", "offset":41, "limit":20}),
                (
                    41,
                    60,
                    Some("[40 more lines in file. Use offset=61 to continue.]"),
                ),
            ),
        ] {
            let (_, signal) = AbortHandle::new();
            let (updates, _) = ToolUpdateSink::channel();
            let result = ReadTool
                .execute(
                    ToolContext::standalone(directory.path().to_path_buf(), signal),
                    ToolCallId::new("window"),
                    input,
                    updates,
                )
                .await
                .unwrap();
            let ContentBlock::Text(text) = &result.content[0] else {
                panic!("expected text")
            };
            assert!(
                text.text
                    .lines()
                    .any(|line| line == format!("Line {}", expected.0))
            );
            assert!(
                text.text
                    .lines()
                    .any(|line| line == format!("Line {}", expected.1))
            );
            if expected.0 > 1 {
                assert!(
                    !text
                        .text
                        .lines()
                        .any(|line| line == format!("Line {}", expected.0 - 1))
                );
            }
            assert_eq!(
                text.text.split("\n\n").nth(1),
                expected.2,
                "unexpected continuation notice"
            );
            assert!(result.details.is_none());
        }
    }

    #[tokio::test]
    async fn reads_bom_file_window_with_hashlines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "\u{feff}one\ntwo\nthree\n").unwrap();
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
        assert_eq!(
            text,
            &format!(
                "{}:two\n\n[2 more lines in file. Use offset=3 to continue.]",
                hashline_tag(1, "two", true)
            )
        );

        // The public output for a later line intentionally has the same tag bytes either way, so
        // also pin the file-level BOM state that hashline edit validation consumes.
        let mut selector = StreamingSelector::new(1, Some(2), true);
        selector.push("\u{feff}one\ntwo\n");
        assert!(selector.had_bom);
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
    async fn relative_reads_use_the_tool_context_cwd() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("ctx-cwd-read.txt"),
            "hello from ctx.cwd",
        )
        .unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();

        let result = ReadTool
            .execute(
                ToolContext::standalone(directory.path().to_path_buf(), signal),
                ToolCallId::new("cwd"),
                json!({"path":"ctx-cwd-read.txt"}),
                updates,
            )
            .await
            .unwrap();

        assert!(matches!(
            &result.content[0],
            ContentBlock::Text(text) if text.text == "hello from ctx.cwd"
        ));
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
        let truncation = &result.details.as_ref().unwrap()["truncation"];
        assert_eq!(truncation["truncated"], true);
        assert_eq!(truncation["truncatedBy"], "lines");
        assert_eq!(truncation["totalLines"], 2_500);
        assert_eq!(truncation["outputLines"], 2_000);
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

    #[tokio::test]
    async fn streaming_matches_whole_file_output_ranges_bytes_and_utf8() {
        let exact_fifty_kb = format!(
            "{}y",
            (0..512)
                .map(|_| "y".repeat(99))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let cases = vec![
            ("empty", String::new()),
            ("no trailing newline", "alpha\nbeta\ngamma".to_string()),
            ("trailing newline", "alpha\nbeta\n".to_string()),
            ("CRLF", "alpha\r\nbeta\r\n\r\ngamma\r\n".to_string()),
            (
                "multi-byte characters and UTF-8 byte order mark",
                "\u{feff}é€😀\nñ𐍈字\n😀😀😀".to_string(),
            ),
            (
                "2001 lines",
                (0..2_001)
                    .map(|line| format!("line {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            (
                "2001 lines, an empty line and more",
                format!(
                    "{}\n\nmore",
                    (0..2_001)
                        .map(|line| format!("line {line}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            ),
            (
                "first line over 50KB",
                format!("{}\nsecond", "é".repeat(MAX_OUTPUT_BYTES / 2 + 1)),
            ),
            (
                "line over 50KB after short lines",
                format!("a\nb\n{}\nc", "z".repeat(60 * 1024)),
            ),
            (
                "exactly 50KB and a trailing newline",
                format!("{exact_fifty_kb}\n"),
            ),
            (
                "exactly 50KB, an empty line and more",
                format!("{exact_fifty_kb}\n\nmore"),
            ),
            (
                "multi-byte characters over 50KB",
                (0..700)
                    .map(|line| format!("{line} {}", "€😀".repeat(13)))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        ];

        for (name, text) in cases {
            let bytes = text.as_bytes();
            let total = text.split('\n').count();
            for offset in [1.0, 2.0, 3.0, total as f64, total as f64 + 1.0] {
                for limit in [None, Some(0.0), Some(1.0), Some(total as f64)] {
                    let start_line = (offset - 1.0_f64).max(0.0);
                    let start = start_line as usize;
                    let end = limit.map(|limit| start.saturating_add(limit as usize));
                    let (expected, _) = select_lines_from_buffer(bytes, start_line, limit, false);
                    for chunk_size in [1, 4_096] {
                        let (_, signal) = AbortHandle::new();
                        let actual = select_lines_from_stream(
                            ChunkedReader::new(bytes, chunk_size),
                            start,
                            end,
                            false,
                            &signal,
                            "fixture.txt",
                        )
                        .await
                        .unwrap();
                        assert_same_selection(
                            &actual,
                            &expected,
                            &format!(
                                "{name}, offset {offset}, limit {limit:?}, chunks {chunk_size}"
                            ),
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn streaming_decoder_matches_lossy_whole_file_for_split_and_invalid_utf8() {
        let mut bytes = "aé€😀\n😀b𐍈\n".as_bytes().to_vec();
        bytes.extend([
            0x61, 0xc3, 0x0a, 0xe2, 0x82, 0x0a, 0xff, 0xfe, 0x80, 0xf0, 0x9f, 0x98, 0x0a, 0xed,
            0xa0, 0x80,
        ]);
        let (expected, _) = select_lines_from_buffer(&bytes, 0.0, None, false);
        for chunk_size in [1, 2, 3] {
            let (_, signal) = AbortHandle::new();
            let actual = select_lines_from_stream(
                ChunkedReader::new(&bytes, chunk_size),
                0,
                None,
                false,
                &signal,
                "invalid.bin",
            )
            .await
            .unwrap();
            assert_same_selection(&actual, &expected, &format!("chunks {chunk_size}"));
        }
    }

    #[tokio::test]
    async fn streaming_details_keep_legacy_serialization_order() {
        let bytes = (0..2_001)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes();
        let (expected, _) = select_lines_from_buffer(&bytes, 0.0, None, false);
        let (_, signal) = AbortHandle::new();
        let actual = select_lines_from_stream(
            ChunkedReader::new(&bytes, 4_096),
            0,
            None,
            false,
            &signal,
            "lines.txt",
        )
        .await
        .unwrap();
        let actual_json = actual.truncation.to_json();
        assert_eq!(
            actual_json.as_object().unwrap().keys().collect::<Vec<_>>(),
            [
                "content",
                "truncated",
                "truncatedBy",
                "totalLines",
                "totalBytes",
                "outputLines",
                "outputBytes",
                "lastLinePartial",
                "firstLineExceedsLimit",
                "maxLines",
                "maxBytes",
            ]
        );
        assert_eq!(
            serde_json::to_string(&actual_json).unwrap(),
            serde_json::to_string(&expected.truncation.to_json()).unwrap()
        );
    }

    #[tokio::test]
    async fn snapshot_length_excludes_bytes_appended_after_open() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("growing.txt");
        std::fs::write(&path, b"head\nbody").unwrap();
        let (_, signal) = AbortHandle::new();
        let (reader, mime) = open_snapshot_stream(&path, &signal, "growing.txt")
            .await
            .unwrap();
        assert!(mime.is_none());
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\nadded")
            .unwrap();

        let selected = select_lines_from_stream(reader, 0, Some(1), false, &signal, "growing.txt")
            .await
            .unwrap();
        assert_eq!(selected.total_file_lines, 2);
        let result = render_text_result("growing.txt", 0.0, Some(1.0), selected).unwrap();
        assert!(matches!(
            &result.content[0],
            ContentBlock::Text(text)
                if text.text == "head\n\n[1 more lines in file. Use offset=2 to continue.]"
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn public_read_supports_a_non_seekable_fifo_without_losing_the_sniffed_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let fifo = directory.path().join("stream.fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());

        let writer_path = fifo.clone();
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt as _;
            let mut output = tokio::fs::OpenOptions::new()
                .write(true)
                .open(writer_path)
                .await
                .unwrap();
            output.write_all(b"fifo\ncontent").await.unwrap();
        });
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ReadTool.execute(
                ToolContext::standalone(directory.path().to_path_buf(), signal),
                ToolCallId::new("fifo"),
                json!({"path":"stream.fifo"}),
                updates,
            ),
        )
        .await
        .expect("FIFO read timed out")
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), writer)
            .await
            .expect("FIFO writer timed out")
            .unwrap();
        assert!(matches!(
            &result.content[0],
            ContentBlock::Text(text) if text.text == "fifo\ncontent"
        ));
    }

    #[tokio::test]
    async fn abort_before_open_wins_over_filesystem_errors() {
        let directory = tempfile::tempdir().unwrap();
        let (handle, signal) = AbortHandle::new();
        handle.abort();
        let (updates, _) = ToolUpdateSink::channel();
        let error = ReadTool
            .execute(
                ToolContext::standalone(directory.path().to_path_buf(), signal),
                ToolCallId::new("aborted"),
                json!({"path":"missing.txt"}),
                updates,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::Aborted));
    }

    #[tokio::test]
    async fn abort_during_open_cancels_and_drops_the_open_operation() {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let (handle, signal) = AbortHandle::new();
        let task = tokio::spawn({
            let dropped = dropped.clone();
            async move {
                abortable(&signal, async move {
                    let _drop = DropFlag(dropped);
                    pending::<std::io::Result<()>>().await
                })
                .await
            }
        });
        tokio::task::yield_now().await;
        handle.abort();
        assert!(matches!(task.await.unwrap(), Err(ToolError::Aborted)));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn abort_midstream_stops_consumption_and_drops_the_reader() {
        let dropped = Arc::new(AtomicBool::new(false));
        let (handle, signal) = AbortHandle::new();
        let task = tokio::spawn({
            let dropped = dropped.clone();
            async move {
                select_lines_from_stream(
                    FirstChunkThenPending {
                        sent: false,
                        dropped,
                    },
                    0,
                    None,
                    false,
                    &signal,
                    "pending.txt",
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        handle.abort();
        assert!(matches!(task.await.unwrap(), Err(ToolError::Aborted)));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn nonexistent_and_out_of_range_files_report_legacy_errors() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("short.txt"), "Line 1\nLine 2\nLine 3").unwrap();
        for (input, expected) in [
            (json!({"path":"missing.txt"}), "cannot read missing.txt"),
            (json!({"path":"."}), "cannot read ."),
            (
                json!({"path":"short.txt", "offset":100}),
                "Offset 100 is beyond end of file (3 lines total)",
            ),
        ] {
            let (_, signal) = AbortHandle::new();
            let (updates, _) = ToolUpdateSink::channel();
            let error = ReadTool
                .execute(
                    ToolContext::standalone(directory.path().to_path_buf(), signal),
                    ToolCallId::new("error"),
                    input,
                    updates,
                )
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error:?}");
        }
    }

    #[tokio::test]
    async fn image_detection_uses_magic_not_the_filename_suffix() {
        let directory = tempfile::tempdir().unwrap();
        let png = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgYGD4DwABBAEAX+XDSwAAAABJRU5ErkJggg==")
            .unwrap();
        std::fs::write(directory.path().join("image.txt"), png).unwrap();
        std::fs::write(
            directory.path().join("not-an-image.png"),
            "definitely not a png",
        )
        .unwrap();

        for (path, expect_image) in [("image.txt", true), ("not-an-image.png", false)] {
            let (_, signal) = AbortHandle::new();
            let (updates, _) = ToolUpdateSink::channel();
            let result = ReadTool
                .execute(
                    ToolContext::standalone(directory.path().to_path_buf(), signal),
                    ToolCallId::new(path),
                    json!({"path":path}),
                    updates,
                )
                .await
                .unwrap();
            assert_eq!(
                result
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Image(_))),
                expect_image
            );
        }
    }

    #[tokio::test]
    async fn byte_limit_keeps_complete_utf8_lines_and_reports_exact_details() {
        let directory = tempfile::tempdir().unwrap();
        let content = (1..=500)
            .map(|line| format!("Line {line}: {}", "é😀".repeat(40)))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(directory.path().join("large-bytes.txt"), &content).unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ReadTool
            .execute(
                ToolContext::standalone(directory.path().to_path_buf(), signal),
                ToolCallId::new("bytes"),
                json!({"path":"large-bytes.txt"}),
                updates,
            )
            .await
            .unwrap();
        let details = result.details.as_ref().unwrap();
        assert_eq!(details["truncation"]["truncatedBy"], "bytes");
        assert_eq!(details["truncation"]["totalBytes"], content.len());
        let ContentBlock::Text(output) = &result.content[0] else {
            panic!("expected text output")
        };
        let shown = output.text.split("\n\n[Showing lines").next().unwrap();
        assert!(std::str::from_utf8(shown.as_bytes()).is_ok());
        assert!(shown.lines().all(|line| line.starts_with("Line ")));
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
