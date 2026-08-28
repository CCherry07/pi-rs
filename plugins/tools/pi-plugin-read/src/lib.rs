#![forbid(unsafe_code)]

use std::io::Cursor;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use image::ImageFormat;
use pi_core::{
    AgentPlugin, ContentBlock, ImageContent, PluginId, RegisterContext, Tool, ToolCallId,
    ToolContext, ToolError, ToolResult, ToolSpec, ToolUpdateSink,
};
use pi_tool_support::{
    execution, hashline_tag, optional_positive_usize, require_str, resolve_read_path, spec,
    with_prompt,
};
use serde_json::{Value, json};

pub struct ReadPlugin;
pub struct ReadTool;

const MAX_OUTPUT_BYTES: usize = 50 * 1024;
const MAX_OUTPUT_LINES: usize = 2_000;

#[pi_core::agent_plugin]
impl AgentPlugin for ReadPlugin {
    fn id(&self) -> PluginId {
        PluginId::new("read")
    }
    fn register(&self, context: &mut RegisterContext<'_>) -> pi_core::Result<()> {
        context.register_tool(Arc::new(ReadTool))
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        with_prompt(
            spec(
                "read",
                "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. Text output is limited to 2000 lines or 50KB; use offset/limit to continue, or hashline for stable edit anchors.",
                json!({
                    "type":"object","properties":{
                        "path":{"type":"string","description":"Path to the file to read (relative or absolute)"},"offset":{"type":"integer","minimum":1,"description":"Line number to start reading from (1-indexed)"},
                        "limit":{"type":"integer","minimum":1,"description":"Maximum number of lines to read"},"hashline":{"type":"boolean"}
                    },"required":["path"],"additionalProperties":false
                }),
            ),
            "Read file contents",
            ["Use read to examine files instead of cat or sed."],
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
        let path = resolve_read_path(&context.cwd, requested)?;
        let offset = optional_positive_usize(&input, "offset", 1)?;
        let limit = input
            .get("limit")
            .filter(|value| !value.is_null())
            .map(|_| optional_positive_usize(&input, "limit", MAX_OUTPUT_LINES))
            .transpose()?;
        let hashline = input
            .get("hashline")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let bytes = tokio::select! {
            biased;
            () = context.abort_signal.wait() => return Err(ToolError::Aborted),
            result = tokio::fs::read(&path) => result
                .map_err(|e| execution(format!("cannot read {requested}: {e}")))?,
        };
        if let Some(mime_type) = image_mime_type(&bytes) {
            let processed = process_image(bytes, mime_type).await?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&processed.bytes);
            let mut note = format!("Read image file [{}]", processed.mime_type);
            if let Some(hint) = processed.hint {
                note.push('\n');
                note.push_str(&hint);
            }
            let mut result = ToolResult::text(note);
            result.content.push(ContentBlock::Image(ImageContent {
                data: encoded,
                mime_type: processed.mime_type.to_string(),
            }));
            return Ok(result);
        }

        let text = String::from_utf8_lossy(&bytes);
        let had_bom = text.starts_with('\u{feff}');
        let lines = text.split('\n').collect::<Vec<_>>();
        let start = offset - 1;
        if start >= lines.len() {
            return Err(execution(format!(
                "Offset {offset} is beyond end of file ({} lines total)",
                lines.len()
            )));
        }

        let selected_end = limit
            .map(|limit| start.saturating_add(limit).min(lines.len()))
            .unwrap_or(lines.len());
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
        context
            .abort_signal
            .check()
            .map_err(|_| ToolError::Aborted)?;

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
            let end_line = offset + truncation.output_lines.saturating_sub(1);
            let next_offset = end_line + 1;
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

        if !truncation.truncated && limit.is_some() && selected_end < lines.len() {
            let remaining = lines.len() - selected_end;
            output.push_str(&format!(
                "\n\n[{remaining} more lines in file. Use offset={} to continue.]",
                selected_end + 1
            ));
        }
        let mut result = ToolResult::text(output);
        result.details = details;
        Ok(result)
    }
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

struct ProcessedImage {
    bytes: Vec<u8>,
    mime_type: &'static str,
    hint: Option<String>,
}

async fn process_image(
    bytes: Vec<u8>,
    mime_type: &'static str,
) -> Result<ProcessedImage, ToolError> {
    if mime_type != "image/bmp" {
        return Ok(ProcessedImage {
            bytes,
            mime_type,
            hint: None,
        });
    }

    tokio::task::spawn_blocking(move || {
        let image =
            image::load_from_memory_with_format(&bytes, ImageFormat::Bmp).map_err(|_| {
                execution(
                    "[Image omitted: could not be converted to a supported inline image format.]",
                )
            })?;
        let mut output = Cursor::new(Vec::new());
        image.write_to(&mut output, ImageFormat::Png).map_err(|_| {
            execution("[Image omitted: could not be converted to a supported inline image format.]")
        })?;
        Ok(ProcessedImage {
            bytes: output.into_inner(),
            mime_type: "image/png",
            hint: Some("[Image converted from image/bmp to image/png.]".to_string()),
        })
    })
    .await
    .map_err(|error| execution(format!("image processing task failed: {error}")))?
}

fn image_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        && bytes.get(12..16) == Some(b"IHDR")
        && !is_animated_png(bytes)
    {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) && bytes.get(3) != Some(&0xf7) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else if is_bmp(bytes) {
        Some("image/bmp")
    } else {
        None
    }
}

