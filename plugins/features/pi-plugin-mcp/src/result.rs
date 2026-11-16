//! MCP result projection into model-visible content, with the original result retained in details.
//!
//! Matches Pi's text/image projection and 20 KiB text budget. Persisted output files intentionally
//! outlive a call and its generation so later read-tool calls can retrieve omitted text or blobs.

use std::io::{self, Write};
use std::path::PathBuf;

use base64::Engine as _;
use pi_core::{ContentBlock, ImageContent, TextContent, ToolResult};
use rmcp::model::{
    CallToolResult, ContentBlock as McpContentBlock, ReadResourceResult, ResourceContents,
};
use serde_json::{Value, json};

const OUTPUT_MAX_BYTES: usize = 20 * 1024;

/// MCP inputs are object schemas; some servers omit fields required by provider tool APIs.
pub(crate) fn normalize_input_schema(mut schema: Value) -> Value {
    if let Value::Object(object) = &mut schema {
        if object.get("type").is_none_or(Value::is_null) {
            object.insert("type".into(), json!("object"));
        }
        object.entry("properties").or_insert_with(|| json!({}));
    }
    schema
}

pub(crate) fn project_result(server: &str, tool: &str, result: CallToolResult) -> ToolResult {
    project_result_with_output(server, tool, result, false, save_output)
}

/// Resource links advertise the reader only when this call can actually reach it.
pub(crate) fn project_result_with_resources(
    server: &str,
    tool: &str,
    result: CallToolResult,
    readable_resources: bool,
) -> ToolResult {
    if readable_resources {
        project_result_with_output(server, tool, result, true, save_output)
    } else {
        project_result(server, tool, result)
    }
}

pub(crate) fn project_resource_listing(payload: Value) -> ToolResult {
    project_resource_listing_with_output(payload, save_output)
}

fn project_resource_listing_with_output(
    payload: Value,
    mut save: impl FnMut(&[u8], &str) -> io::Result<PathBuf>,
) -> ToolResult {
    ToolResult {
        content: limit_content(vec![text(payload.to_string())], &mut save),
        details: Some(payload),
        usage: None,
        added_tool_names: None,
        is_error: false,
        terminate: false,
    }
}

pub(crate) fn project_resource_read(
    server: &str,
    uri: &str,
    result: ReadResourceResult,
) -> ToolResult {
    project_resource_read_with_output(server, uri, result, save_output)
}

fn project_resource_read_with_output(
    server: &str,
    uri: &str,
    result: ReadResourceResult,
    mut save: impl FnMut(&[u8], &str) -> io::Result<PathBuf>,
) -> ToolResult {
    // Resource tools expose Pi's payload shape; transport metadata is not model data.
    let mut contents = json!(result.contents);
    if let Some(items) = contents.as_array_mut() {
        for item in items {
            if let Some(object) = item.as_object_mut() {
                object.remove("_meta");
            }
        }
    }
    let details = json!({"server": server, "uri": uri, "contents": contents});
    let multiple = result.contents.len() > 1;
    let mut content = Vec::new();
    for resource in result.contents {
        if multiple {
            let resource_uri = match &resource {
                ResourceContents::TextResourceContents { uri, .. }
                | ResourceContents::BlobResourceContents { uri, .. } => uri.as_str(),
                _ => uri,
            };
            content.push(text(format!("{resource_uri}:")));
        }
        content.push(project_resource_content(resource, &mut save));
    }
    if content.is_empty() {
        content.push(text(format!("Resource {uri} is empty.")));
    }
    ToolResult {
        content: limit_content(content, &mut save),
        details: Some(details),
        usage: None,
        added_tool_names: None,
        is_error: false,
        terminate: false,
    }
}

