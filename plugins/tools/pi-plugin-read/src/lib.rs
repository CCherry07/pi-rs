#![forbid(unsafe_code)]

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use pi_core::{
    AgentPlugin, ContentBlock, ImageContent, PluginId, RegisterContext, Tool, ToolCallId,
    ToolContext, ToolError, ToolResult, ToolSpec, ToolUpdateSink,
};
use pi_tool_support::{
    MAX_OUTPUT_BYTES, execution, hashline_tag, optional_positive_usize, require_str,
    resolve_read_path, spec, with_prompt,
};
use serde_json::{Value, json};

pub struct ReadPlugin;
pub struct ReadTool;

#[async_trait]
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
                "Read UTF-8 text files and images (jpg, png, gif, webp). Text lines are numbered and limited to 2000 lines or 1MB; use offset/limit to continue, or hashline for stable edit anchors.",
                json!({
                    "type":"object","properties":{
                        "path":{"type":"string","description":"Path to the file to read (relative or absolute)"},"offset":{"type":"integer","minimum":1},
                        "limit":{"type":"integer","minimum":1},"hashline":{"type":"boolean"}
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
        let limit = optional_positive_usize(&input, "limit", 2000)?.min(2000);
        let hashline = input
            .get("hashline")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|e| execution(format!("cannot access {requested}: {e}")))?;
        if !metadata.is_file() {
            return Err(execution(format!(
                "path is not a regular file: {requested}"
            )));
        }
        const MAX_READ_BYTES: u64 = 100 * 1024 * 1024;
        if metadata.len() > MAX_READ_BYTES {
            return Err(execution(format!(
                "file is too large ({} bytes); maximum is {MAX_READ_BYTES} bytes",
                metadata.len()
            )));
        }
        let bytes = tokio::select! {
            biased;
            () = context.abort_signal.wait() => return Err(ToolError::Aborted),
            result = tokio::fs::read(&path) => result
                .map_err(|e| execution(format!("cannot read {requested}: {e}")))?,
        };
        if let Some(mime_type) = image_mime_type(&bytes) {
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let mut result = ToolResult::text(format!("Read image file [{mime_type}]"));
            result.content.push(ContentBlock::Image(ImageContent {
                data: encoded,
                mime_type: mime_type.to_string(),
            }));
            result.details =
                Some(json!({"path":requested,"mimeType":mime_type,"bytes":bytes.len()}));
            return Ok(result);
        }
        if bytes.contains(&0) {
            return Err(execution(
                "unsupported binary file; supported images are jpg, png, gif, and webp",
            ));
        }
        let text = String::from_utf8(bytes).map_err(|_| execution("file is not valid UTF-8"))?;
        let had_bom = text.starts_with('\u{feff}');
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines = normalized.lines().collect::<Vec<_>>();
        if lines.is_empty() && input.get("offset").is_some() {
            return Err(pi_tool_support::invalid(format!(
                "offset {offset} is beyond end of file (0 lines)"
            )));
        }
        if offset > lines.len().max(1) {
            return Err(pi_tool_support::invalid(format!(
                "offset {offset} is beyond end of file ({} lines)",
                lines.len()
            )));
        }
        let start = offset - 1;
        let target_end = (start + limit).min(lines.len());
        let line_num_width = target_end.max(1).to_string().len().max(5);
        let mut output = String::new();
        let mut end = start;
        let mut first_line_exceeds_limit = false;
        for (relative, line) in lines[start..target_end].iter().enumerate() {
            context
                .abort_signal
                .check()
                .map_err(|_| ToolError::Aborted)?;
            let line_index = start + relative;
            let rendered = if hashline {
                format!("{}:{}", hashline_tag(line_index, line, had_bom), line)
            } else {
                format!("{:>line_num_width$}→{}", line_index + 1, line)
            };
            if output.len().saturating_add(rendered.len() + 1) > MAX_OUTPUT_BYTES {
                first_line_exceeds_limit = output.is_empty();
                break;
            }
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&rendered);
            end = line_index + 1;
        }
        if first_line_exceeds_limit {
            output = format!(
                "[Line {offset} exceeds the 1MB limit. Use bash to read a byte-limited slice.]"
            );
        }
        let truncated = end < lines.len();
        if truncated {
            output.push_str(&format!(
                "\n\n[Showing lines {offset}-{end} of {}. Use offset={} to continue.]",
                lines.len(),
                end + 1
            ));
        }
        let mut result = ToolResult::text(output);
        result.details = Some(json!({
            "path":requested,"totalLines":lines.len(),"offset":offset,"endLine":end,
            "truncated":truncated,"firstLineExceedsLimit":first_line_exceeds_limit
        }));
        Ok(result)
    }
}

fn image_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_core::AbortHandle;

    #[tokio::test]
    async fn returns_supported_image_block() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"\x89PNG\r\n\x1a\nminimal";
        std::fs::write(dir.path().join("a.png"), bytes).unwrap();
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
    async fn numbers_lines_and_normalizes_crlf_and_bom() {
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
        assert_eq!(text, "    1→one\n    2→two");
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
        assert!(text.contains("exceeds the 1MB limit"));
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