fn is_animated_png(bytes: &[u8]) -> bool {
    let mut offset = 8usize;
    while offset.saturating_add(8) <= bytes.len().min(4_100) {
        let Some(length_bytes) = bytes.get(offset..offset + 4) else {
            return false;
        };
        let length = u32::from_be_bytes(length_bytes.try_into().expect("four bytes")) as usize;
        let chunk_type = bytes.get(offset + 4..offset + 8);
        if chunk_type == Some(b"acTL") {
            return true;
        }
        if chunk_type == Some(b"IDAT") {
            return false;
        }
        let next = offset.saturating_add(12).saturating_add(length);
        if next <= offset || next > bytes.len().min(4_100) {
            return false;
        }
        offset = next;
    }
    false
}

fn is_bmp(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"BM") || bytes.len() < 30 {
        return false;
    }
    let read_u16 = |offset: usize| {
        bytes
            .get(offset..offset + 2)
            .and_then(|value| value.try_into().ok())
            .map(u16::from_le_bytes)
    };
    let read_u32 = |offset: usize| {
        bytes
            .get(offset..offset + 4)
            .and_then(|value| value.try_into().ok())
            .map(u32::from_le_bytes)
    };
    let Some(file_size) = read_u32(2) else {
        return false;
    };
    let Some(pixel_offset) = read_u32(10) else {
        return false;
    };
    let Some(header_size) = read_u32(14) else {
        return false;
    };
    if (file_size != 0 && file_size < 26)
        || pixel_offset < 14 + header_size
        || (file_size != 0 && pixel_offset >= file_size)
    {
        return false;
    }
    let (planes, bits) = if header_size == 12 {
        (read_u16(22), read_u16(24))
    } else if (40..=124).contains(&header_size) {
        (read_u16(26), read_u16(28))
    } else {
        return false;
    };
    planes == Some(1) && matches!(bits, Some(1 | 4 | 8 | 16 | 24 | 32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;

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
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
                ToolCallId::new("1"),
                json!({"path":"a.png"}),
                updates,
            )
            .await
            .unwrap();
        assert!(
            matches!(&result.content[1], ContentBlock::Image(image) if image.mime_type == "image/png" && base64::engine::general_purpose::STANDARD.decode(&image.data).unwrap() == bytes)
        );
        assert!(result.details.is_none());
    }

    #[tokio::test]
    async fn reads_window_with_hashlines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree\n").unwrap();
        let (_, signal) = AbortHandle::new();
        let (updates, _) = ToolUpdateSink::channel();
        let result = ReadTool
            .execute(
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
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
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
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
                ToolContext {
                    cwd,
                    abort_signal: signal,
                },
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
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
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
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
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
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
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
                ToolContext {
                    cwd: dir.path().to_path_buf(),
                    abort_signal: signal,
                },
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