fn project_result_with_output(
    server: &str,
    tool: &str,
    result: CallToolResult,
    readable_resources: bool,
    mut save: impl FnMut(&[u8], &str) -> io::Result<PathBuf>,
) -> ToolResult {
    // Keep the existing Rust details contract, including _meta and structured content.
    let details = serde_json::to_value(&result).ok();
    let is_error = result.is_error.unwrap_or(false);
    let mut content = result
        .content
        .into_iter()
        .map(|block| project_content(server, block, readable_resources, &mut save))
        .collect::<Vec<_>>();
    if content.is_empty()
        && let Some(structured) = result.structured_content
    {
        content.push(text(
            serde_json::to_string_pretty(&structured).unwrap_or_else(|_| structured.to_string()),
        ));
    }
    if is_error && text_of(&content).is_empty() {
        content.push(text(format!("MCP tool {server}/{tool} returned an error")));
    }
    ToolResult {
        content: limit_content(content, &mut save),
        details,
        usage: None,
        added_tool_names: None,
        is_error,
        terminate: false,
    }
}

fn text(value: impl Into<String>) -> ContentBlock {
    ContentBlock::Text(TextContent::new(value))
}

fn image(data: String, mime_type: String) -> ContentBlock {
    ContentBlock::Image(ImageContent { data, mime_type })
}

fn project_content(
    server: &str,
    content: McpContentBlock,
    readable_resources: bool,
    save: &mut impl FnMut(&[u8], &str) -> io::Result<PathBuf>,
) -> ContentBlock {
    match content {
        McpContentBlock::Text(content) => text(content.text),
        McpContentBlock::Image(content) => image(content.data, content.mime_type),
        McpContentBlock::Audio(content) => text(format!("[audio {} omitted]", content.mime_type)),
        McpContentBlock::ResourceLink(resource) => {
            let mut details = Vec::new();
            if let Some(mime_type) = resource.mime_type {
                details.push(mime_type);
            }
            if let Some(size) = resource.size {
                details.push(format_size(size));
            }
            let details = if details.is_empty() {
                String::new()
            } else {
                format!(" ({})", details.join(", "))
            };
            let description = resource
                .description
                .map(|description| format!(": {description}"))
                .unwrap_or_default();
            let read = if readable_resources {
                format!(". Read it with read_mcp_resource (server \"{server}\")")
            } else {
                String::new()
            };
            text(format!(
                "[Resource {} \"{}\"{details}{description}{read}]",
                resource.uri,
                resource.title.unwrap_or(resource.name),
            ))
        }
        McpContentBlock::Resource(resource) => project_resource_content(resource.resource, save),
        _ => text("[unsupported MCP content omitted]"),
    }
}

fn project_resource_content(
    resource: ResourceContents,
    save: &mut impl FnMut(&[u8], &str) -> io::Result<PathBuf>,
) -> ContentBlock {
    match resource {
        ResourceContents::TextResourceContents { text: value, .. } => text(value),
        ResourceContents::BlobResourceContents {
            uri,
            mime_type,
            blob,
            ..
        } => {
            if let Some(mime_type) = mime_type.as_ref().filter(|mime| mime.starts_with("image/")) {
                return image(blob, mime_type.clone());
            }
            let Ok(data) = base64::engine::general_purpose::STANDARD.decode(&blob) else {
                return text(format!(
                    "[Binary resource {uri} could not be decoded: invalid base64]"
                ));
            };
            if is_text_mime_type(mime_type.as_deref()) {
                return text(String::from_utf8_lossy(&data).into_owned());
            }
            let kind = format!(
                "{}, {}",
                mime_type.as_deref().unwrap_or("unknown type"),
                format_size(data.len() as u64)
            );
            match save(&data, &resource_extension(&uri)) {
                Ok(path) => text(format!(
                    "[Binary resource {uri} ({kind}) saved to {}]",
                    path.display()
                )),
                Err(error) => text(format!(
                    "[Binary resource {uri} ({kind}) could not be saved: {error}]"
                )),
            }
        }
        _ => text("[unsupported MCP resource omitted]"),
    }
}

fn is_text_mime_type(mime_type: Option<&str>) -> bool {
    let Some(mime_type) = mime_type else {
        return false;
    };
    let mime_type = mime_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    mime_type.starts_with("text/")
        || mime_type == "application/json"
        || mime_type.ends_with("+json")
        || mime_type.ends_with("+xml")
}

fn resource_extension(uri: &str) -> String {
    let parsed = url::Url::parse(uri).ok();
    let path = parsed.as_ref().map_or(uri, url::Url::path);
    path.rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| {
            (1..=8).contains(&extension.len())
                && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .map_or_else(|| ".bin".into(), |extension| format!(".{extension}"))
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn save_output(data: &[u8], extension: &str) -> io::Result<PathBuf> {
    // NamedTempFile creates a private, exclusive file. keep() preserves it for later tool calls.
    let mut file = tempfile::Builder::new()
        .prefix("pi-mcp-")
        .suffix(extension)
        .tempfile()?;
    file.write_all(data)?;
    file.flush()?;
    file.keep()
        .map(|(_, path)| path)
        .map_err(|error| error.error)
}

fn text_of(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(content) => Some(content.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn limit_content(
    content: Vec<ContentBlock>,
    save: &mut impl FnMut(&[u8], &str) -> io::Result<PathBuf>,
) -> Vec<ContentBlock> {
    let combined = text_of(&content);
    if combined.len() <= OUTPUT_MAX_BYTES {
        return content;
    }
    let mut head_end = OUTPUT_MAX_BYTES / 2;
    while !combined.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = combined.len() - (OUTPUT_MAX_BYTES - head_end);
    while !combined.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let removed_chars = combined[head_end..tail_start].chars().count();
    let where_saved = match save(combined.as_bytes(), ".txt") {
        Ok(path) => format!(
            "\n\n[Full output: {} (read it with offset/limit)]",
            path.display()
        ),
        Err(error) => format!("\n\n[Could not save the full output: {error}]"),
    };
    let total_lines = combined.split('\n').count() - usize::from(combined.ends_with('\n'));
    let output = format!(
        "Warning: truncated output (original token count: {})\nTotal output lines: {total_lines}\n\n{}…{removed_chars} chars truncated…{}{where_saved}",
        combined.len().div_ceil(4),
        &combined[..head_end],
        &combined[tail_start..],
    );
    std::iter::once(text(output))
        .chain(
            content
                .into_iter()
                .filter(|block| matches!(block, ContentBlock::Image(_))),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(value: Value) -> CallToolResult {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn schema_normalization_preserves_constraints_and_local_references() {
        let schema = json!({
            "$defs": {"Entry": {"type": "string", "minLength": 1}},
            "properties": {"entry": {"$ref": "#/$defs/Entry"}},
            "required": ["entry"],
            "additionalProperties": false
        });
        let normalized = normalize_input_schema(schema.clone());
        assert_eq!(normalized["type"], "object");
        assert_eq!(normalized["$defs"], schema["$defs"]);
        assert_eq!(normalized["properties"], schema["properties"]);
        assert_eq!(normalized["required"], schema["required"]);
        assert_eq!(normalized["additionalProperties"], false);
        assert_eq!(
            normalize_input_schema(json!({"type": null})),
            json!({"type":"object","properties":{}})
        );
    }

    #[test]
    fn structured_only_results_reach_the_model_and_preserve_full_details() {
        for structured in [json!({"answer": 42}), json!([1, 2]), json!(null)] {
            let mut raw = CallToolResult::structured(structured.clone());
            raw.content.clear();
            let original = serde_json::to_value(&raw).unwrap();
            let projected = project_result("docs", "answer", raw);
            assert_eq!(
                serde_json::from_str::<Value>(&text_of(&projected.content)).unwrap(),
                structured
            );
            assert_eq!(projected.details, Some(original));
            assert!(!projected.is_error);
        }
    }

    #[test]
    fn resources_project_to_text_and_images_without_base64_text() {
        let raw = result(json!({"content": [
            {"type":"resource", "resource":{"uri":"file:///note", "text":"A note"}},
            {"type":"resource", "resource":{"uri":"file:///data", "mimeType":"Application/JSON; charset=utf-8", "blob":"eyJhIjoxfQ=="}},
            {"type":"resource", "resource":{"uri":"file:///image", "mimeType":"image/png", "blob":"YWJj"}},
            {"type":"audio", "data":"a".repeat(30_000), "mimeType":"audio/wav"},
            {"type":"resource_link", "uri":"file:///report.pdf", "name":"report", "title":"Report", "description":"Downloadable report", "mimeType":"application/pdf", "size":2048}
        ], "_meta":{"opaque":"preserved"}}));
        let original = serde_json::to_value(&raw).unwrap();
        let projected = project_result("docs", "read", raw);
        assert_eq!(projected.content[0], text("A note"));
        assert_eq!(projected.content[1], text("{\"a\":1}"));
        assert_eq!(
            projected.content[2],
            image("YWJj".into(), "image/png".into())
        );
        assert_eq!(projected.content[3], text("[audio audio/wav omitted]"));
        assert_eq!(
            projected.content[4],
            text(
                "[Resource file:///report.pdf \"Report\" (application/pdf, 2.0KB): Downloadable report]"
            )
        );
        assert_eq!(projected.details, Some(original));
    }

    #[test]
    fn resource_links_name_the_reader_only_when_reachable() {
        let raw = result(json!({"content": [{
            "type": "resource_link", "uri": "file:///report", "name": "report"
        }], "_meta": {"opaque": "preserved"}}));
        let original = serde_json::to_value(&raw).unwrap();
        let reachable = project_result_with_resources("docs", "search", raw.clone(), true);
        assert_eq!(
            reachable.content,
            vec![text(
                "[Resource file:///report \"report\". Read it with read_mcp_resource (server \"docs\")]"
            )]
        );
        assert_eq!(reachable.details, Some(original.clone()));
        let unavailable = project_result_with_resources("docs", "search", raw, false);
        assert_eq!(
            unavailable.content,
            vec![text("[Resource file:///report \"report\"]")]
        );
        assert_eq!(unavailable.details, Some(original));
    }

    #[test]
    fn resource_listing_keeps_full_payload_and_limits_only_model_text() {
        let payload = json!({
            "resources": [{"server": "docs", "uri": "file:///note", "name": "界".repeat(12_000)}],
            "errors": [{"server": "offline", "error": "unavailable"}]
        });
        let mut saved = Vec::new();
        let projected = project_resource_listing_with_output(payload.clone(), |data, extension| {
            assert_eq!(extension, ".txt");
            saved.extend_from_slice(data);
            Ok(PathBuf::from("/saved/resources.txt"))
        });
        assert_eq!(saved, payload.to_string().as_bytes());
        assert_eq!(projected.details, Some(payload));
        assert!(!projected.is_error);
        let visible = text_of(&projected.content);
        assert!(visible.contains("Warning: truncated output"));
        assert!(visible.contains("Full output: /saved/resources.txt"));
        assert!(!visible.contains('\u{fffd}'));

        let small = json!({"server":"docs","resources":[],"nextCursor":"opaque"});
        let projected = project_resource_listing(small.clone());
        assert_eq!(projected.content, vec![text(small.to_string())]);
        assert_eq!(projected.details, Some(small));
    }

    #[test]
    fn resource_reads_label_children_and_remove_transport_metadata() {
        let raw: ReadResourceResult = serde_json::from_value(json!({
            "_meta": {"transport": "private"},
            "contents": [
                {"uri":"file:///directory/note", "text":"A note", "_meta":{"private":true}},
                {"uri":"file:///directory/image", "mimeType":"image/png", "blob":"YWJj", "_meta":{"private":true}},
                {"uri":"file:///directory/data.bin", "blob":"AAECAw=="}
            ]
        })).unwrap();
        let mut saved = Vec::new();
        let projected = project_resource_read_with_output(
            "docs",
            "file:///directory",
            raw,
            |data, extension| {
                assert_eq!(extension, ".bin");
                saved.extend_from_slice(data);
                Ok(PathBuf::from("/saved/data.bin"))
            },
        );
        assert_eq!(saved, [0, 1, 2, 3]);
        assert_eq!(
            projected.content,
            vec![
                text("file:///directory/note:"),
                text("A note"),
                text("file:///directory/image:"),
                image("YWJj".into(), "image/png".into()),
                text("file:///directory/data.bin:"),
                text(
                    "[Binary resource file:///directory/data.bin (unknown type, 4B) saved to /saved/data.bin]"
                ),
            ]
        );
        assert_eq!(
            projected.details,
            Some(json!({
                "server":"docs", "uri":"file:///directory", "contents":[
                    {"uri":"file:///directory/note", "text":"A note"},
                    {"uri":"file:///directory/image", "mimeType":"image/png", "blob":"YWJj"},
                    {"uri":"file:///directory/data.bin", "blob":"AAECAw=="}
                ]
            }))
        );

        let empty =
            project_resource_read("docs", "file:///empty", ReadResourceResult::new(Vec::new()));
        assert_eq!(
            empty.content,
            vec![text("Resource file:///empty is empty.")]
        );
        assert_eq!(
            empty.details,
            Some(json!({"server":"docs","uri":"file:///empty","contents":[]}))
        );
        assert!(!empty.is_error);
    }

    #[test]
    fn binary_resources_are_saved_and_save_failures_stay_visible() {
        let raw = result(json!({"content":[{"type":"resource", "resource":{
            "uri":"https://example.test/data.pdf?download=1", "mimeType":"application/pdf", "blob":"AAECAw=="
        }}]}));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("resource.pdf");
        let projected =
            project_result_with_output("docs", "read", raw.clone(), false, |data, extension| {
                assert_eq!(extension, ".pdf");
                std::fs::write(&path, data)?;
                Ok(path.clone())
            });
        assert_eq!(std::fs::read(&path).unwrap(), [0, 1, 2, 3]);
        assert!(text_of(&projected.content).contains(&path.display().to_string()));
        let failed = project_result_with_output("docs", "read", raw, false, |_, _| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot save",
            ))
        });
        assert!(text_of(&failed.content).contains("could not be saved: cannot save"));
    }

    #[test]
    fn truncation_retains_utf8_ends_images_and_the_complete_file() {
        let beginning = format!("begin\n{}", "界🙂".repeat(3000));
        let ending = format!("{}\nend\n", "界🙂".repeat(3000));
        let combined = format!("{beginning}\n{ending}");
        let raw = CallToolResult::success(vec![
            McpContentBlock::text(beginning),
            McpContentBlock::image("YWJj", "image/png"),
            McpContentBlock::text(ending),
        ]);
        let original = serde_json::to_value(&raw).unwrap();
        let mut saved = Vec::new();
        let projected =
            project_result_with_output("docs", "large", raw, false, |data, extension| {
                assert_eq!(extension, ".txt");
                saved.extend_from_slice(data);
                Ok(PathBuf::from("/saved/full.txt"))
            });
        assert_eq!(saved, combined.as_bytes());
        let visible = text_of(&projected.content);
        assert!(visible.starts_with("Warning: truncated output"));
        assert!(visible.contains("Total output lines: 4"));
        assert!(visible.contains("\n\nbegin\n"));
        assert!(visible.contains("\nend\n\n\n[Full output:"));
        assert!(!visible.contains('\u{fffd}'));
        assert!(visible.len() < OUTPUT_MAX_BYTES + 400);
        assert_eq!(
            projected.content[1],
            image("YWJj".into(), "image/png".into())
        );
        assert_eq!(projected.details, Some(original));
    }

    #[test]
    fn empty_errors_have_visible_text_without_losing_image_or_error_state() {
        let projected = project_result(
            "docs",
            "fail",
            CallToolResult::error(vec![McpContentBlock::image("YWJj", "image/png")]),
        );
        assert!(projected.is_error);
        assert_eq!(
            projected.content[0],
            image("YWJj".into(), "image/png".into())
        );
        assert_eq!(
            projected.content[1],
            text("MCP tool docs/fail returned an error")
        );
    }

    #[test]
    fn saved_output_survives_return_and_is_private() {
        let path = save_output(b"complete output", ".txt").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"complete output");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_file(path).unwrap();
    }
}
